"""Calling a model without an agent: ``Gateway``.

    gw = Gateway(fallbacks=["gemini:gemini-2.5-flash"])
    reply = await gw.chat("openai:gpt-5-mini", "Summarize: …", instructions="Be brief.")
    print(reply.text)

Retries and fallback models work as for an agent, in the Rust engine.
"""

import asyncio
import json
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Dict, List, Optional, Sequence, Union

from . import _native
from .agent import Retry, _run_sync
from .models import Embedder, model_spec

Input = Union[str, Sequence[Dict[str, Any]]]


@dataclass(frozen=True)
class ChatReply:
    """A model's answer, from ``Gateway.chat`` or ``ChatStream.reply``.

    Fields:
        text: All text of the answer.
        message: The assistant message, in the transcript form of
            ``result.messages``.
        usage: Tokens used: ``input_tokens``, ``output_tokens``,
            ``reasoning_tokens``, ``cache_read_tokens``,
            ``cache_write_tokens``.
        stop_reason: Why the model stopped, for example ``"stop"`` or
            ``"length"``.
        model: ``"provider:model"`` that answered; a fallback's name when
            the first model failed.
        output: The parsed JSON answer when ``output`` was given, else
            ``None``.
    """

    text: str
    message: Dict[str, Any]
    usage: Dict[str, Any]
    stop_reason: Optional[str]
    model: str
    output: Any = None


@dataclass(frozen=True)
class Chunk:
    """A piece of a streamed answer.

    Fields:
        text: The new text.
        restart: A retry started the answer over: drop the text shown so
            far. Its ``text`` is empty.
    """

    text: str
    restart: bool = False


def _reply(raw: Dict[str, Any]) -> ChatReply:
    return ChatReply(
        text=raw.get("text", ""),
        message=raw.get("message") or {},
        usage=raw.get("usage") or {},
        stop_reason=raw.get("stop_reason"),
        model=raw.get("model", ""),
        output=raw.get("output"),
    )


class ChatStream:
    """A streamed answer, from ``Gateway.stream``.

    Iterate with ``async for chunk in stream``; then ``stream.reply`` holds
    the whole answer. Leaving the loop early cancels the call.

    Args:
        native: The engine's stream (internal).
    """

    def __init__(self, native: Any):
        self._native = native
        self._reply: Optional[ChatReply] = None

    def __aiter__(self) -> AsyncIterator[Chunk]:
        return self._chunks()

    async def _chunks(self) -> AsyncIterator[Chunk]:
        loop = asyncio.get_running_loop()
        done = False
        try:
            while True:
                data = await self._native.next(loop)
                if data is None:
                    break
                yield Chunk(text=data.get("text", ""), restart=data.get("type") == "restart")
            self._reply = _reply(await self._native.reply(loop))
            done = True
        finally:
            if not done:
                self._native.cancel()

    @property
    def reply(self) -> ChatReply:
        """The whole answer, once the loop has finished."""
        if self._reply is None:
            raise RuntimeError("the reply is ready after the `async for` loop has finished")
        return self._reply

    def cancel(self) -> None:
        """Cancels the call."""
        self._native.cancel()


class Gateway:
    """Model calls without an agent: ``chat``, ``stream`` and ``embed``.

    Each call names its model, and gets the same retries and fallback
    models as an agent, run by the engine. No tools, no threads. One
    ``Gateway`` can serve many calls at once.

    Args:
        fallbacks: Models tried in order when the model asked for fails with
            a transient error (rate limit, overload, server error, timeout,
            network) after its retries. Model objects or
            ``"provider:model"`` strings.
        fallback_on: More model error kinds that also move to the next
            fallback, for example ``"context_overflow"``.
        retry: How transient errors are retried. Default ``Retry()``: 3
            attempts, backoff from 0.5 s doubling up to 20 s, with jitter.
        max_retry_after: The longest ``Retry-After`` that is waited for, in
            seconds (default 60). Longer requests fail at once, or move to a
            fallback.

    Example:
        from criogen import FakeModel, Gateway

        gateway = Gateway(fallbacks=[FakeModel(["From the backup."], name="backup")])
        reply = gateway.chat_sync(FakeModel(["Hi there."]), "Hello", instructions="Be brief.")
        print(reply.text, reply.model)  # Hi there. fake:fake
    """

    def __init__(
        self,
        *,
        fallbacks: Sequence[Any] = (),
        fallback_on: Sequence[str] = (),
        retry: Optional[Retry] = None,
        max_retry_after: Optional[float] = None,
    ):
        config = {
            "fallbacks": [model_spec(m) for m in fallbacks],
            "fallback_on": list(fallback_on) or None,
            "retry": retry._spec() if retry is not None else None,
            "max_retry_after_secs": max_retry_after,
        }
        self._native = _native.NativeGateway(json.dumps(config))

    @staticmethod
    def _payload(
        model: Any,
        input: Input,
        instructions: Optional[str],
        temperature: Optional[float],
        max_tokens: Optional[int],
        output: Any,
    ) -> str:
        if hasattr(output, "model_json_schema"):
            output = output.model_json_schema()
        return json.dumps(
            {
                "model": model_spec(model),
                "input": input if isinstance(input, str) else list(input),
                "instructions": instructions,
                "temperature": temperature,
                "max_tokens": max_tokens,
                "output": output,
            }
        )

    async def chat(
        self,
        model: Any,
        input: Input,
        *,
        instructions: Optional[str] = None,
        temperature: Optional[float] = None,
        max_tokens: Optional[int] = None,
        output: Any = None,
    ) -> ChatReply:
        """Asks a model for one answer.

        Args:
            model: The model: a model object or a ``"provider:model"``
                string.
            input: The user's text, or a list of message dicts (``role`` and
                ``content``; ``content`` may be a string).
            instructions: A system prompt, sent before ``input``.
            temperature: The sampling temperature. ``None`` leaves the
                provider's default.
            max_tokens: The most tokens the answer may have. ``None`` leaves
                the provider's default.
            output: A JSON Schema (dict) or a Pydantic model class the answer
                must match; ``reply.output`` is then the parsed JSON (a dict).

        Returns:
            The ``ChatReply``.

        Raises:
            ModelError: The model and every fallback failed.

        Example:
            reply = await gateway.chat(
                "openai:gpt-5-mini",
                [{"role": "user", "content": "Name a colour."}],
                max_tokens=20,
            )
        """
        payload = self._payload(model, input, instructions, temperature, max_tokens, output)
        raw = await self._native.chat(asyncio.get_running_loop(), payload)
        return _reply(raw)

    def stream(
        self,
        model: Any,
        input: Input,
        *,
        instructions: Optional[str] = None,
        temperature: Optional[float] = None,
        max_tokens: Optional[int] = None,
        output: Any = None,
    ) -> ChatStream:
        """Asks a model for one answer and streams it as it is written.

        Takes the same arguments as ``chat``. Call it inside a running event
        loop.

        Args:
            model: The model: a model object or a ``"provider:model"``
                string.
            input: The user's text, or a list of message dicts.
            instructions: A system prompt, sent before ``input``.
            temperature: The sampling temperature.
            max_tokens: The most tokens the answer may have.
            output: A JSON Schema or a Pydantic model class the answer must
                match.

        Returns:
            A ``ChatStream`` of ``Chunk`` objects.

        Example:
            stream = gateway.stream("openai:gpt-5-mini", "Tell a short story.")
            async for chunk in stream:
                print(chunk.text, end="")
            print(stream.reply.usage)
        """
        payload = self._payload(model, input, instructions, temperature, max_tokens, output)
        return ChatStream(self._native.stream(payload))

    async def embed(self, embedder: Embedder, texts: Sequence[str]) -> List[List[float]]:
        """Turns texts into vectors.

        Args:
            embedder: The ``Embedder`` to use.
            texts: The texts.

        Returns:
            One vector (a list of floats) per text, in order.
        """
        payload = json.dumps({"embedder": embedder._spec, "texts": list(texts)})
        vectors: List[List[float]] = await self._native.embed(asyncio.get_running_loop(), payload)
        return vectors

    def chat_sync(self, model: Any, input: Input, **options: Any) -> ChatReply:
        """``chat`` for code without a running event loop.

        Args:
            model: The model: a model object or a ``"provider:model"``
                string.
            input: The user's text, or a list of message dicts.
            **options: ``instructions``, ``temperature``, ``max_tokens`` and
                ``output``, as for ``chat``.

        Returns:
            The ``ChatReply``.
        """
        result: ChatReply = _run_sync(self.chat(model, input, **options))
        return result

    def embed_sync(self, embedder: Embedder, texts: Sequence[str]) -> List[List[float]]:
        """``embed`` for code without a running event loop.

        Args:
            embedder: The ``Embedder`` to use.
            texts: The texts.

        Returns:
            One vector per text, in order.
        """
        result: List[List[float]] = _run_sync(self.embed(embedder, texts))
        return result

"""Calling a model without an agent (design 0018): ``Gateway``.

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
    """A model's answer."""

    #: All text of the answer.
    text: str
    #: The assistant message (the transcript format of ``result.messages``).
    message: Dict[str, Any]
    #: Tokens used.
    usage: Dict[str, Any]
    #: Why the model stopped, e.g. ``"stop"`` or ``"length"``.
    stop_reason: Optional[str]
    #: ``"provider:model"`` that answered (a fallback, if the first failed).
    model: str
    #: The parsed JSON answer, when ``output`` (a JSON Schema) was given.
    output: Any = None


@dataclass(frozen=True)
class Chunk:
    """A piece of a streamed answer. ``restart``: a retry started the answer
    over, so drop the text shown so far."""

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
    """A streamed answer: ``async for chunk in stream``; then ``stream.reply``
    holds the whole answer. Leaving the loop early cancels the call."""

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
    """Model calls without an agent: ``chat``, ``stream`` and ``embed``,
    with the agent's retries and fallback models (design 0018).

    ``fallbacks``: models tried in order when the one asked for fails with a
    transient error (after its retries); ``fallback_on``: other error kinds
    that also move on (e.g. ``"context_overflow"``); ``retry``: how transient
    errors are retried; ``max_retry_after``: the longest ``Retry-After`` that
    is waited for, in seconds."""

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
        """One answer. ``input``: text, or messages (``content`` may be a
        string). ``output``: a JSON Schema (or a Pydantic model) the answer
        must match; ``reply.output`` is then the parsed JSON."""
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
        """The answer as it is written: ``async for chunk in gw.stream(…)``."""
        payload = self._payload(model, input, instructions, temperature, max_tokens, output)
        return ChatStream(self._native.stream(payload))

    async def embed(self, embedder: Embedder, texts: Sequence[str]) -> List[List[float]]:
        """One vector per text, in order."""
        payload = json.dumps({"embedder": embedder._spec, "texts": list(texts)})
        vectors: List[List[float]] = await self._native.embed(asyncio.get_running_loop(), payload)
        return vectors

    def chat_sync(self, model: Any, input: Input, **options: Any) -> ChatReply:
        """``chat`` from code without an event loop."""
        result: ChatReply = _run_sync(self.chat(model, input, **options))
        return result

    def embed_sync(self, embedder: Embedder, texts: Sequence[str]) -> List[List[float]]:
        """``embed`` from code without an event loop."""
        result: List[List[float]] = _run_sync(self.embed(embedder, texts))
        return result

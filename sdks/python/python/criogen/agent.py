"""Agents and threads (design 0011 §2) over the Rust engine."""

import asyncio
import json
import time
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Dict, List, Mapping, Optional, Sequence, Union

from . import _native
from .models import model_spec
from .hooks import hook_adapter
from .redact import Redaction
from .usage import run_facts
from .storage import Memory, Storage
from .tools import Tool


@dataclass(frozen=True)
class ApprovalRequest:
    """A tool call waiting for a decision."""

    call_id: str
    tool: str
    args: Dict[str, Any]
    description: Optional[str] = None


class Decision:
    """A reviewer's answer to one call (see ``Thread.decide``)."""

    @staticmethod
    def approve() -> Dict[str, Any]:
        return {"type": "approve"}

    @staticmethod
    def reject(message: Optional[str] = None) -> Dict[str, Any]:
        return {"type": "reject", "message": message}

    @staticmethod
    def edit(args: Dict[str, Any], name: Optional[str] = None) -> Dict[str, Any]:
        return {"type": "edit", "args": args, "name": name}

    @staticmethod
    def respond(message: str) -> Dict[str, Any]:
        return {"type": "respond", "message": message}


@dataclass
class RunResult:
    """What a run produced."""

    text: Optional[str]
    output: Any
    messages: List[Dict[str, Any]]
    usage: Dict[str, Any]
    stop_reason: Optional[str]
    needs_approval: bool
    approvals: List[ApprovalRequest]
    model_calls: int
    tool_calls: int
    run_id: str
    raw: Dict[str, Any] = field(repr=False, default_factory=dict)
    #: Wall time of the call, in milliseconds.
    duration_ms: int = 0
    #: ``{"provider", "model"}`` that wrote the last answer (a fallback shows here).
    answered_by: Optional[Dict[str, str]] = None
    #: Each model this call used: ``{"provider", "model", "calls", "usage"}``.
    models: List[Dict[str, Any]] = field(default_factory=list)


@dataclass
class Reply(RunResult):
    """A thread call's result, saved as the thread's new head."""

    version: int = 0
    checkpoint_id: str = ""


def _approvals(raw: Dict[str, Any]) -> List[ApprovalRequest]:
    return [
        ApprovalRequest(r["call_id"], r["tool"], r.get("args") or {}, r.get("description"))
        for r in raw.get("approvals") or []
    ]


def _result(
    raw: Dict[str, Any], output_model: Any, cls: type = RunResult, started: Optional[float] = None, **extra: Any
) -> Any:
    output = raw.get("output")
    if output is not None and output_model is not None:
        output = output_model.model_validate(output)
    return cls(
        text=raw.get("text"),
        output=output,
        messages=raw.get("messages") or [],
        usage=raw.get("usage") or {},
        stop_reason=raw.get("stop_reason"),
        needs_approval=bool(raw.get("needs_approval")),
        approvals=_approvals(raw),
        model_calls=raw.get("model_calls", 0),
        tool_calls=raw.get("tool_calls", 0),
        run_id=raw.get("run_id", ""),
        raw=raw,
        **run_facts(raw.get("messages") or [], started),
        **extra,
    )


def _reply(raw: Dict[str, Any], output_model: Any, started: Optional[float] = None) -> Reply:
    return _result(
        raw,
        output_model,
        Reply,
        started=started,
        version=raw.get("version", 0),
        checkpoint_id=raw.get("checkpoint_id", ""),
    )


def _run_sync(coroutine: Any) -> Any:
    try:
        asyncio.get_running_loop()
    except RuntimeError:
        return asyncio.run(coroutine)
    coroutine.close()
    raise RuntimeError("the *_sync methods cannot run inside an event loop: await the async method")


@dataclass(frozen=True)
class Retry:
    """How often and how patiently to retry (design 0017 §2): attempts in
    total (including the first), and an exponential backoff in seconds."""

    max_attempts: int = 3
    initial_backoff: float = 0.5
    factor: float = 2.0
    max_backoff: float = 20.0
    jitter: bool = True

    def _spec(self) -> Dict[str, Any]:
        return {
            "max_attempts": self.max_attempts,
            "initial_backoff_secs": self.initial_backoff,
            "factor": self.factor,
            "max_backoff_secs": self.max_backoff,
            "jitter": self.jitter,
        }


class AgentTool:
    """An agent as a tool of another agent (``Agent.as_tool``): it takes
    ``{"input": text}`` and returns the sub-agent's answer."""

    def __init__(self, agent: "Agent", name: str, description: str):
        self.agent = agent
        self.name = name
        self.description = description

    def __repr__(self) -> str:
        return f"AgentTool({self.name!r})"


class Agent:
    """A model with tools, instructions and (optionally) approval, typed
    output, storage and memory. The loop runs in Rust.

    ``on_tool_exception``: what happens when a tool raises an exception it
    did not handle (``ToolError`` always goes to the model):
    ``"return_to_model"`` (default; the model sees the exception's type and
    message, the run goes on), ``"return_generic"`` (the model only learns
    that the tool failed; the text stays in the result's ``artifact``) or
    ``"fail"`` (the run fails with ``ToolExecutionError``, the exception as
    its ``__cause__``)."""

    #: The native agent (internal).
    _native: Any

    def __init__(
        self,
        model: Any,
        *,
        instructions: Optional[str] = None,
        tools: Sequence[Any] = (),
        approve: Sequence[str] = (),
        output: Any = None,
        limits: Optional[Dict[str, Any]] = None,
        storage: Union[None, str, Storage] = None,
        memory: Optional[Memory] = None,
        on_tool_exception: Optional[str] = None,
        fallbacks: Sequence[Any] = (),
        fallback_on: Sequence[str] = (),
        model_retry: Optional[Retry] = None,
        tool_retry: Optional[Retry] = None,
        max_retry_after: Optional[float] = None,
        telemetry: Any = None,
        hooks: Sequence[Any] = (),
        redact: Any = None,
    ):
        """``redact``: a function ``text -> text`` (or a coroutine) applied to
        every text before each model call (the input, replies, tool results),
        ahead of all hooks, e.g. to strip patient identifiers before anything
        reaches a provider. The redacted transcript is what a thread saves."""
        if redact is not None:
            hooks = [Redaction(redact), *hooks]
        from .mcp import McpServer, McpTool

        for t in tools:
            if not isinstance(t, (Tool, AgentTool, McpServer, McpTool)):
                raise TypeError(
                    f"{t!r} is not a tool: decorate it with @tool, use agent.as_tool(...), or pass an MCP server"
                )
        functions = [t for t in tools if isinstance(t, Tool)]
        agents = [t for t in tools if isinstance(t, AgentTool)]
        # MCP servers' tools (design 0022): a server means all its tools.
        servers: Dict[int, Any] = {}
        for t in tools:
            if isinstance(t, McpServer):
                servers[id(t)] = [t, None]
            elif isinstance(t, McpTool):
                entry = servers.setdefault(id(t.server), [t.server, []])
                if entry[1] is not None:
                    entry[1].append(t.name)
        mcp = [(server._native, names) for server, names in servers.values()]
        if isinstance(storage, str):
            storage = Storage(storage)
        self.storage = storage
        self._output_model = output if hasattr(output, "model_json_schema") else None
        schema = output.model_json_schema() if self._output_model is not None else output
        config = {
            "model": model_spec(model),
            "instructions": instructions,
            "tools": [t.spec() for t in functions],
            "approve": list(approve),
            "output": schema,
            "limits": {**(limits or {}), "on_tool_exception": on_tool_exception}
            if on_tool_exception is not None
            else limits,
            "memory": memory._spec if memory is not None else None,
            "fallbacks": [model_spec(m) for m in fallbacks],
            "fallback_on": list(fallback_on) or None,
            "model_retry": model_retry._spec() if model_retry is not None else None,
            "tool_retry": tool_retry._spec() if tool_retry is not None else None,
            "max_retry_after_secs": max_retry_after,
        }
        self._native = _native.NativeAgent(
            json.dumps(config),
            [t.adapter() for t in functions],
            storage._native if storage is not None else None,
            [(t.name, t.description, t.agent._native) for t in agents],
            telemetry._native if telemetry is not None else None,
            [hook_adapter(h) for h in hooks],
            mcp,
        )

    async def run(self, input: str, **state: Any) -> RunResult:
        """Runs once (no thread). Keyword arguments set state keys (e.g. the
        user a memory belongs to)."""
        started = time.monotonic()
        raw = await self._native.run(
            asyncio.get_running_loop(), json.dumps({"input": input, "state": state})
        )
        return _result(raw, self._output_model, started=started)

    def run_sync(self, input: str, **state: Any) -> RunResult:
        """:meth:`run` for scripts (not inside an event loop)."""
        return _run_sync(self.run(input, **state))

    def as_tool(self, name: str, description: str) -> AgentTool:
        """This agent as a tool of another agent: the tool takes ``input``
        (text), runs this agent, and returns its answer. How deep agents may
        call agents is bounded by the ``nesting`` limit (default 8)."""
        return AgentTool(self, name, description)

    def thread(self, thread_id: str, actor: Optional[str] = None) -> "Thread":
        """The conversation ``thread_id`` (created on first use). ``actor``
        names who acts, for the audit trail."""
        return Thread(self, thread_id, actor)


class Event:
    """One streamed event: ``type`` is ``text``, ``tool_call_started``,
    ``tool_call_finished``, ``message``, ``approval_needed`` or ``other``;
    the rest are attributes (``text``, ``tool``, ``call_id``, ``status``,
    ``requests``, … by type)."""

    type: str
    data: Dict[str, Any]

    def __init__(self, data: Dict[str, Any]):
        self.__dict__.update(data)
        self.data = data

    def __getattr__(self, name: str) -> Any:
        # Only reached for fields this type of event does not have: typed
        # code may read any field (mypy), and a missing one is an error.
        raise AttributeError(f"a {self.data.get('type')!r} event has no {name!r}")

    def __repr__(self) -> str:
        return f"Event({self.data!r})"


class Stream:
    """A streamed thread call: ``async for event in stream``, then
    ``await stream.reply()``. Leaving early cancels the run (it is still
    saved, and ``retry`` continues it)."""

    def __init__(self, native: Any, output_model: Any):
        self._native = native
        self._output_model = output_model
        self._done = False
        self._started = time.monotonic()

    def __aiter__(self) -> AsyncIterator[Event]:
        return self._events()

    async def _events(self) -> AsyncIterator[Event]:
        loop = asyncio.get_running_loop()
        try:
            while True:
                data = await self._native.next(loop)
                if data is None:
                    self._done = True
                    return
                yield Event(data)
        finally:
            if not self._done:
                self._native.cancel()

    async def reply(self) -> Reply:
        """Waits for the call; its saved reply."""
        raw = await self._native.finish(asyncio.get_running_loop())
        return _reply(raw, self._output_model, self._started)

    def cancel(self) -> None:
        """Cancels the run."""
        self._native.cancel()


class Thread:
    """One conversation, kept in the agent's storage."""

    def __init__(self, agent: Agent, thread_id: str, actor: Optional[str]):
        self.agent = agent
        self.id = thread_id
        self.actor = actor

    async def _op(self, op: str, **payload: Any) -> Any:
        payload.update(thread_id=self.id, actor=self.actor)
        return await self.agent._native.thread(
            asyncio.get_running_loop(), op, json.dumps(payload)
        )

    async def _reply(self, op: str, **payload: Any) -> Reply:
        started = time.monotonic()
        return _reply(await self._op(op, **payload), self.agent._output_model, started)

    async def send(self, input: str, **state: Any) -> Reply:
        """Runs one turn: the stored conversation plus ``input``."""
        return await self._reply("send", input=input, state=state)

    def send_sync(self, input: str, **state: Any) -> Reply:
        """:meth:`send` for scripts (not inside an event loop)."""
        return _run_sync(self.send(input, **state))

    async def approve(self) -> Reply:
        """Approves every pending call and continues."""
        return await self._reply("approve")

    async def reject(self, message: str = "rejected") -> Reply:
        """Rejects every pending call (the model sees ``message``)."""
        return await self._reply("reject", message=message)

    async def decide(self, decisions: Mapping[str, Dict[str, Any]]) -> Reply:
        """One ``Decision`` per pending call, by ``call_id``."""
        return await self._reply("decide", decisions=dict(decisions))

    async def retry(self) -> Reply:
        """Continues a failed or interrupted run."""
        return await self._reply("retry")

    async def history(self, limit: int = 20) -> List[Dict[str, Any]]:
        """Checkpoints, newest first."""
        return await self._op("history", limit=limit)

    async def messages(self) -> List[Dict[str, Any]]:
        """The stored conversation."""
        return await self._op("messages")

    async def rewind(self, checkpoint_id: str) -> Dict[str, Any]:
        """Makes an older checkpoint current again."""
        return await self._op("rewind", checkpoint_id=checkpoint_id)

    async def delete(self) -> bool:
        """Deletes the thread. ``True`` if it existed."""
        return await self._op("delete")

    def _stream(self, op: str, **payload: Any) -> Stream:
        payload.update(thread_id=self.id, actor=self.actor)
        native = self.agent._native.stream(asyncio.get_running_loop(), op, json.dumps(payload))
        return Stream(native, self.agent._output_model)

    def stream(self, input: str, **state: Any) -> Stream:
        """:meth:`send`, streaming its events (call inside a running loop)."""
        return self._stream("send", input=input, state=state)

    def stream_approve(self) -> Stream:
        return self._stream("approve")

    def stream_reject(self, message: str = "rejected") -> Stream:
        return self._stream("reject", message=message)

    def stream_retry(self) -> Stream:
        return self._stream("retry")

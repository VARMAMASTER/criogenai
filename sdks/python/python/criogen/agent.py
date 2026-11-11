"""Agents and threads: a model with tools, run by the Rust engine.

An ``Agent`` calls its model, runs the tools the model asks for, and repeats
until the model answers. ``agent.run`` is one call that remembers nothing;
``agent.thread(id)`` is a conversation saved in the agent's storage, with
approvals, retries, history and rewind.
"""

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
    """A tool call waiting for a reviewer's decision.

    A run stops before a tool listed in ``Agent(approve=[...])`` runs. The
    result then has ``needs_approval`` set and one ``ApprovalRequest`` per
    waiting call in ``approvals``. Answer with ``Thread.approve``,
    ``Thread.reject`` or ``Thread.decide``.

    Fields:
        call_id: The tool call's id. ``Thread.decide`` takes decisions keyed
            by it.
        tool: The name of the tool the model wants to call.
        args: The arguments the model wrote.
        description: Text for the reviewer, when the approval rule provides
            one; usually ``None``.
    """

    call_id: str
    tool: str
    args: Dict[str, Any]
    description: Optional[str] = None


class Decision:
    """A reviewer's answer to one pending tool call, for ``Thread.decide``.

    Each method returns a plain dict. ``Thread.decide`` needs a decision for
    every pending call; a missing or invalid one fails with
    ``resume_invalid`` (``ValidationError``) and the approval stays pending.

    Example:
        reply = await thread.send("Delete a.txt and b.txt")
        decisions = {
            request.call_id: Decision.approve()
            if request.args.get("path") == "a.txt"
            else Decision.reject("Only a.txt may be deleted.")
            for request in reply.approvals
        }
        reply = await thread.decide(decisions)
    """

    @staticmethod
    def approve() -> Dict[str, Any]:
        """Runs the call as the model wrote it."""
        return {"type": "approve"}

    @staticmethod
    def reject(message: Optional[str] = None) -> Dict[str, Any]:
        """Does not run the call; the model is told it was rejected.

        Args:
            message: Why, shown to the model. ``None`` sends a default text.
        """
        return {"type": "reject", "message": message}

    @staticmethod
    def edit(args: Dict[str, Any], name: Optional[str] = None) -> Dict[str, Any]:
        """Runs a changed call instead: other arguments, or another tool.

        The new arguments are checked against the tool's schema. The result
        tells the model what actually ran.

        Args:
            args: The arguments to use instead of the model's.
            name: Another tool to call instead. ``None`` keeps the tool the
                model chose.
        """
        return {"type": "edit", "args": args, "name": name}

    @staticmethod
    def respond(message: str) -> Dict[str, Any]:
        """Does not run the call; ``message`` becomes the call's result.

        Args:
            message: The answer the model receives as the tool result. Must
                not be empty.
        """
        return {"type": "respond", "message": message}


@dataclass
class RunResult:
    """What a run produced: the answer, the transcript and what it used.

    ``Agent.run`` returns one; thread calls return a ``Reply``, which adds
    the saved checkpoint.

    Fields:
        text: The final answer's text. ``None`` while the run waits for
            approval, and when structured output (``output=``) ended it.
        output: The structured output when the agent has ``output=``: a
            dict, or an instance of the Pydantic model you passed. When a hook
            ends the run, the output the hook gave. Otherwise ``None``.
        messages: The transcript as dicts (``role``, ``content`` blocks,
            ``id``, ``metadata``). For a thread, the whole stored
            conversation, earlier turns included.
        usage: Tokens used by the run's model calls: ``input_tokens``,
            ``output_tokens``, ``reasoning_tokens``, ``cache_read_tokens``,
            ``cache_write_tokens``.
        stop_reason: Why the run ended: ``"final_answer"``, ``"output"``
            (structured output accepted), ``"return_direct"``,
            ``"content_filter"``, ``"hook_end"`` or ``"limit"``. ``None``
            while the run waits for approval.
        needs_approval: ``True`` when the run stopped before tool calls that
            need a decision (see ``approvals``).
        approvals: The calls waiting for a decision; empty unless
            ``needs_approval``.
        model_calls: Model calls in the run. Retries of one call count once.
        tool_calls: Tool calls executed in the run.
        run_id: The run's id, also used in telemetry and audit records.
        raw: The engine's result as a dict.
        duration_ms: Wall time of the call, in milliseconds.
        answered_by: ``{"provider", "model"}`` of the model that wrote the
            last answer; shows when a fallback answered. ``None`` if no model
            answered in this call.
        models: Each model this call used, as ``{"provider", "model",
            "calls", "usage"}``. ``estimate_cost`` reads it.
    """

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
    duration_ms: int = 0
    answered_by: Optional[Dict[str, str]] = None
    models: List[Dict[str, Any]] = field(default_factory=list)


@dataclass
class Reply(RunResult):
    """A thread call's result, saved as the thread's new head.

    Has every field of ``RunResult``, plus the checkpoint it was saved as.

    Fields:
        version: The version number of the thread's new head checkpoint.
        checkpoint_id: The id of that checkpoint: the ``id`` that
            ``Thread.history`` lists and ``Thread.rewind`` takes.
    """

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
    """How often and how long to retry: attempts and an exponential backoff.

    Used by ``Agent(model_retry=..., tool_retry=...)`` and
    ``Gateway(retry=...)``. The wait grows from ``initial_backoff`` by
    ``factor`` per attempt, up to ``max_backoff``. When a provider sends
    ``Retry-After``, that wait is used instead (up to ``max_retry_after``),
    and a request that never reached the provider is retried at once.

    Fields:
        max_attempts: Attempts in total, including the first. ``1`` means no
            retry.
        initial_backoff: The first wait, in seconds.
        factor: How much each wait grows over the one before.
        max_backoff: The longest wait, in seconds.
        jitter: Randomize the waits, so that many clients do not retry at the
            same moment.

    Example:
        agent = Agent(
            "openai:gpt-5-mini",
            model_retry=Retry(max_attempts=5, initial_backoff=1.0),
            tool_retry=Retry(max_attempts=2),
        )
    """

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
    """An agent as a tool of another agent; made by ``Agent.as_tool``.

    The tool takes ``{"input": text}`` and returns the sub-agent's answer.
    Put it in the other agent's ``tools``.

    Args:
        agent: The agent that runs when the tool is called.
        name: The tool name the model sees.
        description: What the tool does, for the model.
    """

    def __init__(self, agent: "Agent", name: str, description: str):
        self.agent = agent
        self.name = name
        self.description = description

    def __repr__(self) -> str:
        return f"AgentTool({self.name!r})"


class Agent:
    """A model with tools and instructions; the agent loop runs in Rust.

    The agent calls the model, runs the tools it asks for (several at once
    when it asks for several), sends back the results and repeats until the
    model answers without calling a tool. Limits keep every run bounded.
    Use ``run`` for one-off calls and ``thread`` for conversations that are
    saved, can wait for approval and can be retried.

    Args:
        model: The model: a model object (``OpenAI("gpt-5-mini")``,
            ``FakeModel([...])``, ...) or a ``"provider:model"`` string such
            as ``"openai:gpt-5-mini"``. A string takes the provider's key
            from its usual environment variable.
        instructions: The system prompt, sent ahead of the conversation on
            every model call.
        tools: What the model may call: functions decorated with ``@tool``,
            other agents (``agent.as_tool(...)``), MCP servers or single MCP
            tools (``criogen.mcp``), and collection search tools
            (``Collection.as_tool``).
        approve: Names of tools that need a human decision before they run.
            The run stops with ``needs_approval`` set; continue it on a
            thread with ``approve``, ``reject`` or ``decide``.
        output: Structured output: a JSON Schema (dict) or a Pydantic model
            class. The run must end with JSON that matches it, which becomes
            ``result.output`` (an instance when you passed a Pydantic model).
        limits: Bounds on each run, as a dict: ``model_calls`` (default 25),
            ``tool_calls`` (default 200), ``tokens`` (input plus output tokens
            across the run; no limit by default) and ``tool_timeout_secs``
            (per tool call, default 60). Reaching a limit fails the run with
            ``LimitExceededError``.
        storage: Where threads and memory are kept: a ``Storage``, or a URL
            such as ``"sqlite://agent.db"`` or ``"postgres://..."``. ``None``
            keeps them in this process's memory.
        memory: Long-term memory (``Memory.by_state(...)``,
            ``Memory.shared(...)``): facts are recalled before each model
            call, and the model gets ``remember``, ``forget`` and
            ``search_memory`` tools. Kept in ``storage``.
        on_tool_exception: What happens when a tool raises an exception other
            than ``ToolError`` (a ``ToolError`` always goes to the model).
            ``"return_to_model"`` (default): the model sees the exception's
            type and message, and the run goes on. ``"return_generic"``: the
            model only learns that the tool failed; the text stays in the tool
            result's ``artifact``, which is never sent to a model. ``"fail"``:
            the run fails with ``ToolExecutionError``, the exception as its
            ``__cause__``.
        fallbacks: Models tried in order when the main model fails with a
            transient error (rate limit, overload, server error, timeout,
            network) after its retries. Same forms as ``model``.
        fallback_on: More model error kinds that also move to the next
            fallback, for example ``"context_overflow"`` (a model with a larger
            context window) or ``"content_filter"`` (another vendor).
        model_retry: How transient model errors are retried. Default
            ``Retry()``: 3 attempts, backoff from 0.5 s doubling up to 20 s,
            with jitter.
        tool_retry: How tool calls are retried after a ``ToolError`` with
            ``retryable=True`` or a timeout. Only tools marked ``read_only``
            or ``idempotent`` are retried. Default ``Retry()``.
        max_retry_after: The longest ``Retry-After`` the agent waits for, in
            seconds (default 60). When a provider asks for longer, the call
            fails at once (or moves to a fallback) instead of hanging.
        telemetry: A ``Telemetry`` exporter; every run of this agent is sent
            to it.
        hooks: Objects whose methods run around the loop (``on_start``,
            ``before_model``, ``after_model``, ``before_tool``,
            ``after_tool``, ``on_end``); see ``criogen.hooks``.
        redact: A function ``text -> text`` (or an async function) applied
            to every text in the transcript before each model call: the
            input, earlier replies and tool results. It runs before all hooks,
            so neither a hook nor a provider sees the original text, and the
            redacted transcript is what a thread saves. Use it to remove
            identifiers before anything leaves your systems.

    Raises:
        TypeError: An item of ``tools`` is not a tool, or an item of
            ``hooks`` has none of the hook methods.
        ValueError: ``model`` or a fallback is not a known
            ``"provider:model"`` string, or its API key is missing.
        ValidationError: The configuration is invalid, for example a limit
            of 0 or an unknown ``on_tool_exception``.
        ModelError: A model cannot be built, for example ``Bedrock`` without
            an API key.

    Example:
        from criogen import Agent, FakeModel, FakeReply, tool

        @tool
        def add(a: int, b: int) -> int:
            "Adds two numbers."
            return a + b

        model = FakeModel([FakeReply.tool_call("add", {"a": 2, "b": 3}), "2 + 3 = 5"])
        agent = Agent(model, instructions="Use the tools for arithmetic.", tools=[add])
        result = agent.run_sync("What is 2 + 3?")
        print(result.text)  # 2 + 3 = 5
    """

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
        """Builds the agent; the class describes the arguments."""
        if redact is not None:
            hooks = [Redaction(redact), *hooks]
        from .collections import CollectionTool
        from .mcp import McpServer, McpTool

        for t in tools:
            if not isinstance(t, (Tool, AgentTool, McpServer, McpTool, CollectionTool)):
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
        searches = [(t.collection._native, t.name, t.description) for t in tools if isinstance(t, CollectionTool)]
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
            searches,
        )

    async def run(self, input: str, **state: Any) -> RunResult:
        """Runs the agent once on ``input``, without a thread.

        Nothing is kept between calls: each run starts from the instructions
        and ``input``. A run that reaches a tool in ``approve`` ends with
        ``needs_approval`` set and cannot be continued; use a ``thread`` for
        approvals.

        Args:
            input: The user's message.
            **state: Values for the run state, which hooks (``ctx.state``)
                and memory read. For example ``user_id="u-1"`` selects the
                namespace of ``Memory.by_state("user_id", ...)``.

        Returns:
            The run's ``RunResult``.

        Raises:
            ModelError: The model failed after retries and fallbacks.
            LimitExceededError: A run limit was reached.
            ToolExecutionError: A tool raised and ``on_tool_exception`` is
                ``"fail"``.
            CancelledError: The run was cancelled.

        Example:
            result = await agent.run("Summarize the release notes.")
            print(result.text, result.usage["output_tokens"])
        """
        started = time.monotonic()
        raw = await self._native.run(
            asyncio.get_running_loop(), json.dumps({"input": input, "state": state})
        )
        return _result(raw, self._output_model, started=started)

    def run_sync(self, input: str, **state: Any) -> RunResult:
        """``run`` for code without a running event loop, such as scripts.

        Args:
            input: The user's message.
            **state: Values for the run state, as for ``run``.

        Returns:
            The run's ``RunResult``.

        Raises:
            RuntimeError: Called inside a running event loop; ``await
                agent.run(...)`` there instead.
        """
        return _run_sync(self.run(input, **state))

    def as_tool(self, name: str, description: str) -> AgentTool:
        """This agent as a tool of another agent.

        The tool takes ``input`` (text), runs this agent on it and returns
        its answer. Agents calling agents may nest up to 8 levels deep.

        Args:
            name: The tool name the other agent's model sees.
            description: What this agent does, so the model knows when to
                call it.

        Returns:
            An ``AgentTool`` to put in another agent's ``tools``.

        Example:
            researcher = Agent("openai:gpt-5-mini", instructions="Find facts.")
            writer = Agent(
                "openai:gpt-5-mini",
                tools=[researcher.as_tool("research", "Finds facts on a topic.")],
            )
        """
        return AgentTool(self, name, description)

    def thread(self, thread_id: str, actor: Optional[str] = None) -> "Thread":
        """The conversation ``thread_id`` in the agent's storage.

        The thread is created by its first call. ``Thread`` objects with the
        same id share the stored conversation.

        Args:
            thread_id: Your id for the conversation, for example a chat
                session id.
            actor: Who acts on the thread, recorded in the audit trail when
                the storage has ``audit=True``. ``None`` records no actor.

        Returns:
            A ``Thread``.

        Example:
            thread = agent.thread("session-42", actor="support-ui")
            reply = await thread.send("Hello")
            reply = await thread.send("What did I just say?")
        """
        return Thread(self, thread_id, actor)


class Event:
    """One event of a streamed thread call.

    The event's fields are also attributes (``event.text``). Reading a field
    this type of event does not have raises ``AttributeError``. By ``type``:

    - ``text``: ``text`` (a piece of the answer) and ``message_id``.
    - ``tool_call_started``: ``call_id``, ``tool`` and ``args``.
    - ``tool_call_finished``: ``call_id``, ``tool`` and ``status`` (for
      example ``"success"`` or ``"error"``).
    - ``message``: ``message``, a whole message added to the transcript.
    - ``approval_needed``: ``requests``, the calls waiting for a decision
      (as dicts).
    - ``other``: ``event``, any other engine event as debug text.

    Args:
        data: The event as a dict with a ``type`` key. ``Stream`` makes
            events; you do not build them.

    Fields:
        type: The kind of event, one of the six above.
        data: The whole event as a dict.
    """

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
    """A streamed thread call: its events as they happen, then its reply.

    Made by ``Thread.stream`` and the other ``stream_*`` methods. Iterate
    with ``async for event in stream``, then ``await stream.reply()``.
    Leaving the loop early cancels the run; what it did so far is saved,
    and ``Thread.retry`` continues it.

    Args:
        native: The engine's stream (internal).
        output_model: The agent's Pydantic output model, if any (internal).

    Example:
        stream = agent.thread("t-1").stream("Weather in Paris?")
        async for event in stream:
            if event.type == "text":
                print(event.text, end="")
        reply = await stream.reply()
    """

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
        """Waits for the call to end and returns its saved ``Reply``.

        Raises the errors ``Thread.send`` raises when the call failed.
        """
        raw = await self._native.finish(asyncio.get_running_loop())
        return _reply(raw, self._output_model, self._started)

    def cancel(self) -> None:
        """Cancels the run. What it did so far stays saved."""
        self._native.cancel()


class Thread:
    """One conversation, kept in the agent's storage; made by ``Agent.thread``.

    Each call is saved as a checkpoint. While a call waits for approval, the
    thread takes only ``approve``, ``reject`` or ``decide``, and ``send``
    raises ``ThreadSuspendedError``. A failed or interrupted call is saved
    too, and ``retry`` continues it. A call on a thread that another run
    holds raises ``ThreadBusyError`` or waits, as the storage's ``on_busy``
    says.

    Args:
        agent: The agent that runs the thread's calls.
        thread_id: The conversation's id.
        actor: Who acts, for the audit trail.

    Example:
        agent = Agent(model, tools=[delete_file], approve=["delete_file"])
        thread = agent.thread("t-1")
        reply = await thread.send("Delete a.txt")
        if reply.needs_approval:
            reply = await thread.approve()
        print(reply.text)
    """

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
        """Runs one turn: the stored conversation plus ``input``.

        Args:
            input: The user's message.
            **state: Values for the run state, as for ``Agent.run``.

        Returns:
            The ``Reply``, saved as the thread's new head.

        Raises:
            ThreadSuspendedError: The thread waits for an approval decision.
            ThreadBusyError: Another run holds the thread.
            ModelError: The model failed after retries and fallbacks. The
                failed run is saved; ``retry`` continues it.
        """
        return await self._reply("send", input=input, state=state)

    def send_sync(self, input: str, **state: Any) -> Reply:
        """``send`` for code without a running event loop.

        Args:
            input: The user's message.
            **state: Values for the run state, as for ``Agent.run``.

        Returns:
            The ``Reply``.

        Raises:
            RuntimeError: Called inside a running event loop; ``await
                thread.send(...)`` there instead.
        """
        return _run_sync(self.send(input, **state))

    async def approve(self) -> Reply:
        """Approves every pending call and continues the run.

        Returns:
            The ``Reply`` of the continued run.
        """
        return await self._reply("approve")

    async def reject(self, message: str = "rejected") -> Reply:
        """Rejects every pending call and continues the run.

        The calls do not run; the model is told they were rejected.

        Args:
            message: The reason the model sees for each call.

        Returns:
            The ``Reply`` of the continued run.
        """
        return await self._reply("reject", message=message)

    async def decide(self, decisions: Mapping[str, Dict[str, Any]]) -> Reply:
        """Continues the run with one decision per pending call.

        Args:
            decisions: ``{call_id: decision}``, with a ``Decision`` for every
                call in ``reply.approvals``.

        Returns:
            The ``Reply`` of the continued run.

        Raises:
            ValidationError: A call has no decision or a decision is invalid
                (``resume_invalid``); the approval stays pending.
        """
        return await self._reply("decide", decisions=dict(decisions))

    async def retry(self) -> Reply:
        """Continues a failed or interrupted run from where it stopped.

        Tool calls that finished and were saved do not run again.

        Returns:
            The ``Reply`` of the continued run.
        """
        return await self._reply("retry")

    async def history(self, limit: int = 20) -> List[Dict[str, Any]]:
        """The checkpoints of the thread's current branch, newest first.

        Args:
            limit: The most checkpoints to return.

        Returns:
            One dict per checkpoint: ``id``, ``parent_id``, ``run_id``,
            ``version``, ``step``, ``status`` (for example ``"completed"`` or
            ``"suspended"``), ``source`` and ``created_at_ms``.
        """
        return await self._op("history", limit=limit)

    async def messages(self) -> List[Dict[str, Any]]:
        """The stored conversation: every message, as dicts."""
        return await self._op("messages")

    async def rewind(self, checkpoint_id: str) -> Dict[str, Any]:
        """Makes an older checkpoint current again.

        Writes a new head with that checkpoint's state, and its pending
        approval or failure if it had one. Newer checkpoints stay stored but
        leave the history. The next call runs from there again.

        Args:
            checkpoint_id: The ``id`` of a checkpoint from ``history``.

        Returns:
            The new head checkpoint, in the form of a ``history`` entry.
        """
        return await self._op("rewind", checkpoint_id=checkpoint_id)

    async def delete(self) -> bool:
        """Deletes the thread and its checkpoints.

        Returns:
            ``True`` if the thread existed.
        """
        return await self._op("delete")

    def _stream(self, op: str, **payload: Any) -> Stream:
        payload.update(thread_id=self.id, actor=self.actor)
        native = self.agent._native.stream(asyncio.get_running_loop(), op, json.dumps(payload))
        return Stream(native, self.agent._output_model)

    def stream(self, input: str, **state: Any) -> Stream:
        """``send``, streaming its events. Call it inside a running event loop.

        Args:
            input: The user's message.
            **state: Values for the run state, as for ``Agent.run``.

        Returns:
            A ``Stream`` of ``Event`` objects; ``await stream.reply()`` gives
            the ``Reply``.
        """
        return self._stream("send", input=input, state=state)

    def stream_approve(self) -> Stream:
        """``approve``, streaming the continued run's events."""
        return self._stream("approve")

    def stream_reject(self, message: str = "rejected") -> Stream:
        """``reject``, streaming the continued run's events.

        Args:
            message: The reason the model sees for each call.
        """
        return self._stream("reject", message=message)

    def stream_retry(self) -> Stream:
        """``retry``, streaming the continued run's events."""
        return self._stream("retry")

"""Workflows: a graph of Python functions and agents, run by the Rust engine.

You declare nodes and how they connect; the engine schedules them in steps
(nodes that are ready together run concurrently), merges their updates into
one state dict, checks the graph before the first run, and pauses a run for
a person and continues it later.

    from criogen import END, Workflow

    wf = Workflow()
    wf.add_node("draft", lambda state: {"text": f"About {state['topic']}"})
    wf.start("draft").edge("draft", END)
    result = wf.compile().run_sync({"topic": "solar panels"})
"""

import asyncio
import inspect
import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Mapping, Optional, Sequence, Union

from . import _native
from .agent import Agent, _run_sync
from .storage import Storage

END = "__end__"
"""The end of a workflow: use it as the target of an edge, a route or ``goto``."""


@dataclass(frozen=True)
class NodeContext:
    """What a node can know about its execution.

    A node function that takes two parameters, ``(state, ctx)``, gets one.

    Example:
        def review(state, ctx):
            if ctx.resume is None:
                return suspend({"question": "Approve this text?"})
            return {"approved": ctx.resume == "yes"}
    """

    run_id: str
    """The run's id (the same for every node of one run)."""
    step: int
    """The step this node runs in, counted from 0."""
    node: str
    """This node's name."""
    attempt: int
    """Which attempt this is, from 1."""
    resume: Any = None
    """The value given to ``resume`` for this node after it suspended, else ``None``."""


class _Suspend:
    def __init__(self, payload: Any, update: Optional[Dict[str, Any]]):
        self.payload = payload
        self.update = update


class _Goto:
    def __init__(self, targets: Sequence[str], update: Optional[Dict[str, Any]]):
        self.targets = list(targets)
        self.update = update


def suspend(payload: Any, update: Optional[Dict[str, Any]] = None) -> Any:
    """Return this from a node to pause the run until a person answers.

    The run ends with ``suspended=True`` and an :class:`Interrupt` for this
    node. Continue it with ``CompiledWorkflow.resume(snapshot, {node:
    value})`` or ``WorkflowThread.resume({node: value})``: the node runs
    again, with ``ctx.resume == value``.

    Args:
        payload: What the person is asked (any JSON value); it comes back
            as ``Interrupt.payload``.
        update: State updates to apply now, before the pause.

    Returns:
        A marker for the engine; return it from the node as is.

    Example:
        from criogen import END, Workflow, suspend

        def approve(state, ctx):
            if ctx.resume is None:
                return suspend({"question": "Send this message?", "text": state["text"]})
            return {"approved": ctx.resume == "yes"}

        wf = Workflow()
        wf.add_node("approve", approve)
        wf.start("approve").edge("approve", END)
        app = wf.compile()
        paused = await app.run({"text": "Hello"})
        done = await app.resume(paused.snapshot, {"approve": "yes"})
    """
    return _Suspend(payload, update)


def goto(targets: Union[str, Sequence[str]], update: Optional[Dict[str, Any]] = None) -> Any:
    """Return this from a node to choose which nodes run next.

    The node must be declared with :meth:`Workflow.goto`, and every target
    must be among the targets declared there.

    Args:
        targets: A node name (or ``END``), or several names to run next in
            parallel.
        update: State updates to apply with the choice.

    Returns:
        A marker for the engine; return it from the node as is.

    Example:
        from criogen import END, Workflow, goto

        def pick(state):
            return goto("big" if state["n"] > 10 else "small", {"checked": True})

        wf = Workflow()
        wf.add_node("pick", pick)
        wf.add_node("big", lambda s: {"size": "big"})
        wf.add_node("small", lambda s: {"size": "small"})
        wf.goto("pick", ["big", "small"])
        wf.start("pick").edge("big", END).edge("small", END)
        wf.compile().run_sync({"n": 3}).state["size"]   # "small"
    """
    return _Goto([targets] if isinstance(targets, str) else targets, update)


@dataclass(frozen=True)
class Interrupt:
    """A paused node and what it asked for."""

    node: str
    """The node that called :func:`suspend`; use its name as the key when resuming."""
    payload: Any
    """What it asked: the ``payload`` given to :func:`suspend`."""


@dataclass
class WorkflowResult:
    """What a workflow run produced."""

    state: Dict[str, Any]
    """The state the run ended with (or paused at)."""
    steps: int
    """How many steps ran."""
    suspended: bool
    """``True`` if nodes wait for a person; see ``interrupts``."""
    interrupts: List[Interrupt]
    """The waiting nodes and what they asked (empty unless ``suspended``)."""
    run_id: str
    """The run's id."""
    snapshot: Optional[Dict[str, Any]] = field(repr=False, default=None)
    """The paused run, to pass to :meth:`CompiledWorkflow.resume` (runs without a thread). ``None`` when the run finished."""
    checkpoint_id: str = ""
    """Thread runs only: the checkpoint this call wrote (``""`` otherwise)."""
    version: int = 0
    """Thread runs only: the thread's version after this call (``0`` otherwise)."""


def _result(raw: Dict[str, Any]) -> WorkflowResult:
    return WorkflowResult(
        state=raw.get("state") or {},
        steps=raw.get("steps", 0),
        suspended=bool(raw.get("suspended")),
        interrupts=[Interrupt(i["node"], i.get("payload")) for i in raw.get("interrupts") or []],
        run_id=raw.get("run_id", ""),
        snapshot=raw.get("snapshot"),
        checkpoint_id=raw.get("checkpoint_id", ""),
        version=raw.get("version", 0),
    )


def _wants_ctx(func: Callable[..., Any]) -> bool:
    return len(inspect.signature(func).parameters) >= 2


def _node_adapter(func: Callable[..., Any]) -> Any:
    """``(state_json, ctx_json)`` → an envelope (a coroutine for async
    functions)."""
    with_ctx = _wants_ctx(func)

    def call_args(state_json: str, ctx_json: str) -> List[Any]:
        state = json.loads(state_json)
        return [state, NodeContext(**json.loads(ctx_json))] if with_ctx else [state]

    def envelope(value: Any) -> str:
        if isinstance(value, _Suspend):
            return json.dumps({"suspend": value.payload, "update": value.update}, default=str)
        if isinstance(value, _Goto):
            return json.dumps({"goto": value.targets, "update": value.update}, default=str)
        if value is not None and not isinstance(value, Mapping):
            raise TypeError(f"a node returns a dict (the update), suspend(...) or goto(...), not {value!r}")
        return json.dumps({"update": dict(value) if value else None}, default=str)

    if inspect.iscoroutinefunction(func):

        async def call_async(state_json: str, ctx_json: str) -> str:
            return envelope(await func(*call_args(state_json, ctx_json)))

        return call_async, True

    def call_sync(state_json: str, ctx_json: str) -> str:
        return envelope(func(*call_args(state_json, ctx_json)))

    return call_sync, False


def _router_adapter(func: Callable[[Dict[str, Any]], Any]) -> Any:
    if inspect.iscoroutinefunction(func):
        raise TypeError("a router is a plain (not async) function of the state: it only chooses a label")

    def call(state_json: str, _: str) -> str:
        label = func(json.loads(state_json))
        if not isinstance(label, str):
            raise TypeError(f"a router returns a label (a string), not {label!r}")
        return json.dumps({"label": label})

    return call


class Workflow:
    """A graph of nodes (Python functions or agents), checked by :meth:`compile` and run by the Rust engine.

    A node gets the state (a dict) and returns an update (a dict of keys to
    change, or ``None``), :func:`suspend` or :func:`goto`. After a node,
    the next nodes come from its edges (:meth:`edge`), a router
    (:meth:`route`), a join (:meth:`join`) or its own choice
    (:meth:`goto`). Nodes that are ready together run in the same step,
    concurrently, and their updates are merged into the state. State
    values must be JSON.

    The builder methods return the workflow, so calls can be chained.

    Args:
        reducers: How writes to a state key combine, by key.
            ``"replace"`` (the default for every key): a write replaces the
            value, and two writes to the key in one step fail with
            ``update_conflict``. ``"append"``: the value is a list and each
            write is a list appended to it. ``"upsert_by_id"``: a list of
            objects with a string ``id``; a new id is appended, an existing
            one replaced in place, ``{"id": ..., "$remove": True}``
            removes it. For both list reducers, ``{"$replace": [...]}``
            replaces the whole list.

    Example:
        from criogen import END, Workflow

        wf = Workflow(reducers={"notes": "append"})

        @wf.node
        def research(state):
            return {"notes": [f"facts about {state['topic']}"]}

        @wf.node
        def write(state):
            return {"text": "; ".join(state["notes"])}

        wf.start("research").edge("research", "write").edge("write", END)
        result = wf.compile().run_sync({"topic": "solar panels"})
        result.state["text"]   # "facts about solar panels"
    """

    def __init__(self, reducers: Optional[Mapping[str, str]] = None):
        self._nodes: Dict[str, Any] = {}
        self._entry: Optional[str] = None
        self._edges: List[List[str]] = []
        self._routes: List[Any] = []
        self._joins: List[Any] = []
        self._gotos: List[Any] = []
        self._reducers = dict(reducers or {})

    def node(self, func: Optional[Callable[..., Any]] = None, *, name: Optional[str] = None) -> Any:
        """Decorator that adds a function as a node: ``@wf.node`` or ``@wf.node(name="...")``.

        The function is returned unchanged.

        Args:
            func: The node function (given when used as ``@wf.node``).
            name: The node's name. Default: the function's name.

        Example:
            @wf.node
            def summarize(state):
                return {"summary": state["text"][:100]}
        """

        def add(f: Callable[..., Any]) -> Callable[..., Any]:
            self.add_node(name or f.__name__, f)
            return f

        return add(func) if func is not None else add

    def add_node(self, name: str, node: Union[Callable[..., Any], Agent]) -> "Workflow":
        """Adds a node: a function or an ``Agent``.

        A function takes ``(state)`` or ``(state, ctx)`` (a
        :class:`NodeContext`), sync or async, and returns an update dict,
        ``None``, :func:`suspend` or :func:`goto`. An exception it raises
        fails the run (``node_failed``, with the exception as
        ``__cause__``).

        An ``Agent`` reads the conversation from the ``messages`` key (a
        list of messages, or a string taken as one user message), runs, and
        writes the whole transcript back to ``messages``. If it waits for
        approval, the workflow suspends with ``{"kind": "agent_approval",
        "requests": [...]}``.

        Args:
            name: The node's name, unique in the workflow.
            node: The function or agent.

        Raises:
            ValueError: A node with this name exists.
            TypeError: ``node`` is not a function or an ``Agent``.
        """
        if name in self._nodes:
            raise ValueError(f"node {name!r} already exists")
        if not isinstance(node, Agent) and not callable(node):
            raise TypeError(f"node {name!r}: not a function or an Agent")
        self._nodes[name] = node
        return self

    def start(self, name: str) -> "Workflow":
        """Sets the node every run starts with.

        Args:
            name: The entry node's name.
        """
        self._entry = name
        return self

    def edge(self, source: str, target: str) -> "Workflow":
        """After ``source`` finishes, run ``target``.

        Several edges from one node run their targets in parallel.

        Args:
            source: The node the edge leaves from.
            target: The node to run next, or ``END`` to finish.
        """
        self._edges.append([source, target])
        return self

    def route(self, source: str, router: Callable[[Dict[str, Any]], str], routes: Mapping[str, str]) -> "Workflow":
        """After ``source``, a router function picks the next node from the state.

        Args:
            source: The node the route leaves from.
            router: A plain (not async) function ``state -> label`` that
                returns one of the labels in ``routes``.
            routes: Label to node name (or ``END``).

        Example:
            wf.route("classify", lambda s: s["kind"], {"refund": "refunds", "other": "general"})
        """
        self._routes.append((source, router, dict(routes)))
        return self

    def join(self, sources: Sequence[str], target: str) -> "Workflow":
        """Runs ``target`` once, after every node in ``sources`` has finished.

        Args:
            sources: The nodes to wait for (usually branches that run in
                parallel).
            target: The node to run after all of them.

        Example:
            wf.edge("begin", "a").edge("begin", "b").join(["a", "b"], "merge")
        """
        self._joins.append([list(sources), target])
        return self

    def goto(self, source: str, targets: Sequence[str]) -> "Workflow":
        """Declares that ``source`` chooses its next nodes itself, by returning :func:`goto`.

        The graph is checked with these targets, so a node can only jump
        where it said it might.

        Args:
            source: The node that returns ``goto(...)``.
            targets: Every node (or ``END``) it may choose.
        """
        self._gotos.append([source, list(targets)])
        return self

    def compile(self, storage: Union[None, str, Storage] = None) -> "CompiledWorkflow":
        """Checks the graph in Rust and returns a workflow ready to run.

        Every problem is reported at once (missing start node, edges to
        unknown nodes, unknown reducers, and so on).

        Args:
            storage: Where :meth:`CompiledWorkflow.thread` keeps threads: a
                :class:`~criogen.Storage` or a storage URL. Default:
                process memory. Not needed for ``run`` and ``resume``.

        Returns:
            The compiled workflow.

        Raises:
            ValidationError: The graph is invalid (``invalid_workflow``);
                ``details["issues"]`` lists every problem.
        """
        if isinstance(storage, str):
            storage = Storage(storage)
        config = {
            "entry": self._entry,
            "edges": self._edges,
            "routes": [{"from": source, "routes": routes} for source, _, routes in self._routes],
            "joins": self._joins,
            "gotos": self._gotos,
            "reducers": self._reducers,
        }
        functions = [
            (name, *_node_adapter(node)) for name, node in self._nodes.items() if not isinstance(node, Agent)
        ]
        agents = [(name, node._native) for name, node in self._nodes.items() if isinstance(node, Agent)]
        routers = [(source, _router_adapter(router)) for source, router, _ in self._routes]
        native = _native.NativeWorkflow(
            json.dumps(config),
            functions,
            routers,
            agents,
            storage._native if storage is not None else None,
        )
        return CompiledWorkflow(native, storage)


def _options(max_steps: Optional[int] = None, deadline: Optional[float] = None) -> Dict[str, Any]:
    options: Dict[str, Any] = {}
    if max_steps is not None:
        options["max_steps"] = max_steps
    if deadline is not None:
        options["deadline_ms"] = int(deadline * 1000)
    return options


class CompiledWorkflow:
    """A checked workflow, ready to run; made by :meth:`Workflow.compile`.

    Use :meth:`run` and :meth:`resume` for one-off runs (a paused run is
    kept by you, as a snapshot), or :meth:`thread` for durable runs kept in
    storage.

    Args:
        native: The engine's workflow. Internal: use
            :meth:`Workflow.compile`.
        storage: The storage given to ``compile``, or ``None``.
    """

    def __init__(self, native: Any, storage: Optional[Storage]):
        self._native = native
        self.storage = storage

    async def run(
        self, input: Optional[Mapping[str, Any]] = None, *, max_steps: Optional[int] = None, deadline: Optional[float] = None
    ) -> WorkflowResult:
        """Runs once from the start node, with ``input`` as the state.

        Args:
            input: The starting state (a dict of JSON values). Default
                empty.
            max_steps: The most steps the run may take. Default 64; past it
                the run fails with ``step_limit_exceeded``.
            deadline: The longest the run may take, in seconds. Default: no
                limit; past it the run fails with ``deadline_exceeded``.

        Returns:
            The final (or paused) state and how the run ended.

        Raises:
            LimitExceededError: ``max_steps`` or ``deadline`` was reached.
            CriogenError: A node raised (``node_failed``; ``__cause__`` is
                the exception, ``snapshot`` the failed run).
        """
        payload = {"input": dict(input or {}), **_options(max_steps, deadline)}
        return _result(await self._native.run(asyncio.get_running_loop(), json.dumps(payload)))

    def run_sync(self, input: Optional[Mapping[str, Any]] = None, **options: Any) -> WorkflowResult:
        """:meth:`run` for scripts (not inside an event loop).

        Args:
            input: The starting state.
            **options: ``max_steps`` and ``deadline``, as for :meth:`run`.
        """
        return _run_sync(self.run(input, **options))

    async def resume(self, snapshot: Dict[str, Any], values: Mapping[str, Any]) -> WorkflowResult:
        """Continues a paused run (without a thread) from its snapshot.

        Args:
            snapshot: ``WorkflowResult.snapshot`` of the paused run.
            values: One answer per paused node, by node name; each node
                runs again with ``ctx.resume`` set to its value.

        Returns:
            The result of the continued run.

        Raises:
            ValidationError: The snapshot cannot be read or does not fit
                this workflow, or the values do not match the paused nodes
                (``resume_invalid``, ``workflow_mismatch``).
        """
        payload = {"snapshot": snapshot, "values": dict(values)}
        return _result(await self._native.resume(asyncio.get_running_loop(), json.dumps(payload)))

    def thread(self, thread_id: str, actor: Optional[str] = None) -> "WorkflowThread":
        """A durable run history in the workflow's storage, created on first use.

        Args:
            thread_id: The thread's id, e.g. one per request or case. Do not
                share ids between different workflows: another workflow's
                thread is refused (``workflow_mismatch``).
            actor: Who acts through this handle (a user id), written to the
                audit trail when the storage has ``audit=True``.

        Returns:
            A handle; nothing is read or written until you call it.

        Example:
            app = wf.compile("sqlite://workflows.db")
            thread = app.thread("request-1", actor="reviewer-7")
            first = await thread.run({"text": "Hello"})
            if first.suspended:
                done = await thread.resume({"approve": "yes"})
        """
        return WorkflowThread(self, thread_id, actor)


class WorkflowThread:
    """One durable run history of a workflow, kept in storage.

    Each :meth:`run` starts from the state the last one ended with, plus
    the input. A run that suspends waits in storage for :meth:`resume`,
    from this process or another; a failed or interrupted run waits for
    :meth:`retry`. Only one call uses a thread at a time (see the
    storage's ``on_busy``).

    Args:
        app: The compiled workflow. Internal: use
            :meth:`CompiledWorkflow.thread`.
        thread_id: The thread's id.
        actor: Who acts through this handle, for the audit trail.
    """

    def __init__(self, app: CompiledWorkflow, thread_id: str, actor: Optional[str]):
        self.app = app
        self.id = thread_id
        self.actor = actor

    async def _op(self, op: str, **payload: Any) -> Any:
        payload.update(thread_id=self.id, actor=self.actor)
        return await self.app._native.thread(asyncio.get_running_loop(), op, json.dumps(payload))

    async def run(self, input: Optional[Mapping[str, Any]] = None, **options: Any) -> WorkflowResult:
        """Runs from the start node, on the stored state plus ``input``.

        Args:
            input: State values to set before the run; a key here replaces
                the stored one.
            **options: ``max_steps`` (default 64) and ``deadline`` (seconds),
                as for :meth:`CompiledWorkflow.run`.

        Returns:
            The result, with the ``checkpoint_id`` written and the thread's
            new ``version``.

        Raises:
            ThreadSuspendedError: Nodes wait for :meth:`resume`
                (``thread_suspended``).
            CriogenError: The last run did not finish
                (``thread_interrupted``); call :meth:`retry`.
            ThreadBusyError: Another call holds the thread.
        """
        return _result(await self._op("run", input=dict(input or {}), **_options(**options)))

    async def resume(self, values: Mapping[str, Any]) -> WorkflowResult:
        """Answers the paused nodes and continues the run.

        Args:
            values: One value per paused node, by node name; each node runs
                again with ``ctx.resume`` set to its value.

        Raises:
            ValidationError: The values do not match the paused nodes
                (``resume_invalid``); nothing runs and the thread keeps
                waiting.
        """
        return _result(await self._op("resume", values=dict(values)))

    async def retry(self) -> WorkflowResult:
        """Continues a failed or interrupted run from where it stopped.

        Nodes that finished (and were saved) do not run again.
        """
        return _result(await self._op("retry"))

    async def state(self) -> Dict[str, Any]:
        """The thread's current state (empty for a new thread)."""
        return await self._op("state")

    async def interrupts(self) -> List[Interrupt]:
        """What the paused nodes asked for (empty unless the thread is suspended)."""
        return [Interrupt(i["node"], i.get("payload")) for i in await self._op("interrupts")]

    async def history(self, limit: int = 20) -> List[Dict[str, Any]]:
        """The thread's checkpoints, newest first.

        Args:
            limit: The most checkpoints to return. Default 20.

        Returns:
            One dict per checkpoint: ``id``, ``parent_id``, ``version``,
            ``source``, ``status``, ``run_id``, ``step`` and
            ``created_at_ms`` (Unix milliseconds). Pass an ``id`` to
            :meth:`rewind`.
        """
        return await self._op("history", limit=limit)

    async def rewind(self, checkpoint_id: str) -> Dict[str, Any]:
        """Makes an older checkpoint current again.

        The checkpoint is copied as the new head (the history is kept), so
        the next call continues from that point.

        Args:
            checkpoint_id: An ``id`` from :meth:`history`.

        Returns:
            The new head checkpoint, shaped like a :meth:`history` entry.

        Raises:
            StorageError: The thread has no such checkpoint
                (``checkpoint_not_found``).
        """
        return await self._op("rewind", checkpoint_id=checkpoint_id)

    async def delete(self) -> bool:
        """Deletes the thread and all its checkpoints.

        Returns:
            ``True`` if the thread existed.
        """
        return await self._op("delete")

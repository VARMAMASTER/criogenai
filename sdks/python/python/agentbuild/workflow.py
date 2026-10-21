"""Workflows (design 0015): a graph of Python functions and agents, run
by the Rust engine (scheduling, joins, reducers, validation, pauses)."""

import asyncio
import inspect
import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Mapping, Optional, Sequence, Union

from . import _native
from .agent import Agent, _run_sync
from .storage import Storage

#: The end of a workflow, as an edge or route target.
END = "__end__"


@dataclass(frozen=True)
class NodeContext:
    """What a node can know about its execution."""

    run_id: str
    step: int
    node: str
    attempt: int
    #: The value given to ``resume`` for this node, else ``None``.
    resume: Any = None


class _Suspend:
    def __init__(self, payload: Any, update: Optional[Dict[str, Any]]):
        self.payload = payload
        self.update = update


class _Goto:
    def __init__(self, targets: Sequence[str], update: Optional[Dict[str, Any]]):
        self.targets = list(targets)
        self.update = update


def suspend(payload: Any, update: Optional[Dict[str, Any]] = None) -> Any:
    """Return this from a node to pause the run for a person: ``payload``
    says what is asked; ``resume({node: value})`` continues it, and the
    node runs again with ``ctx.resume == value``."""
    return _Suspend(payload, update)


def goto(targets: Union[str, Sequence[str]], update: Optional[Dict[str, Any]] = None) -> Any:
    """Return this from a node to choose the next nodes (declared with
    ``Workflow.goto``)."""
    return _Goto([targets] if isinstance(targets, str) else targets, update)


@dataclass(frozen=True)
class Interrupt:
    """A paused node and what it asked for."""

    node: str
    payload: Any


@dataclass
class WorkflowResult:
    """What a run produced."""

    state: Dict[str, Any]
    steps: int
    suspended: bool
    interrupts: List[Interrupt]
    run_id: str
    #: Pass to ``CompiledWorkflow.resume`` (runs without storage).
    snapshot: Optional[Dict[str, Any]] = field(repr=False, default=None)
    #: Threads only: the checkpoint written, and the thread's version.
    checkpoint_id: str = ""
    version: int = 0


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
    """A graph of nodes (Python functions or agents), compiled by
    ``compile`` and run by the Rust engine.

    ``reducers``: how writes to a key combine when several nodes write it
    (``"replace"``, the default; ``"append"``; ``"upsert_by_id"`` for
    message lists)."""

    def __init__(self, reducers: Optional[Mapping[str, str]] = None):
        self._nodes: Dict[str, Any] = {}
        self._entry: Optional[str] = None
        self._edges: List[List[str]] = []
        self._routes: List[Any] = []
        self._joins: List[Any] = []
        self._gotos: List[Any] = []
        self._reducers = dict(reducers or {})

    def node(self, func: Optional[Callable[..., Any]] = None, *, name: Optional[str] = None) -> Any:
        """Decorator: ``@wf.node`` adds a function as a node named after it."""

        def add(f: Callable[..., Any]) -> Callable[..., Any]:
            self.add_node(name or f.__name__, f)
            return f

        return add(func) if func is not None else add

    def add_node(self, name: str, node: Union[Callable[..., Any], Agent]) -> "Workflow":
        """Adds a function ``(state)`` / ``(state, ctx)`` or an ``Agent`` (it
        reads and writes the ``messages`` key)."""
        if name in self._nodes:
            raise ValueError(f"node {name!r} already exists")
        if not isinstance(node, Agent) and not callable(node):
            raise TypeError(f"node {name!r}: not a function or an Agent")
        self._nodes[name] = node
        return self

    def start(self, name: str) -> "Workflow":
        """The node a run starts with."""
        self._entry = name
        return self

    def edge(self, source: str, target: str) -> "Workflow":
        """After ``source``, run ``target`` (or ``END``)."""
        self._edges.append([source, target])
        return self

    def route(self, source: str, router: Callable[[Dict[str, Any]], str], routes: Mapping[str, str]) -> "Workflow":
        """After ``source``, ``router(state)`` returns a label; run
        ``routes[label]``."""
        self._routes.append((source, router, dict(routes)))
        return self

    def join(self, sources: Sequence[str], target: str) -> "Workflow":
        """Run ``target`` once every node in ``sources`` has finished."""
        self._joins.append([list(sources), target])
        return self

    def goto(self, source: str, targets: Sequence[str]) -> "Workflow":
        """``source`` may choose among ``targets`` by returning ``goto(...)``."""
        self._gotos.append([source, list(targets)])
        return self

    def compile(self, storage: Union[None, str, Storage] = None) -> "CompiledWorkflow":
        """Checks the graph in Rust (every problem at once:
        ``ValidationError`` with ``details["issues"]``)."""
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
    """A checked workflow, ready to run."""

    def __init__(self, native: Any, storage: Optional[Storage]):
        self._native = native
        self.storage = storage

    async def run(
        self, input: Optional[Mapping[str, Any]] = None, *, max_steps: Optional[int] = None, deadline: Optional[float] = None
    ) -> WorkflowResult:
        """Runs from the start node with ``input`` as the state."""
        payload = {"input": dict(input or {}), **_options(max_steps, deadline)}
        return _result(await self._native.run(asyncio.get_running_loop(), json.dumps(payload)))

    def run_sync(self, input: Optional[Mapping[str, Any]] = None, **options: Any) -> WorkflowResult:
        """:meth:`run` for scripts (not inside an event loop)."""
        return _run_sync(self.run(input, **options))

    async def resume(self, snapshot: Dict[str, Any], values: Mapping[str, Any]) -> WorkflowResult:
        """Continues a paused run (without storage) from its ``snapshot``:
        one value per paused node."""
        payload = {"snapshot": snapshot, "values": dict(values)}
        return _result(await self._native.resume(asyncio.get_running_loop(), json.dumps(payload)))

    def thread(self, thread_id: str, actor: Optional[str] = None) -> "WorkflowThread":
        """A durable run history in the workflow's storage (created on
        first use)."""
        return WorkflowThread(self, thread_id, actor)


class WorkflowThread:
    """One durable conversation with a workflow: each ``run`` continues
    from the stored state; a paused run waits in storage for ``resume``."""

    def __init__(self, app: CompiledWorkflow, thread_id: str, actor: Optional[str]):
        self.app = app
        self.id = thread_id
        self.actor = actor

    async def _op(self, op: str, **payload: Any) -> Any:
        payload.update(thread_id=self.id, actor=self.actor)
        return await self.app._native.thread(asyncio.get_running_loop(), op, json.dumps(payload))

    async def run(self, input: Optional[Mapping[str, Any]] = None, **options: Any) -> WorkflowResult:
        """Runs from the start node on the stored state plus ``input``."""
        return _result(await self._op("run", input=dict(input or {}), **_options(**options)))

    async def resume(self, values: Mapping[str, Any]) -> WorkflowResult:
        """Answers the paused nodes: one value per node name."""
        return _result(await self._op("resume", values=dict(values)))

    async def retry(self) -> WorkflowResult:
        """Continues a failed or interrupted run; finished nodes do not run again."""
        return _result(await self._op("retry"))

    async def state(self) -> Dict[str, Any]:
        return await self._op("state")

    async def interrupts(self) -> List[Interrupt]:
        return [Interrupt(i["node"], i.get("payload")) for i in await self._op("interrupts")]

    async def history(self, limit: int = 20) -> List[Dict[str, Any]]:
        return await self._op("history", limit=limit)

    async def rewind(self, checkpoint_id: str) -> Dict[str, Any]:
        return await self._op("rewind", checkpoint_id=checkpoint_id)

    async def delete(self) -> bool:
        return await self._op("delete")

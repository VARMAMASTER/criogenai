"""Design patterns for agentic systems, ready to use (design 0019).

    from agentbuild import patterns

    flow = patterns.sequential(["researcher", "writer"], model="openai:gpt-5-mini")
    print((await flow.run("Write about solar panels")).output)

Steps can be an ``Agent``, a ``Specialist`` or its registered name (a string),
a plain function ``text -> text`` (sync or async), or ``(name, step)``. The
model for specialists comes from ``model=`` or the ``AGENTBUILD_MODEL``
environment variable. Every loop has a cap, so nothing runs forever.

Workflow patterns return a :class:`Pattern`: ``run`` / ``run_sync`` for the
answer, ``compile(storage)`` for durable threads, approvals and an audit trail,
``workflow`` to extend the graph. ``react``, ``coordinator`` and ``hierarchical``
return an ``Agent``, so they nest as steps of other patterns.
"""

import asyncio
import inspect
import os
import re
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Dict, List, Mapping, Optional, Sequence, Tuple, Union

from . import specialists as _specialists
from .agent import Agent, _run_sync
from .storage import Storage
from .workflow import END, CompiledWorkflow, Workflow, WorkflowResult, goto, suspend

__all__ = [
    "Pattern", "PatternResult", "sequential", "parallel", "loop", "review_and_critique",
    "iterative_refinement", "react", "coordinator", "hierarchical", "swarm", "human_review", "custom", "quick",
]

#: No loop runs more often than this, whatever the caller asks for.
CEILING = 50
DEFAULT_MODEL_ENV = "AGENTBUILD_MODEL"

Step = Union[Agent, "_specialists.Specialist", str, Callable[[str], Any], Tuple[str, Any]]


# ------------------------------------------------------------------- results


@dataclass
class PatternResult:
    """What a pattern produced. ``print(result)`` shows the output;
    ``result.explain()`` shows each step."""

    output: Optional[str]
    trace: List[Dict[str, Any]]
    state: Dict[str, Any]
    #: Why it ended: "done", "condition", "approved", "max_iterations", "max_rounds", "final",
    #: "max_handoffs", "unknown_handoff", "rejected", or "suspended".
    stop_reason: str
    suspended: bool = False
    #: What a person is asked, when ``suspended``.
    waiting_for: Any = None
    raw: Optional[WorkflowResult] = field(default=None, repr=False)

    def __str__(self) -> str:
        return self.output or ""

    def explain(self) -> str:
        """Each step on its own line: which step, what it returned (shortened)."""
        lines = [f"ended: {self.stop_reason}"]
        for i, entry in enumerate(self.trace, 1):
            text = " ".join(str(entry.get("output", "")).split())
            lines.append(f"{i}. {entry.get('step')}: {text[:140]}{'…' if len(text) > 140 else ''}")
        return "\n".join(lines)


# --------------------------------------------------------------------- steps


def _default_model(model: Any) -> Any:
    if model is not None:
        return model
    named = os.environ.get(DEFAULT_MODEL_ENV)
    if named:
        return named
    raise ValueError(
        "this pattern needs a model for its specialists: pass model='openai:gpt-5-mini' (or any model), "
        f"or set the {DEFAULT_MODEL_ENV} environment variable"
    )


def _cap(value: int, name: str) -> int:
    if not isinstance(value, int) or value < 1:
        raise ValueError(f"{name} must be a whole number of at least 1")
    return min(value, CEILING)


def _resolve(step: Any, model: Any, index: int) -> Tuple[str, Callable[[str], Awaitable[str]]]:
    """A step as ``(name, async text -> text)``."""
    name: Optional[str] = None
    if isinstance(step, tuple) and len(step) == 2 and isinstance(step[0], str):
        name, step = step
    if isinstance(step, str):
        step = _specialists.get(step)
    if isinstance(step, _specialists.Specialist):
        name = name or step.name
        step = step.build(_default_model(model))
    if isinstance(step, Agent):
        agent = step

        async def run_agent(text: str) -> str:
            result = await agent.run(text)
            return result.text if result.text is not None else str(result.output)

        return name or f"step{index + 1}", run_agent
    if callable(step):
        function = step

        async def run_function(text: str) -> str:
            value = function(text)
            if inspect.isawaitable(value):
                value = await value
            return "" if value is None else str(value)

        return name or str(getattr(function, "__name__", f"step{index + 1}")), run_function
    raise TypeError(f"step {index + 1}: {step!r} is not an Agent, a Specialist (or its name), or a function text -> text")


_RESERVED = {"begin", "finish", "check", "combine", "generate", "review", "dispatch", "route_first"}


def _unique(names: List[str]) -> List[str]:
    """Node names: distinct, and never one of the pattern's own (begin, finish ...)."""
    seen: Dict[str, int] = {}
    out = []
    for n in names:
        n = f"{n}-step" if n in _RESERVED else n
        seen[n] = seen.get(n, 0) + 1
        out.append(n if seen[n] == 1 else f"{n}-{seen[n]}")
    return out


def _entry(step: str, output: str) -> List[Dict[str, Any]]:
    return [{"step": step, "output": output}]


# ------------------------------------------------------------------- pattern


class Pattern:
    """A workflow pattern: run it, compile it for durability, or extend it.

    ``workflow`` is the plain :class:`~agentbuild.Workflow` (add nodes and
    edges to extend it before the first run)."""

    def __init__(self, name: str, workflow: Workflow, budget: int):
        self.name = name
        self.workflow = workflow
        self._budget = budget
        self._compiled: Optional[CompiledWorkflow] = None

    def compile(self, storage: Union[None, str, Storage] = None) -> CompiledWorkflow:
        """The checked, durable form: ``compile(storage).thread(id).run({"input": text})``,
        with approvals, resume and an audit trail."""
        return self.workflow.compile(storage)

    async def run(self, text: str, **options: Any) -> PatternResult:
        """Runs once and returns the result. A ``human_review`` pattern returns
        ``suspended=True``; continue with :meth:`resume`."""
        if self._compiled is None:
            self._compiled = self.compile()
        options.setdefault("max_steps", self._budget)
        raw = await self._compiled.run({"input": text}, **options)
        return self._result(raw)

    def run_sync(self, text: str, **options: Any) -> PatternResult:
        """:meth:`run` for scripts (not inside an event loop)."""
        return _run_sync(self.run(text, **options))

    async def resume(self, paused: PatternResult, decision: Any) -> PatternResult:
        """Continues a ``human_review`` run: ``"approve"``, ``{"decision": "edit", "text": ...}``
        or ``{"decision": "reject"}``."""
        if self._compiled is None or paused.raw is None or not paused.suspended:
            raise ValueError("resume needs the result of a run that is waiting for a person")
        snapshot = paused.raw.snapshot
        if snapshot is None:
            raise ValueError("this run has no snapshot to resume from")
        node = paused.raw.interrupts[0].node
        return self._result(await self._compiled.resume(snapshot, {node: decision}))

    @staticmethod
    def _result(raw: WorkflowResult) -> PatternResult:
        state = raw.state
        reason = "suspended" if raw.suspended else state.get("stop_reason", "done")
        return PatternResult(
            output=state.get("output"),
            trace=list(state.get("trace") or []),
            state=state,
            stop_reason=reason,
            suspended=raw.suspended,
            waiting_for=raw.interrupts[0].payload if raw.interrupts else None,
            raw=raw,
        )

    def __repr__(self) -> str:
        return f"Pattern({self.name!r})"


def custom(workflow: Workflow, *, name: str = "custom", max_steps: int = 64) -> Pattern:
    """Custom logic: any ``Workflow``, with the same ``run`` / ``compile`` as the ready-made patterns.
    Its start state is ``{"input": text}``; the answer is ``state["output"]``."""
    return Pattern(name, workflow, max_steps)


def _begin(state: Dict[str, Any]) -> Dict[str, Any]:
    return {"current": state["input"], "trace": [], "round": 0}


def _finish(reason: str = "done") -> Callable[[Dict[str, Any]], Dict[str, Any]]:
    def finish(state: Dict[str, Any]) -> Dict[str, Any]:
        return {"output": state.get("current"), "stop_reason": state.get("stop_reason", reason)}

    return finish


def _step_node(name: str, run: Callable[[str], Awaitable[str]]):
    async def node(state: Dict[str, Any]) -> Dict[str, Any]:
        out = await run(state["current"])
        return {"current": out, "trace": _entry(name, out)}

    return node


# ----------------------------------------------------------- workflow patterns


def sequential(steps: Sequence[Step], *, model: Any = None) -> Pattern:
    """Runs the steps in a fixed order; each one's output is the next one's input.

        flow = patterns.sequential(["researcher", "writer"], model="openai:gpt-5-mini")
    """
    if not steps:
        raise ValueError("sequential needs at least one step")
    resolved = [_resolve(s, model, i) for i, s in enumerate(steps)]
    names = _unique([n for n, _ in resolved])
    wf = Workflow(reducers={"trace": "append"})
    wf.add_node("begin", _begin)
    previous = "begin"
    for name, (_, run) in zip(names, resolved):
        wf.add_node(name, _step_node(name, run))
        wf.edge(previous, name)
        previous = name
    wf.add_node("finish", _finish())
    wf.edge(previous, "finish").edge("finish", END).start("begin")
    return Pattern("sequential", wf, 3 * len(steps) + 8)


def parallel(
    branches: Union[Sequence[Step], Mapping[str, Step]],
    synthesizer: Optional[Step] = None,
    *,
    model: Any = None,
) -> Pattern:
    """Runs the branches at the same time on the same input, then combines them:
    with a ``synthesizer`` step (it receives the input and every branch's answer),
    or as a titled list.

        flow = patterns.parallel({"sentiment": s, "keywords": k}, synthesizer="summarizer", model=m)
    """
    items = list(branches.items()) if isinstance(branches, Mapping) else list(branches)
    if not items:
        raise ValueError("parallel needs at least one branch")
    resolved = [_resolve(s, model, i) for i, s in enumerate(items)]
    names = _unique([n for n, _ in resolved])
    synth = _resolve(synthesizer, model, 0) if synthesizer is not None else None
    wf = Workflow(reducers={"trace": "append", "results": "append"})
    wf.add_node("begin", _begin)

    def branch_node(index: int, name: str, run: Callable[[str], Awaitable[str]]):
        async def node(state: Dict[str, Any]) -> Dict[str, Any]:
            out = await run(state["current"])
            return {"results": [{"i": index, "name": name, "output": out}], "trace": _entry(name, out)}

        return node

    for i, (name, (_, run)) in enumerate(zip(names, resolved)):
        wf.add_node(name, branch_node(i, name, run))
        wf.edge("begin", name)

    async def combine(state: Dict[str, Any]) -> Dict[str, Any]:
        found = sorted(state["results"], key=lambda r: r["i"])
        listing = "\n\n".join(f"## {r['name']}\n{r['output']}" for r in found)
        if synth is None:
            return {"current": listing, "stop_reason": "done"}
        out = await synth[1](f"Original request:\n{state['input']}\n\nResults from {len(found)} specialists:\n\n{listing}")
        return {"current": out, "trace": _entry(synth[0] + " (synthesis)", out)}

    wf.add_node("combine", combine)
    wf.join(names, "combine")
    wf.add_node("finish", _finish())
    wf.edge("combine", "finish").edge("finish", END).start("begin")
    return Pattern("parallel", wf, 2 * len(items) + 10)


def loop(
    steps: Sequence[Step],
    until: Callable[[str, int], bool],
    *,
    max_iterations: int = 5,
    model: Any = None,
) -> Pattern:
    """Repeats the steps (a chain, each output feeding the next) until
    ``until(text, round)`` is true or ``max_iterations`` is reached (at most 50).
    The result's ``stop_reason`` says which.

        flow = patterns.loop([improve], until=lambda text, n: "DONE" in text, max_iterations=4)
    """
    if not steps:
        raise ValueError("loop needs at least one step")
    cap = _cap(max_iterations, "max_iterations")
    resolved = [_resolve(s, model, i) for i, s in enumerate(steps)]
    names = _unique([n for n, _ in resolved])
    wf = Workflow(reducers={"trace": "append"})
    wf.add_node("begin", _begin)
    previous = "begin"
    for name, (_, run) in zip(names, resolved):
        wf.add_node(name, _step_node(name, run))
        wf.edge(previous, name)
        previous = name

    def check(state: Dict[str, Any]) -> Dict[str, Any]:
        round_no = state["round"] + 1
        if until(state["current"], round_no):
            return {"round": round_no, "again": False, "stop_reason": "condition"}
        if round_no >= cap:
            return {"round": round_no, "again": False, "stop_reason": "max_iterations"}
        return {"round": round_no, "again": True}

    wf.add_node("check", check)
    wf.edge(previous, "check")
    wf.route("check", lambda s: "again" if s["again"] else "done", {"again": names[0], "done": "finish"})
    wf.add_node("finish", _finish())
    wf.edge("finish", END).start("begin")
    return Pattern("loop", wf, cap * (len(steps) + 2) + 8)


def iterative_refinement(
    refine: Step,
    score: Callable[[str], float],
    *,
    threshold: float = 0.8,
    max_iterations: int = 5,
    model: Any = None,
) -> Pattern:
    """Improves a result step by step: ``refine`` gets the current text and returns a better one;
    stops when ``score(text) >= threshold`` or after ``max_iterations``."""
    return loop([refine], lambda text, _round: score(text) >= threshold, max_iterations=max_iterations, model=model)


def review_and_critique(
    generator: Step,
    critic: Step,
    *,
    max_rounds: int = 3,
    approved: Optional[Callable[[str], bool]] = None,
    model: Any = None,
) -> Pattern:
    """A generator writes, a critic reviews. The critic approves (its reply starts with
    ``APPROVED``, or ``approved(reply)`` is true) or returns feedback; the generator revises.
    Ends on approval or after ``max_rounds`` (at most 50) with the last draft.
    ``stop_reason`` is ``"approved"`` or ``"max_rounds"``.

        flow = patterns.review_and_critique("writer", "critic", model=m)
    """
    cap = _cap(max_rounds, "max_rounds")
    gen_name, gen = _resolve(generator, model, 0)
    crit_name, crit = _resolve(critic, model, 1)
    if gen_name == crit_name:
        crit_name += "-critic"
    is_ok = approved or (lambda reply: reply.strip().upper().startswith("APPROVED"))
    wf = Workflow(reducers={"trace": "append"})
    wf.add_node("begin", _begin)

    async def generate(state: Dict[str, Any]) -> Dict[str, Any]:
        feedback = state.get("feedback")
        prompt = state["input"] if not feedback else (
            f"{state['input']}\n\nYour previous draft:\n{state['current']}\n\nReviewer feedback to address:\n{feedback}\n\nWrite the improved version."
        )
        out = await gen(prompt)
        return {"current": out, "trace": _entry(f"{gen_name} (draft {state['round'] + 1})", out)}

    async def review(state: Dict[str, Any]) -> Dict[str, Any]:
        reply = await crit(f"Request:\n{state['input']}\n\nDraft:\n{state['current']}")
        round_no = state["round"] + 1
        update: Dict[str, Any] = {"round": round_no, "trace": _entry(crit_name, reply)}
        if is_ok(reply):
            return {**update, "again": False, "stop_reason": "approved", "feedback": None}
        if round_no >= cap:
            return {**update, "again": False, "stop_reason": "max_rounds", "feedback": reply}
        return {**update, "again": True, "feedback": reply}

    wf.add_node("generate", generate)
    wf.add_node("review", review)
    wf.edge("begin", "generate").edge("generate", "review")
    wf.route("review", lambda s: "again" if s["again"] else "done", {"again": "generate", "done": "finish"})
    wf.add_node("finish", _finish())
    wf.edge("finish", END).start("begin")
    return Pattern("review_and_critique", wf, cap * 4 + 8)


def swarm(
    agents: Mapping[str, Step],
    start: Union[str, Step, None] = None,
    *,
    max_handoffs: int = 6,
    model: Any = None,
) -> Pattern:
    """Agents hand the work to each other with no central coordinator. Each agent is told who it can
    hand off to and ends its reply with ``HANDOFF: <name>`` to pass the work on or ``FINAL`` when
    the answer is ready. ``start`` names the first agent (or is a dispatcher step that replies with a
    name); it defaults to the first agent. Ends on ``FINAL`` or after ``max_handoffs`` (at most 50);
    a hand-off to an unknown name ends the run with ``stop_reason="unknown_handoff"``.
    """
    if len(agents) < 2:
        raise ValueError("a swarm needs at least two agents")
    cap = _cap(max_handoffs, "max_handoffs")
    resolved = {name: _resolve((name, step), model, i)[1] for i, (name, step) in enumerate(agents.items())}
    names = list(resolved)
    dispatcher = None
    if start is not None and start not in resolved:
        dispatcher = _resolve(start, model, 0)[1]
    first = start if isinstance(start, str) and start in resolved else names[0]
    wf = Workflow(reducers={"trace": "append"})

    def begin(state: Dict[str, Any]) -> Any:
        return {"current": state["input"], "trace": [], "round": 0, "last": None}

    wf.add_node("begin", begin)
    wf.add_node("finish", _finish("final"))

    def agent_node(me: str):
        async def node(state: Dict[str, Any]) -> Any:
            others = [n for n in names if n != me]
            note = (
                f"You are '{me}' in a team. You can hand the work to: {', '.join(others)}. "
                "Do your part, then end your reply with exactly one line: 'HANDOFF: <name>' to pass it on, "
                "or 'FINAL' if the answer is ready for the user."
            )
            out = await resolved[me](f"{note}\n\nRequest:\n{state['input']}\n\nWork so far:\n{state['current']}")
            round_no = state["round"] + 1
            update = {"current": out, "round": round_no, "trace": _entry(me, out)}
            lines = [ln.strip() for ln in out.strip().splitlines() if ln.strip()]
            tail = lines[-1] if lines else "FINAL"
            if tail.upper().startswith("HANDOFF:"):
                target = tail.split(":", 1)[1].strip()
                if target not in resolved:
                    return goto("finish", {**update, "stop_reason": "unknown_handoff"})
                if round_no > cap:
                    return goto("finish", {**update, "stop_reason": "max_handoffs"})
                return goto(target, update)
            return goto("finish", {**update, "stop_reason": "final"})

        return node

    for name in names:
        wf.add_node(name, agent_node(name))
        wf.goto(name, [*[n for n in names if n != name], "finish"])
    if dispatcher is not None:
        async def dispatch(state: Dict[str, Any]) -> Any:
            choice = (await dispatcher(f"Choose who should start. Options: {', '.join(names)}.\nRequest: {state['input']}\nReply with one name.")).strip()
            return goto(choice if choice in resolved else names[0], {"trace": _entry("dispatcher", choice)})

        wf.add_node("dispatch", dispatch)
        wf.goto("dispatch", names)
        wf.edge("begin", "dispatch")
    else:
        wf.add_node("route_first", lambda s: goto(first))
        wf.goto("route_first", [first])
        wf.edge("begin", "route_first")
    wf.edge("finish", END).start("begin")
    return Pattern("swarm", wf, (cap + 3) * 2 + 8)


def human_review(step: Step, *, model: Any = None) -> Pattern:
    """Runs a step, then pauses for a person to look at its draft. Resume with
    ``"approve"``, ``{"decision": "edit", "text": "..."}`` or ``{"decision": "reject"}``.
    With ``compile(storage)`` the pause is durable: another process can resume it days later.

        paused = await flow.run("Release the dataset?")   # paused.suspended, paused.waiting_for
        done = await flow.resume(paused, "approve")
    """
    name, run = _resolve(step, model, 0)
    wf = Workflow(reducers={"trace": "append"})
    wf.add_node("begin", _begin)
    wf.add_node(name, _step_node(name, run))

    def review(state: Dict[str, Any], ctx: Any) -> Any:
        decision = ctx.resume
        if decision is None:
            return suspend({"draft": state["current"], "question": "Approve, edit or reject this draft."})
        kind = decision if isinstance(decision, str) else decision.get("decision")
        if kind == "approve":
            return {"stop_reason": "approved", "trace": _entry("person", "approved")}
        if kind == "edit" and isinstance(decision, dict) and isinstance(decision.get("text"), str):
            return {"current": decision["text"], "stop_reason": "approved", "trace": _entry("person", "edited")}
        if kind == "reject":
            return {"current": "", "stop_reason": "rejected", "trace": _entry("person", "rejected")}
        raise ValueError("decision must be 'approve', {'decision': 'edit', 'text': ...} or {'decision': 'reject'}")

    wf.add_node("review", review)
    wf.add_node("finish", _finish())
    wf.edge("begin", name).edge(name, "review").edge("review", "finish").edge("finish", END).start("begin")
    return Pattern("human_review", wf, 16)


def quick(spec: str, *, model: Any = None) -> Pattern:
    """A pattern from one line of registered specialist names (plug and play):

        patterns.quick("researcher > writer")        # one after the other
        patterns.quick("sentiment | keywords")       # at the same time
        patterns.quick("writer <-> critic")          # write, review, revise until approved
        patterns.quick("summarizer")                 # a single specialist

    One kind of connector per line (combine patterns with the functions above). The model
    comes from ``model=`` or the ``AGENTBUILD_MODEL`` environment variable."""
    if not isinstance(spec, str) or not spec.strip():
        raise ValueError("quick needs a line like 'researcher > writer'")
    pieces = re.split(r"(<->|>|\|)", spec)
    parts = [p.strip() for p in pieces[0::2]]
    connectors = set(pieces[1::2])
    if len(connectors) > 1:
        raise ValueError(f"{spec!r}: use one kind of connector per line (>, | or <->)")
    if any(not p for p in parts):
        raise ValueError(f"{spec!r}: a name is empty")
    kind = connectors.pop() if connectors else ">"
    if kind == "<->":
        if len(parts) != 2:
            raise ValueError(f"{spec!r}: <-> connects exactly two names (a writer and a reviewer)")
        return review_and_critique(parts[0], parts[1], model=model)
    if kind == "|":
        return parallel(parts, model=model)
    return sequential(parts, model=model)


# ------------------------------------------------------------- agent patterns

_REACT = (
    "Work in a loop: think about what you need next, call a tool, read the result, and repeat. "
    "Stop and answer as soon as you can; if a tool result looks wrong, say so instead of building on it."
)


def react(model: Any, tools: Sequence[Any], *, max_steps: int = 8, instructions: Optional[str] = None, **agent_options: Any) -> Agent:
    """A single agent in a reason-act loop with a hard cap on model calls (default 8, at most 50).
    The agent loop already thinks, acts and observes; this sets the prompt and the limit."""
    limits = {**agent_options.pop("limits", {}), "model_calls": _cap(max_steps, "max_steps")}
    text = f"{instructions.rstrip()}\n\n{_REACT}" if instructions else _REACT
    return Agent(model, instructions=text, tools=list(tools), limits=limits, **agent_options)


def coordinator(
    model: Any,
    specialists: Mapping[str, Any],
    *,
    instructions: Optional[str] = None,
    **agent_options: Any,
) -> Agent:
    """An agent that reads each request and hands it to the right specialist, which it calls as a tool.
    ``specialists``: ``{name: Agent | Specialist | "registered-name" | (Agent, "when to use it")}``.
    A model decides the routing (more flexible than a fixed workflow, and more model calls).

        triage = patterns.coordinator(m, {"orders": (orders_agent, "order status"), "refunds": refunds_agent})
    """
    if not specialists:
        raise ValueError("a coordinator needs at least one specialist")
    tools = []
    for name, spec in specialists.items():
        description = None
        if isinstance(spec, tuple) and len(spec) == 2 and isinstance(spec[1], str):
            spec, description = spec
        if isinstance(spec, str):
            spec = _specialists.get(spec)
        if isinstance(spec, _specialists.Specialist):
            description = description or spec.description
            spec = spec.build(_default_model(model))
        if not isinstance(spec, Agent):
            raise TypeError(f"specialist {name!r}: expected an Agent, a Specialist or its name, got {spec!r}")
        tools.append(spec.as_tool(name, description or f"Handles {name} requests"))
    roster = "\n".join(f"- {t.name}: {t.description}" for t in tools)
    base = instructions or "You are a coordinator. Decide which specialist should handle the request."
    return Agent(
        model,
        instructions=f"{base.rstrip()}\n\nSpecialists you can call (pass the request as input):\n{roster}\nCall the best one; if none fits, answer yourself briefly.",
        tools=tools,
        **agent_options,
    )


def hierarchical(model: Any, tree: Mapping[str, Any], *, instructions: Optional[str] = None, **agent_options: Any) -> Agent:
    """A coordinator of coordinators: the root splits the work, each level hands it further down, workers do it.
    ``tree`` maps names to a worker (as for :func:`coordinator`) or to a nested mapping, which becomes a
    sub-coordinator (describe it with a ``"_description"`` key). Depth is bounded by the SDK's agent nesting limit.

        patterns.hierarchical(m, {"research": {"_description": "gathers facts", "web": web_agent, "docs": docs_agent},
                                  "writing": writer_agent})
    """

    def build(level: Mapping[str, Any]) -> Dict[str, Any]:
        out: Dict[str, Any] = {}
        for name, node in level.items():
            if name == "_description":
                continue
            if isinstance(node, Mapping):
                out[name] = (coordinator(model, build(node), instructions=f"You lead the {name} team."), node.get("_description", f"The {name} team"))
            else:
                out[name] = node
        return out

    return coordinator(model, build(tree), instructions=instructions, **agent_options)

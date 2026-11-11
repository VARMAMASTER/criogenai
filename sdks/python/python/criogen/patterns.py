"""Ready-made designs for agentic systems: chains, fan-outs, loops, reviews, hand-offs and human review.

    from criogen import patterns

    flow = patterns.sequential(["researcher", "writer"], model="openai:gpt-5-mini")
    print((await flow.run("Write about solar panels")).output)

Steps can be an ``Agent``, a :class:`~criogen.specialists.Specialist` or
the name it is registered under (a string), a plain function ``text -> text``
(sync or async), or a ``(name, step)`` tuple to name the step yourself.
The model for specialists comes from ``model=`` or the ``CRIOGEN_MODEL``
environment variable. Every loop has a cap (at most 50 rounds), so nothing
runs forever.

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

CEILING = 50
"""The most rounds any loop runs (``max_iterations``, ``max_rounds``, ``max_handoffs``, ``max_steps``), whatever is asked for."""
DEFAULT_MODEL_ENV = "CRIOGEN_MODEL"

Step = Union[Agent, "_specialists.Specialist", str, Callable[[str], Any], Tuple[str, Any]]


# ------------------------------------------------------------------- results


@dataclass
class PatternResult:
    """What a pattern run produced.

    ``print(result)`` shows the output; ``result.explain()`` shows each step.

    Example:
        result = flow.run_sync("Write about solar panels")
        print(result.output)
        print(result.explain())
    """

    output: Optional[str]
    """The final answer (the last step's text), or ``None`` while ``suspended``."""
    trace: List[Dict[str, Any]]
    """Each step in order: ``{"step": name, "output": text}``."""
    state: Dict[str, Any]
    """The workflow's final state (``input``, ``output``, ``trace`` and the pattern's own keys)."""
    stop_reason: str
    """Why it ended: ``"done"``, ``"condition"``, ``"approved"``, ``"max_iterations"``, ``"max_rounds"``, ``"final"``, ``"max_handoffs"``, ``"unknown_handoff"``, ``"rejected"`` or ``"suspended"``."""
    suspended: bool = False
    """``True`` while a :func:`human_review` run waits for a person; continue with :meth:`Pattern.resume`."""
    waiting_for: Any = None
    """What the person is asked when ``suspended`` (for :func:`human_review`: ``{"draft", "question"}``), else ``None``."""
    raw: Optional[WorkflowResult] = field(default=None, repr=False)
    """The underlying :class:`~criogen.WorkflowResult` (snapshot, interrupts, run id)."""

    def __str__(self) -> str:
        return self.output or ""

    def explain(self) -> str:
        """Each step on its own line: which step, and what it returned (cut to 140 characters).

        Returns:
            The text, starting with ``ended: <stop_reason>``.
        """
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
    """A workflow pattern: run it, compile it for durable threads, or extend it.

    Made by the pattern functions (:func:`sequential`, :func:`parallel`,
    :func:`loop`, ...). Every run starts with the state ``{"input": text}``
    and the answer is ``state["output"]``.

    Args:
        name: The pattern's name, e.g. ``"sequential"``.
        workflow: The :class:`~criogen.Workflow` it runs. Available as
            ``pattern.workflow``: add nodes and edges to extend it before the
            first run.
        budget: The step limit :meth:`run` uses unless given ``max_steps``.

    Example:
        from criogen import patterns

        flow = patterns.sequential([lambda t: f"Outline: {t}", lambda t: t + " (expanded)"])
        result = flow.run_sync("solar panels")
        result.output   # "Outline: solar panels (expanded)"
    """

    def __init__(self, name: str, workflow: Workflow, budget: int):
        self.name = name
        self.workflow = workflow
        self._budget = budget
        self._compiled: Optional[CompiledWorkflow] = None

    def compile(self, storage: Union[None, str, Storage] = None) -> CompiledWorkflow:
        """The pattern as a compiled workflow, for durable threads, resume from another process and an audit trail.

        Start a thread run with ``{"input": text}``; the answer is
        ``state["output"]``. Pass ``max_steps`` if the pattern needs more than the
        default 64 steps. A :func:`human_review` pause is answered with
        ``thread.resume({"review": decision})``.

        Args:
            storage: A :class:`~criogen.Storage` or a storage URL. Default:
                process memory.

        Returns:
            A :class:`~criogen.CompiledWorkflow`.

        Example:
            app = flow.compile("sqlite://reviews.db")
            paused = await app.thread("request-1").run({"input": "Can I change my booking?"})
            done = await app.thread("request-1").resume({"review": "approve"})
            done.state["output"]
        """
        return self.workflow.compile(storage)

    async def run(self, text: str, **options: Any) -> PatternResult:
        """Runs the pattern once on ``text`` and returns the result.

        A :func:`human_review` pattern returns ``suspended=True``; continue it
        with :meth:`resume`.

        Args:
            text: The input, given to the first step.
            **options: ``max_steps`` (default: the pattern's own budget, sized
                from its steps and caps) and ``deadline`` (seconds), as for
                :meth:`CompiledWorkflow.run <criogen.CompiledWorkflow.run>`.

        Returns:
            The output, each step's trace and why the run ended.
        """
        if self._compiled is None:
            self._compiled = self.compile()
        options.setdefault("max_steps", self._budget)
        raw = await self._compiled.run({"input": text}, **options)
        return self._result(raw)

    def run_sync(self, text: str, **options: Any) -> PatternResult:
        """:meth:`run` for scripts (not inside an event loop).

        Args:
            text: The input.
            **options: As for :meth:`run`.
        """
        return _run_sync(self.run(text, **options))

    async def resume(self, paused: PatternResult, decision: Any) -> PatternResult:
        """Continues a :func:`human_review` run that is waiting for a person.

        Args:
            paused: The result of the :meth:`run` that paused (``suspended=True``).
            decision: ``"approve"`` (keep the draft), ``{"decision": "edit",
                "text": "..."}`` (use this text instead) or ``{"decision":
                "reject"}`` (empty output, ``stop_reason="rejected"``).

        Returns:
            The finished result.

        Raises:
            ValueError: ``paused`` is not a paused run of this pattern.
        """
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
    """Wraps any :class:`~criogen.Workflow` as a :class:`Pattern`, with the same ``run`` and ``compile`` as the ready-made ones.

    The workflow starts with the state ``{"input": text}`` and should write
    its answer to ``state["output"]`` (and a ``stop_reason`` if you like).

    Args:
        workflow: The workflow to run.
        name: The pattern's name. Default ``"custom"``.
        max_steps: The step limit for :meth:`Pattern.run`. Default 64.

    Returns:
        The pattern.

    Example:
        from criogen import END, Workflow, patterns

        wf = Workflow()
        wf.add_node("shout", lambda state: {"output": state["input"].upper()})
        wf.start("shout").edge("shout", END)
        patterns.custom(wf, name="shout").run_sync("hello").output   # "HELLO"
    """
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
    """Runs the steps in a fixed order; each step's output is the next step's input.

    Steps can be an ``Agent``, a :class:`~criogen.specialists.Specialist` or
    the name it is registered under (a string), a plain function ``text -> text``
    (sync or async), or a ``(name, step)`` tuple to name the step yourself.

    Args:
        steps: The steps, in order (at least one).
        model: The model for steps given as specialists (a model string or
            object). Default: the ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`; its output is the last step's text.

    Raises:
        ValueError: No steps, or a specialist step and no model.

    Example:
        from criogen import patterns

        def outline(text):
            return f"Outline: {text}"

        flow = patterns.sequential([outline, "writer"], model="openai:gpt-5-mini")
        result = await flow.run("Write about solar panels")
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
    """Runs the branches at the same time on the same input, then combines their answers.

    With a ``synthesizer`` step, it receives the original request and every
    branch's answer and writes the output. Without one, the output is the
    answers as a list with a ``## <name>`` heading each, in branch order.

    Args:
        branches: The steps, as a list or as ``{name: step}`` (at least one).
        synthesizer: A step that combines the answers. Default: none.
        model: The model for steps given as specialists. Default: the
            ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`.

    Raises:
        ValueError: No branches, or a specialist step and no model.

    Example:
        from criogen import patterns

        flow = patterns.parallel({
            "upper": str.upper,
            "length": lambda text: f"{len(text)} characters",
        })
        print(flow.run_sync("hello").output)   # "## upper", "HELLO", "## length", "5 characters"
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
    """Repeats the steps until a condition holds or the round limit is reached.

    Each round runs the steps as a chain (each output feeds the next); the
    first step of the next round gets the last step's output.

    Args:
        steps: The steps of one round (at least one).
        until: ``until(text, round)`` is called after each round with the
            round's output and its number (from 1); ``True`` ends the loop
            (``stop_reason="condition"``).
        max_iterations: The most rounds (at most 50). Default 5; reaching it
            ends with ``stop_reason="max_iterations"``.
        model: The model for steps given as specialists. Default: the
            ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`.

    Raises:
        ValueError: No steps, ``max_iterations`` below 1, or a specialist step
            and no model.

    Example:
        from criogen import patterns

        flow = patterns.loop([lambda t: t + "!"], until=lambda text, n: text.count("!") >= 3)
        result = flow.run_sync("Go")
        result.output, result.stop_reason   # ("Go!!!", "condition")
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
    """Improves a text round by round until a score is high enough.

    ``refine`` gets the current text and returns a better one; after each
    round, ``score(text)`` decides whether to stop. A :func:`loop` with one
    step: ``stop_reason`` is ``"condition"`` or ``"max_iterations"``.

    Args:
        refine: The step that improves the text.
        score: A function ``text -> float``.
        threshold: Stop when ``score(text) >= threshold``. Default 0.8.
        max_iterations: The most rounds (at most 50). Default 5.
        model: The model if ``refine`` is a specialist. Default: the
            ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`.

    Example:
        from criogen import patterns

        flow = patterns.iterative_refinement(
            "writer",
            score=lambda text: min(len(text) / 500, 1.0),
            threshold=1.0,
            model="openai:gpt-5-mini",
        )
    """
    return loop([refine], lambda text, _round: score(text) >= threshold, max_iterations=max_iterations, model=model)


def review_and_critique(
    generator: Step,
    critic: Step,
    *,
    max_rounds: int = 3,
    approved: Optional[Callable[[str], bool]] = None,
    model: Any = None,
) -> Pattern:
    """A generator writes, a critic reviews, and the generator revises until the critic approves.

    The critic approves when its reply starts with ``APPROVED`` (or when
    ``approved(reply)`` is true); otherwise its reply is passed back to the
    generator as feedback with the previous draft. The output is the last
    draft.

    Args:
        generator: The step that writes and revises.
        critic: The step that reviews (the ``"critic"`` specialist replies
            ``APPROVED`` or ``REVISE: ...``).
        max_rounds: The most reviews (at most 50). Default 3.
        approved: A function ``reply -> bool`` that decides approval instead
            of the ``APPROVED`` prefix.
        model: The model for steps given as specialists. Default: the
            ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`; ``stop_reason`` is ``"approved"`` or
        ``"max_rounds"``.

    Example:
        from criogen import patterns

        flow = patterns.review_and_critique("writer", "critic", max_rounds=3, model="openai:gpt-5-mini")
        result = await flow.run("A two-line product description for a desk lamp")
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
    r"""Agents hand the work to each other, with no central coordinator.

    Each agent is told who it can hand off to, and ends its reply with a line
    ``HANDOFF: <name>`` to pass the work on or ``FINAL`` when the answer is
    ready (a reply with neither counts as ``FINAL``). Each agent sees the
    request and the work so far.

    Args:
        agents: ``{name: step}``, at least two.
        start: The first agent's name, or a dispatcher step that replies with
            a name (an unknown name falls back to the first agent). Default:
            the first agent.
        max_handoffs: The most hand-offs (at most 50). Default 6.
        model: The model for steps given as specialists. Default: the
            ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`; ``stop_reason`` is ``"final"``,
        ``"max_handoffs"`` or ``"unknown_handoff"`` (a hand-off to a name not
        in ``agents``).

    Raises:
        ValueError: Fewer than two agents, or ``max_handoffs`` below 1.

    Example:
        from criogen import patterns

        flow = patterns.swarm({
            "triage": lambda text: "Looks like billing.\nHANDOFF: billing",
            "billing": lambda text: "Your invoice was sent again.\nFINAL",
        })
        result = flow.run_sync("I did not get my invoice")
        result.stop_reason   # "final"
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
    """Runs a step, then pauses so a person can approve, edit or reject its draft.

    The run returns ``suspended=True`` with ``waiting_for`` set to
    ``{"draft", "question"}``. Continue it with :meth:`Pattern.resume`. With
    ``compile(storage)`` the pause is durable: another process can answer it
    days later with ``thread.resume({"review": decision})``.

    Args:
        step: The step that writes the draft.
        model: The model if ``step`` is a specialist. Default: the
            ``CRIOGEN_MODEL`` environment variable.

    Returns:
        A :class:`Pattern`; ``stop_reason`` is ``"approved"`` (also after an
        edit) or ``"rejected"``.

    Example:
        from criogen import patterns

        flow = patterns.human_review(lambda text: f"Draft reply to: {text}")
        paused = await flow.run("Can I change my booking?")
        paused.waiting_for["draft"]
        done = await flow.resume(paused, {"decision": "edit", "text": "Yes, from your account page."})
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
    """A pattern from one line of registered specialist names.

    ``"a > b"`` runs them one after the other (:func:`sequential`), ``"a |
    b"`` at the same time (:func:`parallel`), ``"writer <-> critic"`` writes,
    reviews and revises until approved (:func:`review_and_critique`), and a
    single name runs one specialist. Use one kind of connector per line;
    combine patterns with the functions above.

    Args:
        spec: The line, e.g. ``"researcher > writer"``.
        model: The specialists' model. Default: the ``CRIOGEN_MODEL``
            environment variable.

    Returns:
        A :class:`Pattern`.

    Raises:
        ValueError: An empty line or name, mixed connectors, or ``<->``
            between more than two names.
        KeyError: A name is not registered.

    Example:
        from criogen import patterns

        flow = patterns.quick("researcher > writer", model="openai:gpt-5-mini")
        result = await flow.run("Write about solar panels")
    """
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
    """A single agent in a reason-act loop (think, call a tool, read the result, repeat), with a hard cap on model calls.

    The agent loop already works this way; this adds instructions for it and
    the limit.

    Args:
        model: The agent's model.
        tools: Its tools.
        max_steps: The most model calls (at most 50). Default 8.
        instructions: Your instructions, put before the reason-act ones.
        **agent_options: Other ``Agent`` keyword arguments (``storage``,
            ``approve``, ``limits``, ...).

    Returns:
        An ``Agent``.

    Example:
        from criogen import patterns, tool

        @tool
        def lookup_order(order_id: str) -> str:
            "Looks up an order's status."
            return "shipped"

        agent = patterns.react("openai:gpt-5-mini", [lookup_order], max_steps=5)
        result = await agent.run("Where is order A1?")
    """
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

    A model decides the routing: more flexible than a fixed workflow, and it
    costs more model calls.

    Args:
        model: The coordinator's model; also used to build specialists given
            as recipes or names.
        specialists: ``{name: specialist}``, where each value is an
            ``Agent``, a ``Specialist``, a registered name, or ``(agent,
            "when to use it")``. The description is what the coordinator reads
            to choose (a ``Specialist`` brings its own).
        instructions: The coordinator's instructions. Default: a short
            routing prompt.
        **agent_options: Other ``Agent`` keyword arguments.

    Returns:
        An ``Agent``.

    Raises:
        ValueError: No specialists.
        TypeError: A value is not an agent, a specialist or a name.

    Example:
        from criogen import Agent, patterns

        orders = Agent("openai:gpt-5-mini", instructions="You answer order questions.")
        triage = patterns.coordinator(
            "openai:gpt-5-mini",
            {"orders": (orders, "Order status questions"), "summaries": "summarizer"},
        )
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
    """A coordinator of coordinators: the root splits the work, each level hands it further down, and workers do it.

    Args:
        model: The model of every coordinator (and of workers given as
            recipes or names).
        tree: ``{name: worker or subtree}``. A worker is anything
            :func:`coordinator` accepts; a nested mapping becomes a
            sub-coordinator, described by its ``"_description"`` key. Depth is
            bounded by the agents' nesting limit.
        instructions: The root coordinator's instructions.
        **agent_options: Other ``Agent`` keyword arguments for the root.

    Returns:
        The root ``Agent``.

    Example:
        from criogen import patterns

        lead = patterns.hierarchical("openai:gpt-5-mini", {
            "research": {"_description": "Gathers facts", "web": "researcher", "check": "fact_checker"},
            "writing": "writer",
        })
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

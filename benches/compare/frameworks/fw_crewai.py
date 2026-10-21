"""CrewAI (design 0016): agents with `Agent` + `Task` + `Crew`, workflows
with `Flow`, as its documentation shows them. The simulated model is a
`BaseLLM` subclass with native tool calling (CrewAI then runs one turn's
tool calls in parallel), answering from the shared script."""

import asyncio
import json
import time
from importlib.metadata import version
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple

from common import ANSWER, Counter, ORDER, Script, TOOLS, note, tool_body
from crewai import Agent, Crew, Task
from crewai.flow.flow import Flow, and_, listen, start
from crewai.llms.base_llm import BaseLLM
from crewai.tools import tool as crew_tool

PROMPT = f"Where is my refund for order {ORDER}?"

NOTES = (
    "Crews run with cache=False so tools run on every run, as in the other frameworks "
    "(CrewAI caches tool results by default). Flows are instantiated per run (their state "
    "lives on the instance)."
)


def info() -> Dict[str, Any]:
    return {"label": "CrewAI", "version": version("crewai"), "notes": NOTES}


class ScriptedLLM(BaseLLM):
    """Answers from the shared script with native tool calls, after `delay`
    seconds; can fail the first `fail_first` calls."""

    mode: str = "tools"
    tools: int = 5
    delay: float = 0.0
    fail_first: int = 0
    bad_arguments: bool = False
    calls: int = 0

    def __init__(self, **data: Any):
        super().__init__(model="scripted", **data)

    def supports_function_calling(self) -> bool:
        return True

    def _reply(self, messages: Any) -> Any:
        self.calls += 1
        if self.calls <= self.fail_first:
            raise RuntimeError("simulated rate limit (429)")
        results = sum(1 for m in messages if isinstance(m, dict) and m.get("role") == "tool") if isinstance(messages, list) else 0
        step = Script(self.mode, self.tools).next(results)
        if "answer" in step:
            return step["answer"]
        return [
            {
                "id": f"call_{self.calls}_{i}",
                "type": "function",
                "function": {"name": name, "arguments": json.dumps({"order_id": 1001} if self.bad_arguments else args)},
            }
            for i, (name, args) in enumerate(step["calls"])
        ]

    def call(self, messages: Any, tools: Any = None, callbacks: Any = None, available_functions: Any = None,
             from_task: Any = None, from_agent: Any = None, response_model: Any = None) -> Any:
        if self.delay:
            time.sleep(self.delay)
        return self._reply(messages)

    async def acall(self, messages: Any, tools: Any = None, callbacks: Any = None, available_functions: Any = None,
                    from_task: Any = None, from_agent: Any = None, response_model: Any = None) -> Any:
        if self.delay:
            await asyncio.sleep(self.delay)
        return self._reply(messages)


def _tool(name: str, delay: float, counter: Optional[Counter], body: Any = None) -> Any:
    def call(order_id: str) -> dict:
        return (body or tool_body)(name, order_id, delay, counter)

    call.__name__ = name
    call.__doc__ = f"{name.replace('_', ' ').capitalize()} for an order."
    return crew_tool(name)(call)


def _crew(llm: ScriptedLLM, tools: List[Any], **crew: Any) -> Crew:
    support = Agent(
        role="Support agent",
        goal="Answer customers about their orders",
        backstory="You help customers of an online shop.",
        llm=llm,
        tools=tools,
        verbose=False,
    )
    task = Task(description=PROMPT, expected_output="A short answer for the customer.", agent=support)
    return Crew(agents=[support], tasks=[task], cache=False, verbose=False, **crew)


def agent(
    mode: str,
    model_delay: float,
    tool_delay: float,
    counter: Optional[Counter] = None,
    tools: int = 5,
    concurrent: bool = False,
) -> Tuple[Callable[[], Any], bool]:
    """One crew reused for sequential runs (`kickoff`); for concurrent runs
    a crew per run (a crew holds per-run state) with native async
    `akickoff`."""

    def make() -> Crew:
        return _crew(ScriptedLLM(mode=mode, tools=tools, delay=model_delay), [_tool(n, tool_delay, counter) for n in TOOLS[:tools]])

    if concurrent:

        async def run_async() -> str:
            return (await make().akickoff()).raw

        return run_async, True

    crew = make()

    def run() -> str:
        return crew.kickoff().raw

    return run, False


def chain(n: int) -> Tuple[Callable[[], Any], bool]:
    attrs: Dict[str, Any] = {}
    for i in range(n):

        def step(self: Any, i: int = i) -> None:
            self.state[f"k{i}"] = i

        step.__name__ = f"n{i}"
        attrs[f"n{i}"] = start()(step) if i == 0 else listen(f"n{i - 1}")(step)
    Chain = type("Chain", (Flow,), attrs)

    def run() -> Any:
        return Chain().kickoff()

    return run, False


def fanout(n: int, delay: float) -> Tuple[Callable[[], Any], bool]:
    attrs: Dict[str, Any] = {}

    def begin(self: Any) -> None:
        self.state["started"] = True

    attrs["begin"] = start()(begin)
    names = []
    for i in range(n):

        def work(self: Any, i: int = i) -> None:
            time.sleep(delay)
            self.state[f"p{i}"] = i

        work.__name__ = f"p{i}"
        attrs[f"p{i}"] = listen("begin")(work)
        names.append(f"p{i}")

    def end(self: Any) -> None:
        self.state["done"] = True

    attrs["end"] = listen(and_(*names))(end)
    FanOut = type("FanOut", (Flow,), attrs)

    def run() -> Any:
        return FanOut().kickoff()

    return run, False


def model_probe() -> Optional[Callable[[], Any]]:
    llm = ScriptedLLM(mode="answer")
    messages = [{"role": "user", "content": PROMPT}]
    return lambda: llm.call(messages)


# ---------- reliability (design 0016 §3) ----------


def rel_model_transient() -> Dict[str, Any]:
    llm = ScriptedLLM(mode="answer", fail_first=1)
    try:
        text = _crew(llm, []).kickoff().raw
    except Exception as err:  # noqa: BLE001
        return {"outcome": "fail", "reason": f"the run failed: {type(err).__name__}: {err}"}
    if text == ANSWER:
        return {
            "outcome": "pass",
            "reason": f"the agent retried and answered ({llm.calls} model calls)",
            "note": "CrewAI re-runs the agent's whole task after an error (max_retry_limit, default 2).",
        }
    return {"outcome": "fail", "reason": f"unexpected answer {text!r}"}


def rel_tool_exception() -> Dict[str, Any]:
    def broken(name: str, order_id: Any, delay: float, counter: Optional[Counter]) -> dict:
        raise RuntimeError("database is down")

    try:
        text = _crew(ScriptedLLM(mode="tools", tools=1), [_tool("get_order", 0, None, broken)]).kickoff().raw
    except Exception as err:  # noqa: BLE001
        return {"outcome": "fail", "reason": f"the run stopped with {type(err).__name__}: {err}"}
    if text == ANSWER:
        return {"outcome": "pass", "reason": "the tool's error was sent to the model, which answered"}
    return {"outcome": "partial", "reason": f"the run completed with {text!r}"}


def rel_bad_arguments() -> Dict[str, Any]:
    counter = Counter()
    try:
        text = _crew(ScriptedLLM(mode="tools", tools=1, bad_arguments=True), [_tool("get_order", 0, counter)]).kickoff().raw
    except Exception as err:  # noqa: BLE001
        return {"outcome": "fail", "reason": f"the run failed: {type(err).__name__}: {err}"}
    if counter.calls:
        return {"outcome": "fail", "reason": f"the tool ran with bad arguments ({counter.calls}); the run answered {text!r}"}
    if text == ANSWER:
        return {"outcome": "pass", "reason": "the arguments were rejected before the tool ran; the model answered"}
    return {"outcome": "partial", "reason": f"the tool did not run, but the answer was {text!r}"}


def rel_runaway_loop() -> Dict[str, Any]:
    counter = Counter()
    llm = ScriptedLLM(mode="runaway")
    started = time.perf_counter()
    try:
        text = _crew(llm, [_tool("get_order", 0, counter)]).kickoff().raw
    except Exception as err:  # noqa: BLE001
        took = (time.perf_counter() - started) * 1000
        return {
            "outcome": "pass" if llm.calls <= 100 else "partial",
            "reason": f"stopped after {llm.calls} model calls and {len(counter.calls)} tool calls in {took:.0f} ms: {type(err).__name__}",
            "note": "At max_iter (25) CrewAI asks the model once more for a final answer; the scripted model "
            "kept asking for a tool, which CrewAI rejected with this error. A real model would usually answer in text.",
            "log": str(err)[:1500],
        }
    took = (time.perf_counter() - started) * 1000
    return {
        "outcome": "pass" if llm.calls <= 100 else "partial",
        "reason": f"stopped after {llm.calls} model calls and {len(counter.calls)} tool calls in {took:.0f} ms, "
        f"with a forced final answer instead of an error",
        "log": str(text)[:1500],
    }


# ---------- crash and resume ----------


CRASH_NOTE = (
    "Uses BaseTool subclasses: tools made with @tool keep a Python function that CrewAI's "
    "checkpoints cannot serialize, so no checkpoint is written for a crew using them."
)


def _crash_crew(work: Path) -> Crew:
    from crewai.state.checkpoint_config import CheckpointConfig
    from crewai.tools import BaseTool
    from pydantic import BaseModel

    log = work / "calls.log"

    class Args(BaseModel):
        order_id: str

    class GetOrder(BaseTool):
        name: str = "get_order"
        description: str = "Get order for an order."
        args_schema: type[BaseModel] = Args

        def _run(self, order_id: str) -> dict:
            note(log, "get_order done")
            return {"order": order_id, "status": "shipped"}

    class GetInvoice(BaseTool):
        name: str = "get_invoice"
        description: str = "Get invoice for an order."
        args_schema: type[BaseModel] = Args

        def _run(self, order_id: str) -> dict:
            note(log, "get_invoice started")
            if (work / "slow").exists():
                time.sleep(60)
            note(log, "get_invoice done")
            return {"invoice": order_id, "amount": 40}

    class Two(ScriptedLLM):
        def _reply(self, messages: Any) -> Any:
            self.calls += 1
            if any(isinstance(m, dict) and m.get("role") == "tool" for m in messages):
                return ANSWER
            return [
                {"id": "call_1", "type": "function", "function": {"name": "get_order", "arguments": json.dumps({"order_id": ORDER})}},
                {"id": "call_2", "type": "function", "function": {"name": "get_invoice", "arguments": json.dumps({"order_id": ORDER})}},
            ]

    tools = [GetOrder(), GetInvoice()]
    # The most detailed checkpointing CrewAI offers: on every event.
    checkpoint = CheckpointConfig(location=str(work / "checkpoints"), on_events=["*"])
    return _crew(Two(), tools, checkpoint=checkpoint)


def _scripted_parts(work: Path) -> Any:
    """The crash crew's model and tools, to attach to a restored crew."""
    crew = _crash_crew(work)
    return crew.agents[0].llm, crew.agents[0].tools


def crash_phase(phase: int, work: Path) -> Dict[str, Any]:
    from crewai.state.checkpoint_config import CheckpointConfig

    crew = _crash_crew(work)
    if phase == 1:
        crew.kickoff()  # killed while get_invoice runs
        return {"outcome": "fail", "reason": "phase 1 was not killed"}
    saved = sorted((p for p in (work / "checkpoints").rglob("*") if p.is_file()), key=lambda p: p.stat().st_mtime)
    if not saved:
        return {"outcome": "fail", "reason": "no checkpoint was written before the crash", "note": CRASH_NOTE}
    # A checkpoint stores the model by name and rebuilds it on restore; the
    # scripted model has no name to rebuild from, so the same model and
    # tools are attached to the restored crew before it continues.
    restored = Crew.from_checkpoint(CheckpointConfig(restore_from=str(saved[-1])))
    llm, tools = _scripted_parts(work)
    for member in restored.agents:
        member.llm = llm
        member.tools = tools
    output = restored.kickoff()
    return {
        "answer": output.raw,
        "how": f"Crew.from_checkpoint(…) from the newest of {len(saved)} checkpoints, then kickoff()",
        "note": CRASH_NOTE + " The scripted model and tools were attached again after the restore.",
    }

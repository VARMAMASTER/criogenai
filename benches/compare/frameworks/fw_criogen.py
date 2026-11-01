"""Criogen through its Python SDK (design 0016): agents and workflows as
its documentation shows them, with its built-in simulated model."""

import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple

from common import ANSWER, Counter, ORDER, TOOLS, note, tool_body

from criogen import END, Agent, FakeModel, FakeReply, Storage, Workflow, __version__, tool

PROMPT = f"Where is my refund for order {ORDER}?"


def info() -> Dict[str, Any]:
    return {"label": "Criogen", "version": __version__}


def _named(name: str, delay: float, counter: Optional[Counter], body: Any) -> Callable[..., dict]:
    def call(order_id: str) -> dict:
        return (body or tool_body)(name, order_id, delay, counter)

    call.__name__ = name
    call.__qualname__ = name
    call.__doc__ = f"{name.replace('_', ' ').capitalize()} for an order."
    return call


def _replies(mode: str, tools: int = 5) -> List[Any]:
    if mode == "answer" or tools == 0:
        return [ANSWER]
    if mode == "runaway":
        return [FakeReply.tool_call("get_order", {"order_id": ORDER})]
    return [FakeReply.tool_calls([(name, {"order_id": ORDER}) for name in TOOLS[:tools]]), ANSWER]


def agent(
    mode: str,
    model_delay: float,
    tool_delay: float,
    counter: Optional[Counter] = None,
    tools: int = 5,
    concurrent: bool = False,
) -> Tuple[Callable[[], Any], bool]:
    """One agent, reused for every run (the simulated model picks replies
    by the conversation's turn). Its async API serves single and
    concurrent runs alike."""
    built = Agent(
        FakeModel(_replies(mode, tools), delay=model_delay, by_turn=True),
        tools=[tool(_named(n, tool_delay, counter, None)) for n in TOOLS[:tools]],
    )

    async def run() -> str:
        return (await built.run(PROMPT)).text

    return run, True


def chain(n: int) -> Tuple[Callable[[], Any], bool]:
    wf = Workflow()
    for i in range(n):
        wf.add_node(f"n{i}", (lambda i: lambda state: {f"k{i}": i})(i))
        if i:
            wf.edge(f"n{i - 1}", f"n{i}")
    wf.start("n0").edge(f"n{n - 1}", END)
    app = wf.compile()

    async def run() -> Any:
        return await app.run({})

    return run, True


def fanout(n: int, delay: float) -> Tuple[Callable[[], Any], bool]:
    wf = Workflow()
    wf.add_node("start", lambda state: {})
    names = []
    for i in range(n):
        name = f"p{i}"
        wf.add_node(name, _sleeper(i, delay))
        wf.edge("start", name)
        names.append(name)
    wf.add_node("end", lambda state: {"done": True})
    wf.start("start").join(names, "end").edge("end", END)
    app = wf.compile()

    async def run() -> Any:
        return await app.run({})

    return run, True


def _sleeper(i: int, delay: float) -> Callable[[Dict[str, Any]], Dict[str, Any]]:
    """A node doing `delay` of blocking I/O (a plain function, as most are)."""

    def work(state: Dict[str, Any]) -> Dict[str, Any]:
        time.sleep(delay)
        return {f"p{i}": i}

    return work


def model_probe() -> Optional[Callable[[], Any]]:
    """The simulated model is Criogen's FakeModel, in Rust: it cannot be
    called alone from Python, so no separate figure."""
    return None


# ---------- reliability (design 0016 §3) ----------


async def rel_model_transient() -> Dict[str, Any]:
    built = Agent(FakeModel([ANSWER], fail_first=1, by_turn=True))
    try:
        text = (await built.run(PROMPT)).text
    except Exception as err:  # noqa: BLE001 - the outcome is what we report
        return {"outcome": "fail", "reason": f"the run failed: {type(err).__name__}: {err}"}
    if text == ANSWER:
        return {"outcome": "pass", "reason": "the agent retried the rate-limited call by itself and answered"}
    return {"outcome": "fail", "reason": f"unexpected answer {text!r}"}


async def rel_tool_exception() -> Dict[str, Any]:
    def broken(name: str, order_id: Any, delay: float, counter: Optional[Counter]) -> dict:
        raise RuntimeError("database is down")

    built = Agent(
        FakeModel([FakeReply.tool_call("get_order", {"order_id": ORDER}), ANSWER], by_turn=True),
        tools=[tool(_named("get_order", 0, None, broken))],
    )
    try:
        text = (await built.run(PROMPT)).text
    except Exception as err:  # noqa: BLE001
        return {
            "outcome": "fail",
            "reason": f"the run stopped with {type(err).__name__}: {err}",
            "note": "By design: an unexpected exception fails the run (a thread can retry it). "
            "Raise criogen.ToolError to send the message to the model instead.",
        }
    return {"outcome": "pass" if text == ANSWER else "partial", "reason": "the model saw the error and answered"}


async def rel_bad_arguments() -> Dict[str, Any]:
    counter = Counter()
    built = Agent(
        FakeModel([FakeReply.tool_call("get_order", {"order_id": 1001}), ANSWER], by_turn=True),
        tools=[tool(_named("get_order", 0, counter, None))],
    )
    try:
        result = await built.run(PROMPT)
    except Exception as err:  # noqa: BLE001
        return {"outcome": "fail", "reason": f"the run failed: {type(err).__name__}: {err}"}
    errors = [m for m in result.messages if m["role"] == "tool"]
    if counter.calls:
        return {"outcome": "fail", "reason": f"the tool ran with bad arguments: {counter.calls}"}
    if result.text == ANSWER:
        return {
            "outcome": "pass",
            "reason": "the arguments were rejected against the schema before the tool ran; "
            "the model got the error and answered",
            "log": str(errors[:1]),
        }
    return {"outcome": "partial", "reason": f"the tool did not run, but the answer was {result.text!r}"}


async def rel_runaway_loop() -> Dict[str, Any]:
    counter = Counter()
    built = Agent(
        FakeModel(_replies("runaway"), by_turn=True),
        tools=[tool(_named("get_order", 0, counter, None))],
    )
    started = time.perf_counter()
    try:
        await built.run(PROMPT)
    except Exception as err:  # noqa: BLE001
        took = (time.perf_counter() - started) * 1000
        return {
            "outcome": "pass",
            "reason": f"stopped after {len(counter.calls)} tool calls in {took:.0f} ms: {type(err).__name__}",
            "log": str(err),
        }
    return {"outcome": "fail", "reason": "the run ended without an error"}


# ---------- crash and resume ----------


def _crash_agent(work: Path) -> Agent:
    log = work / "calls.log"

    def get_order(order_id: str) -> dict:
        "Get order for an order."
        note(log, "get_order done")
        return {"order": order_id, "status": "shipped"}

    def get_invoice(order_id: str) -> dict:
        "Get invoice for an order."
        note(log, "get_invoice started")
        if (work / "slow").exists():
            time.sleep(60)
        note(log, "get_invoice done")
        return {"invoice": order_id, "amount": 40}

    reply = FakeReply.tool_calls([("get_order", {"order_id": ORDER}), ("get_invoice", {"order_id": ORDER})])
    return Agent(
        FakeModel([reply, ANSWER], by_turn=True),
        tools=[tool(get_order), tool(get_invoice)],
        storage=Storage(f"sqlite:///{(work / 'criogen.db').as_posix()}"),
    )


async def crash_phase(phase: int, work: Path) -> Dict[str, Any]:
    thread = _crash_agent(work).thread("ticket-7")
    if phase == 1:
        await thread.send(PROMPT)  # killed while get_invoice runs
        return {"outcome": "fail", "reason": "phase 1 was not killed"}
    reply = await thread.retry()
    return {"answer": reply.text, "how": "thread.retry() in a new process, same SQLite file"}

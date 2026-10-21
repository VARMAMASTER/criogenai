"""LangGraph and LangChain (design 0016): agents with LangChain's
`create_agent`, workflows with LangGraph's `StateGraph`, as their
documentation shows them. The simulated model is a `BaseChatModel`
subclass answering from the shared script."""

import asyncio
import operator
import time
from importlib.metadata import version
from pathlib import Path
from typing import Annotated, Any, Callable, Dict, List, Optional, Sequence, Tuple, TypedDict

from common import ANSWER, Counter, ORDER, Script, TOOLS, note, tool_body
from langchain.agents import create_agent
from langchain_core.language_models import BaseChatModel
from langchain_core.messages import AIMessage, BaseMessage, HumanMessage, ToolMessage
from langchain_core.outputs import ChatGeneration, ChatResult
from langchain_core.tools import StructuredTool
from langgraph.graph import END, START, StateGraph

PROMPT = f"Where is my refund for order {ORDER}?"


def info() -> Dict[str, Any]:
    return {
        "label": "LangGraph",
        "version": version("langgraph"),
        "also": {"langchain": version("langchain"), "langchain-core": version("langchain-core")},
    }


class ScriptedModel(BaseChatModel):
    """Answers from the shared script, after `delay` seconds; can fail the
    first `fail_first` calls with a rate-limit-like error."""

    mode: str = "tools"
    tools: int = 5
    delay: float = 0.0
    fail_first: int = 0
    bad_arguments: bool = False
    calls: int = 0

    @property
    def _llm_type(self) -> str:
        return "scripted"

    def bind_tools(self, tools: Sequence[Any], **kwargs: Any) -> "ScriptedModel":
        return self

    def _reply(self, messages: List[BaseMessage]) -> AIMessage:
        self.calls += 1
        if self.calls <= self.fail_first:
            raise RuntimeError("simulated rate limit (429)")
        start = max(i for i, m in enumerate(messages) if isinstance(m, HumanMessage))
        results = sum(1 for m in messages[start:] if isinstance(m, ToolMessage))
        step = Script(self.mode, self.tools).next(results)
        if "answer" in step:
            return AIMessage(content=step["answer"])
        calls = [
            {"name": name, "args": {"order_id": 1001} if self.bad_arguments else args, "id": f"call_{self.calls}_{i}"}
            for i, (name, args) in enumerate(step["calls"])
        ]
        return AIMessage(content="", tool_calls=calls)

    def _generate(self, messages: List[BaseMessage], stop: Any = None, run_manager: Any = None, **kwargs: Any) -> ChatResult:
        if self.delay:
            time.sleep(self.delay)
        return ChatResult(generations=[ChatGeneration(message=self._reply(messages))])

    async def _agenerate(self, messages: List[BaseMessage], stop: Any = None, run_manager: Any = None, **kwargs: Any) -> ChatResult:
        if self.delay:
            await asyncio.sleep(self.delay)
        return ChatResult(generations=[ChatGeneration(message=self._reply(messages))])


def _tool(name: str, delay: float, counter: Optional[Counter], body: Any = None) -> StructuredTool:
    def call(order_id: str) -> dict:
        return (body or tool_body)(name, order_id, delay, counter)

    return StructuredTool.from_function(call, name=name, description=f"{name.replace('_', ' ').capitalize()} for an order.")


def agent(
    mode: str,
    model_delay: float,
    tool_delay: float,
    counter: Optional[Counter] = None,
    tools: int = 5,
    concurrent: bool = False,
) -> Tuple[Callable[[], Any], bool]:
    """One agent graph, reused for every run, through its async API
    (`ainvoke`)."""
    graph = create_agent(
        ScriptedModel(mode=mode, tools=tools, delay=model_delay),
        tools=[_tool(n, tool_delay, counter) for n in TOOLS[:tools]],
    )

    async def run() -> str:
        state = await graph.ainvoke({"messages": [HumanMessage(PROMPT)]})
        return state["messages"][-1].content

    return run, True


def chain(n: int) -> Tuple[Callable[[], Any], bool]:
    State = TypedDict("State", {f"k{i}": int for i in range(n)}, total=False)  # type: ignore[misc]
    graph = StateGraph(State)
    for i in range(n):
        graph.add_node(f"n{i}", (lambda i: lambda state: {f"k{i}": i})(i))
        graph.add_edge(START if i == 0 else f"n{i - 1}", f"n{i}")
    graph.add_edge(f"n{n - 1}", END)
    app = graph.compile()

    async def run() -> Any:
        return await app.ainvoke({})

    return run, True


def fanout(n: int, delay: float) -> Tuple[Callable[[], Any], bool]:
    keys: Dict[str, Any] = {f"p{i}": int for i in range(n)}
    keys["done"] = bool
    State = TypedDict("State", keys, total=False)  # type: ignore[misc]
    graph = StateGraph(State)
    graph.add_node("start", lambda state: {})
    names = []
    for i in range(n):

        def work(state: Dict[str, Any], i: int = i) -> Dict[str, Any]:
            time.sleep(delay)
            return {f"p{i}": i}

        graph.add_node(f"p{i}", work)
        graph.add_edge("start", f"p{i}")
        names.append(f"p{i}")
    graph.add_node("end", lambda state: {"done": True})
    graph.add_edge(START, "start")
    graph.add_edge(names, "end")
    graph.add_edge("end", END)
    app = graph.compile()

    async def run() -> Any:
        return await app.ainvoke({})

    return run, True


def model_probe() -> Optional[Callable[[], Any]]:
    model = ScriptedModel(mode="answer")
    messages = [HumanMessage(PROMPT)]
    return lambda: model.invoke(messages)


# ---------- reliability (design 0016 §3) ----------


async def rel_model_transient() -> Dict[str, Any]:
    graph = create_agent(ScriptedModel(mode="answer", fail_first=1))
    try:
        state = await graph.ainvoke({"messages": [HumanMessage(PROMPT)]})
    except Exception as err:  # noqa: BLE001
        return {
            "outcome": "fail",
            "reason": f"the run failed: {type(err).__name__}: {err}",
            "note": "Not retried by default. LangChain documents ModelRetryMiddleware (or a model's own "
            "max_retries) to retry model calls.",
        }
    ok = state["messages"][-1].content == ANSWER
    return {"outcome": "pass" if ok else "fail", "reason": "the run retried the model call and answered"}


async def rel_tool_exception() -> Dict[str, Any]:
    def broken(name: str, order_id: Any, delay: float, counter: Optional[Counter]) -> dict:
        raise RuntimeError("database is down")

    graph = create_agent(ScriptedModel(mode="tools", tools=1), tools=[_tool("get_order", 0, None, broken)])
    try:
        state = await graph.ainvoke({"messages": [HumanMessage(PROMPT)]})
    except Exception as err:  # noqa: BLE001
        return {
            "outcome": "fail",
            "reason": f"the run stopped with {type(err).__name__}: {err}",
            "note": "By default the tool node re-raises exceptions other than invalid arguments. "
            "ToolNode(handle_tool_errors=True) or a tool's handle_tool_error sends them to the model.",
        }
    ok = state["messages"][-1].content == ANSWER
    return {"outcome": "pass" if ok else "partial", "reason": "the model saw the error and answered"}


async def rel_bad_arguments() -> Dict[str, Any]:
    counter = Counter()
    graph = create_agent(ScriptedModel(mode="tools", tools=1, bad_arguments=True), tools=[_tool("get_order", 0, counter)])
    try:
        state = await graph.ainvoke({"messages": [HumanMessage(PROMPT)]})
    except Exception as err:  # noqa: BLE001
        return {"outcome": "fail", "reason": f"the run failed: {type(err).__name__}: {err}"}
    if counter.calls:
        return {"outcome": "fail", "reason": f"the tool ran with bad arguments: {counter.calls}"}
    tool_messages = [m for m in state["messages"] if isinstance(m, ToolMessage)]
    if state["messages"][-1].content == ANSWER:
        return {
            "outcome": "pass",
            "reason": "the arguments were rejected before the tool ran; the model got the error and answered",
            "log": str(tool_messages[:1])[:1500],
        }
    return {"outcome": "partial", "reason": "the tool did not run, but there was no answer"}


async def rel_runaway_loop() -> Dict[str, Any]:
    counter = Counter()
    model = ScriptedModel(mode="runaway")
    graph = create_agent(model, tools=[_tool("get_order", 0, counter)])
    started = time.perf_counter()
    try:
        await asyncio.wait_for(graph.ainvoke({"messages": [HumanMessage(PROMPT)]}), timeout=300)
    except asyncio.TimeoutError:
        return {"outcome": "fail", "reason": f"still running after 5 minutes ({model.calls} model calls)"}
    except Exception as err:  # noqa: BLE001
        took = (time.perf_counter() - started) * 1000
        return {
            "outcome": "pass" if model.calls <= 100 else "partial",
            "reason": f"stopped after {model.calls} model calls and {len(counter.calls)} tool calls in {took:.0f} ms: "
            f"{type(err).__name__}",
            "note": "create_agent sets recursion_limit=9999; pass a lower recursion_limit in the run config "
            "to stop earlier." if model.calls > 100 else None,
            "log": str(err)[:1500],
        }
    return {"outcome": "fail", "reason": "the run ended without an error"}


# ---------- crash and resume ----------


def _crash_graph(work: Path, saver: Any) -> Any:
    log = work / "calls.log"

    def get_order(order_id: str) -> dict:
        """Get order for an order."""
        note(log, "get_order done")
        return {"order": order_id, "status": "shipped"}

    def get_invoice(order_id: str) -> dict:
        """Get invoice for an order."""
        note(log, "get_invoice started")
        if (work / "slow").exists():
            time.sleep(60)
        note(log, "get_invoice done")
        return {"invoice": order_id, "amount": 40}

    class Two(ScriptedModel):
        def _reply(self, messages: List[BaseMessage]) -> AIMessage:
            self.calls += 1
            if any(isinstance(m, ToolMessage) for m in messages):
                return AIMessage(content=ANSWER)
            return AIMessage(
                content="",
                tool_calls=[
                    {"name": "get_order", "args": {"order_id": ORDER}, "id": "call_1"},
                    {"name": "get_invoice", "args": {"order_id": ORDER}, "id": "call_2"},
                ],
            )

    return create_agent(Two(), tools=[get_order, get_invoice], checkpointer=saver)


async def crash_phase(phase: int, work: Path) -> Dict[str, Any]:
    try:
        from langgraph.checkpoint.sqlite.aio import AsyncSqliteSaver
    except ImportError:
        return {
            "outcome": "fail",
            "reason": "langgraph-checkpoint-sqlite is not installed",
        }
    config = {"configurable": {"thread_id": "ticket-7"}}
    async with AsyncSqliteSaver.from_conn_string(str(work / "langgraph.db")) as saver:
        graph = _crash_graph(work, saver)
        if phase == 1:
            await graph.ainvoke({"messages": [HumanMessage(PROMPT)]}, config)
            return {"outcome": "fail", "reason": "phase 1 was not killed"}
        # LangGraph's documented resume: invoke the thread again with no input.
        state = await graph.ainvoke(None, config)
    return {"answer": state["messages"][-1].content, "how": "ainvoke(None) on the same thread, AsyncSqliteSaver"}

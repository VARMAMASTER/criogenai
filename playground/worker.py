"""One framework, one job, in its own process (the playground's worker).

    <framework's venv python> playground/worker.py '<json job>'

It prints one JSON object per line on stdout: what happened and when. The
controller (server.py) reads those lines and, from outside, samples this
process's CPU and memory, so the numbers include everything the framework
costs (the interpreter, imports, its HTTP client, its threads).

The comparison is fair by construction:

- **The same model server.** Both frameworks talk to the same local fake model
  (mock_model.py, OpenAI wire format) with their own real clients: each builds
  the request, sends it over HTTP, parses the reply and extracts the tool calls
  itself, as with a hosted model. Only the model's answers are scripted.
- **The same tools**: plain synchronous functions that wait a fixed time (a
  database call), with the same name, argument and description.
- **The same agent**: each framework's standard way to build a tool-calling
  agent, with its defaults (LangChain's `create_agent`, AgentBuild's `Agent`).
- **The same work, the same order of events**, measured from outside the
  process by the same code.

Job fields: framework ("agentbuild" | "langchain"), message, tools (0-5),
model_delay_ms, tool_delay_ms, runs, concurrency, mock_url.
"""

import asyncio
import json
import sys
import time

T0 = time.perf_counter()  # the process's start


def emit(**event):
    sys.stdout.write(json.dumps(event, default=str) + "\n")
    sys.stdout.flush()


TOOLS = ["get_order", "get_customer", "check_policy", "get_shipping", "get_invoice"]
ORDER = "A-1001"

# Events of the tools are reported for the first measured run only, so the
# reporting itself costs both frameworks the same and almost nothing.
tracing = False
run_started = 0.0


def tool_body(name: str, delay: float) -> dict:
    """What every tool does: wait `delay` seconds (a database call), then
    answer. Plain synchronous code, as most tools are written."""
    if tracing:
        start = (time.perf_counter() - run_started) * 1000
        time.sleep(delay)
        emit(e="tool", name=name, start_ms=round(start, 2), end_ms=round((time.perf_counter() - run_started) * 1000, 2))
    else:
        time.sleep(delay)
    return {"tool": name, "order_id": ORDER, "ok": True}


def description(name: str) -> str:
    return f"{name.replace('_', ' ').capitalize()} for an order."


# ---------------------------------------------------------------- AgentBuild


def build_agentbuild(job):
    from agentbuild import Agent, OpenAICompatible, __version__, tool

    delay = job["tool_delay_ms"] / 1000

    def make(name):
        def call(order_id: str) -> dict:
            return tool_body(name, delay)

        call.__name__ = name
        call.__doc__ = description(name)
        return tool(call)

    agent = Agent(
        OpenAICompatible(f"mock-{job['model_delay_ms']}", base_url=job["mock_url"]),
        tools=[make(n) for n in TOOLS[: job["tools"]]],
    )

    async def run():
        return (await agent.run(job["message"])).text

    return run, __version__


# ----------------------------------------------------------------- LangChain


def build_langchain(job):
    from importlib.metadata import version

    from langchain.agents import create_agent
    from langchain_core.messages import HumanMessage
    from langchain_core.tools import StructuredTool
    from langchain_openai import ChatOpenAI

    delay = job["tool_delay_ms"] / 1000

    def make(name):
        def call(order_id: str) -> dict:
            return tool_body(name, delay)

        return StructuredTool.from_function(call, name=name, description=description(name))

    model = ChatOpenAI(model=f"mock-{job['model_delay_ms']}", base_url=job["mock_url"], api_key="not-a-real-key")
    graph = create_agent(model, tools=[make(n) for n in TOOLS[: job["tools"]]])

    async def run():
        state = await graph.ainvoke({"messages": [HumanMessage(job["message"])]})
        return state["messages"][-1].content

    return run, f"langchain {version('langchain')} / langgraph {version('langgraph')} / langchain-openai {version('langchain-openai')}"


# ---------------------------------------------------------------------- main


async def work(job):
    global tracing, run_started
    builders = {"agentbuild": build_agentbuild, "langchain": build_langchain}
    run, version = builders[job["framework"]](job)
    # "Ready": the framework imported and the agent built (imports are most of it).
    emit(e="start", version=version, ready_ms=round((time.perf_counter() - T0) * 1000, 2))

    # The first run is the cold one (first use of everything): reported
    # apart, with its tools timeline and its answer.
    tracing, run_started = True, time.perf_counter()
    answer = await run()
    cold = (time.perf_counter() - run_started) * 1000
    tracing = False
    emit(e="first_run", ms=round(cold, 2), answer=answer)

    runs, concurrency = job["runs"], job["concurrency"]
    if runs <= 0:
        emit(e="done", runs=[], wall_ms=0)
        return
    semaphore = asyncio.Semaphore(concurrency)
    samples = []

    async def one(i):
        async with semaphore:
            started = time.perf_counter()
            await run()
            ms = (time.perf_counter() - started) * 1000
            samples.append(ms)
            emit(e="run", i=i, ms=round(ms, 2))

    wall = time.perf_counter()
    await asyncio.gather(*(one(i) for i in range(runs)))
    emit(e="done", runs=[round(s, 2) for s in samples], wall_ms=round((time.perf_counter() - wall) * 1000, 2))


def main():
    job = json.loads(sys.argv[1])
    try:
        asyncio.run(work(job))
    except Exception as error:  # noqa: BLE001 - a framework's failure is a result
        emit(e="error", error=f"{type(error).__name__}: {error}")
        sys.exit(1)


if __name__ == "__main__":
    main()

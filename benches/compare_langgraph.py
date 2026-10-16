"""The same graph shapes as `agentbuild-bench`, in LangGraph (design 0014 §3).

    research/.venv/Scripts/python benches/compare_langgraph.py

Trivial nodes, no model calls: this measures each engine's own cost per
step and per parallel node. LangGraph runs in Python; AgentBuild's numbers
come from its Rust core (an SDK-defined workflow would add the bridge cost
measured by benches/python_bridge.py).
"""

import asyncio
import statistics
import sys
import time
from importlib.metadata import version
from typing import TypedDict

from langgraph.graph import END, START, StateGraph


def chain(n):
    State = TypedDict("State", {f"k{i}": int for i in range(n)}, total=False)
    graph = StateGraph(State)
    for i in range(n):
        graph.add_node(f"n{i}", (lambda i: lambda state: {f"k{i}": i})(i))
        graph.add_edge(START if i == 0 else f"n{i - 1}", f"n{i}")
    graph.add_edge(f"n{n - 1}", END)
    return graph.compile()


def fan_out(n):
    keys = {f"p{i}": int for i in range(n)}
    keys["done"] = bool
    State = TypedDict("State", keys, total=False)
    graph = StateGraph(State)
    graph.add_node("start", lambda state: {})
    graph.add_node("end", lambda state: {"done": True})
    names = []
    for i in range(n):
        name = f"p{i}"
        graph.add_node(name, (lambda i: lambda state: {f"p{i}": i})(i))
        graph.add_edge("start", name)
        names.append(name)
    graph.add_edge(START, "start")
    graph.add_edge(names, "end")
    graph.add_edge("end", END)
    return graph.compile()


def measure(name, per_run, warmup, runs, call):
    for _ in range(warmup):
        call()
    samples = []
    for _ in range(runs):
        started = time.perf_counter()
        call()
        samples.append((time.perf_counter() - started) * 1e6)
    samples.sort()
    median = statistics.median(samples)
    p95 = samples[round((len(samples) - 1) * 0.95)]
    return name, len(samples), median, p95, median / per_run


def show(us):
    return f"{us / 1000:.2f} ms" if us >= 1000 else f"{us:.1f} µs"


def main():
    quick = "quick" in sys.argv
    n = (lambda full: max(full // 10, 5)) if quick else (lambda full: full)
    print(f"LangGraph {version('langgraph')} on Python {sys.version.split()[0]}\n")
    results = []
    c = chain(10)
    results.append(measure("workflow: 10 nodes in a row (invoke)", 10, n(200), n(2000), lambda: c.invoke({})))
    loop = asyncio.new_event_loop()
    results.append(
        measure("workflow: 10 nodes in a row (ainvoke)", 10, n(200), n(2000), lambda: loop.run_until_complete(c.ainvoke({})))
    )
    f = fan_out(100)
    results.append(measure("workflow: 100 parallel nodes + join (invoke)", 102, n(50), n(500), lambda: f.invoke({})))
    results.append(
        measure(
            "workflow: 100 parallel nodes + join (ainvoke)", 102, n(50), n(500), lambda: loop.run_until_complete(f.ainvoke({}))
        )
    )
    print("| Scenario | Runs | Median per run | p95 per run | Median per unit |")
    print("|---|---|---|---|---|")
    for name, runs, median, p95, unit in results:
        print(f"| {name} | {runs} | {show(median)} | {show(p95)} | {show(unit)} per {'step' if 'row' in name else 'node'} |")


main()

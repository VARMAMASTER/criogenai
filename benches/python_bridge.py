"""What the Python SDK's bridge costs (design 0014 §2): agent runs with an
instant simulated model, with and without one Python tool call.

    python benches/python_bridge.py [quick]   (with the criogen wheel installed)
"""

import asyncio
import statistics
import sys
import time

from criogen import Agent, FakeModel, FakeReply, tool


@tool
def echo_sync(text: str) -> str:
    """Echoes."""
    return text


@tool
async def echo_async(text: str) -> str:
    """Echoes."""
    return text


def show(us):
    return f"{us / 1000:.2f} ms" if us >= 1000 else f"{us:.1f} µs"


async def measure(name, warmup, runs, replies_per_run, script, tools):
    # One scripted model for every run: runs use its replies in order.
    agent = Agent(FakeModel(script * (warmup + runs)), tools=tools)
    for _ in range(warmup):
        await agent.run("go")
    samples = []
    for _ in range(runs):
        started = time.perf_counter()
        await agent.run("go")
        samples.append((time.perf_counter() - started) * 1e6)
    samples.sort()
    return name, runs, statistics.median(samples), samples[round((runs - 1) * 0.95)]


async def main():
    quick = "quick" in sys.argv
    warmup, runs = (20, 200) if quick else (200, 2000)
    call = FakeReply.tool_call
    results = [
        await measure("python: agent run, no tool", warmup, runs, 1, ["done"], []),
        await measure(
            "python: agent run, 1 sync Python tool", warmup, runs, 2, [call("echo_sync", {"text": "x"}), "done"], [echo_sync]
        ),
        await measure(
            "python: agent run, 1 async Python tool", warmup, runs, 2, [call("echo_async", {"text": "x"}), "done"], [echo_async]
        ),
    ]
    print(f"Python {sys.version.split()[0]}\n")
    print("| Scenario | Runs | Median per run | p95 per run |")
    print("|---|---|---|---|")
    for name, n, median, p95 in results:
        print(f"| {name} | {n} | {show(median)} | {show(p95)} |")


asyncio.run(main())

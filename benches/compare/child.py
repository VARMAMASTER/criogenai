"""Runs one framework through one scenario or test, in that framework's
venv, and prints the result (design 0016 §1: one process per measurement).

    <venv python> benches/compare/child.py <framework> <task> '<json args>'

Tasks: a scenario id of §2 (`agent_call`, …, `concurrent`), `rel_<test>`
of §3, `crash` (args: phase, work) and `probe` (the model adapter's own
cost). Called by run.py.
"""

import asyncio
import importlib
import json
import sys
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE / "frameworks")]

from common import ANSWER, Counter, emit, model_cost_us, quiet, rss_mb, time_async, time_batch, time_sync  # noqa: E402

quiet()

# What each scenario runs (design 0016 §2).
AGENT = {
    "agent_call": {"mode": "answer", "tools": 0, "model_delay": 0.0, "tool_delay": 0.0},
    "agent_tools": {"mode": "tools", "tools": 5, "model_delay": 0.0, "tool_delay": 0.0},
    "agent_latency": {"mode": "tools", "tools": 5, "model_delay": 0.2, "tool_delay": 0.3},
    "concurrent": {"mode": "tools", "tools": 1, "model_delay": 0.1, "tool_delay": 0.05},
}
WORKFLOW = {
    "workflow_chain": ("chain", {"n": 10}),
    "workflow_fanout": ("fanout", {"n": 20, "delay": 0.1}),
}


def call_once(call, is_async):
    return asyncio.run(call()) if is_async else call()


def check_agent(fw, spec):
    """One run with a counter: the right answer, and every tool ran once."""
    counter = Counter()
    call, is_async = fw.agent(spec["mode"], 0.0, 0.0, counter, spec["tools"])
    answer = call_once(call, is_async)
    problems = []
    if answer != ANSWER:
        problems.append(f"answer was {answer!r}")
    if len(counter.calls) != spec["tools"]:
        problems.append(f"{len(counter.calls)} tool calls instead of {spec['tools']}: {counter.calls}")
    return problems


def scenario(fw, task, args):
    runs, warmup = args["runs"], args["warmup"]
    if task in AGENT:
        spec = AGENT[task]
        problems = check_agent(fw, spec)
        if problems:
            return {"status": "error", "error": "; ".join(problems)}
        if task == "concurrent":
            call, is_async = fw.agent(spec["mode"], spec["model_delay"], spec["tool_delay"], None, spec["tools"], concurrent=True)
            baseline = rss_mb()
            if not is_async:
                return {"status": "error", "error": "no async entry point for concurrent runs"}
            return asyncio.run(time_batch(call, args["n"], baseline))
        call, is_async = fw.agent(spec["mode"], spec["model_delay"], spec["tool_delay"], None, spec["tools"])
    else:
        builder, params = WORKFLOW[task]
        made = getattr(fw, builder)(**params)
        if made is None:
            return {"status": "unsupported", "note": getattr(fw, "UNSUPPORTED", {}).get(task)}
        call, is_async = made
    baseline = rss_mb()
    if is_async:
        return asyncio.run(time_async(call, runs, warmup, baseline))
    return time_sync(call, runs, warmup, baseline)


def main():
    name, task = sys.argv[1], sys.argv[2]
    args = json.loads(sys.argv[3]) if len(sys.argv) > 3 else {}
    started = time.perf_counter()
    fw = importlib.import_module(f"fw_{name}")
    result = {"framework": name, "task": task, "import_ms": round((time.perf_counter() - started) * 1000, 2)}
    try:
        if task == "info":
            result.update(fw.info())
        elif task == "probe":
            probe = fw.model_probe()
            result["model_us"] = model_cost_us(probe) if probe else None
        elif task.startswith("rel_"):
            outcome = getattr(fw, task)()
            result.update(asyncio.run(outcome) if asyncio.iscoroutine(outcome) else outcome)
        elif task == "crash":
            outcome = fw.crash_phase(args["phase"], Path(args["work"]))
            result.update(asyncio.run(outcome) if asyncio.iscoroutine(outcome) else outcome)
        else:
            result.update(scenario(fw, task, args))
    except Exception as err:  # noqa: BLE001 - a framework's failure is a result
        result.update(status="error", outcome="fail", error=f"{type(err).__name__}: {err}", log=traceback.format_exc()[-4000:])
    emit(result)


if __name__ == "__main__":
    main()

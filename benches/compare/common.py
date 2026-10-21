"""Shared by every framework adapter (design 0016): the scripted model's
decisions, the tool bodies, timing and resource readings. Standard library
plus psutil, so it runs in each framework's venv."""

import json
import os
import statistics
import sys
import threading
import time
from typing import Any, Callable, Dict, List, Optional

import psutil

#: The five tools of the agent scenarios, in the order the model asks for them.
TOOLS = ["get_order", "get_customer", "check_policy", "get_shipping", "get_invoice"]
ORDER = "A-1001"
ANSWER = "Your refund of 40.00 is on its way."


class Script:
    """What the simulated model says, the same for every framework.

    ``mode``:
      - ``answer``: answers at once;
      - ``tools``: asks for ``len(tools)`` tools (all in one turn, or one per
        turn where the framework only takes one), then answers;
      - ``runaway``: asks for ``get_order`` forever.
    """

    def __init__(self, mode: str, tools: int = 5, per_turn: bool = True):
        self.mode = mode
        self.tools = TOOLS[:tools]
        self.per_turn = per_turn

    def next(self, tool_results: int) -> Dict[str, Any]:
        """``{"answer": text}`` or ``{"calls": [(name, args)]}``, given how
        many tool results the conversation has so far."""
        if self.mode == "runaway":
            return {"calls": [("get_order", {"order_id": ORDER})]}
        if self.mode == "answer" or tool_results >= len(self.tools):
            return {"answer": ANSWER}
        if self.per_turn:
            return {"calls": [(name, {"order_id": ORDER}) for name in self.tools]}
        return {"calls": [(self.tools[tool_results], {"order_id": ORDER})]}


class Counter:
    """Counts tool calls (thread-safe), for the reliability checks."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.calls: List[str] = []

    def add(self, name: str) -> None:
        with self.lock:
            self.calls.append(name)


def tool_body(name: str, order_id: Any, delay: float, counter: Optional[Counter] = None) -> Dict[str, Any]:
    """What every tool does: wait `delay` (I/O, like a database call), then
    answer. Plain synchronous code, as most tools are written."""
    if counter is not None:
        counter.add(name)
    if delay:
        time.sleep(delay)
    return {"tool": name, "order_id": order_id, "ok": True}


_LOG_LOCK = threading.Lock()


def note(path: Any, line: str) -> None:
    """Appends a line to a log file shared by tools running in parallel
    (the crash test's evidence). Appends from several threads are not
    atomic on every platform, so one lock serialises them."""
    with _LOG_LOCK, open(path, "a", encoding="utf-8") as f:
        f.write(line + "\n")
        f.flush()


# ---------- measurement ----------


def _peak_rss_mb() -> float:
    info = psutil.Process().memory_info()
    peak = getattr(info, "peak_wset", None)  # Windows
    if peak is None:
        import resource

        peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        peak = peak if sys.platform == "darwin" else peak * 1024
    return peak / 2**20


def rss_mb() -> float:
    return psutil.Process().memory_info().rss / 2**20


def cpu_s() -> float:
    times = psutil.Process().cpu_times()
    return times.user + times.system


def pick(samples: List[float], limit: int = 200) -> List[float]:
    if len(samples) <= limit:
        return [round(s, 4) for s in samples]
    step = len(samples) / limit
    return [round(samples[int(i * step)], 4) for i in range(limit)]


def summary(samples_ms: List[float]) -> Dict[str, Any]:
    ordered = sorted(samples_ms)
    return {
        "median_ms": round(statistics.median(ordered), 4),
        "p95_ms": round(ordered[round((len(ordered) - 1) * 0.95)], 4),
        "min_ms": round(ordered[0], 4),
        "max_ms": round(ordered[-1], 4),
        "samples_ms": pick(samples_ms),
    }


def time_sync(call: Callable[[], Any], runs: int, warmup: int, baseline: float) -> Dict[str, Any]:
    for _ in range(warmup):
        call()
    samples = []
    cpu0, wall0 = cpu_s(), time.perf_counter()
    for _ in range(runs):
        started = time.perf_counter()
        call()
        samples.append((time.perf_counter() - started) * 1000)
    wall, cpu = time.perf_counter() - wall0, cpu_s() - cpu0
    return {
        "status": "ok", "runs": runs, **summary(samples),
        "wall_ms": round(wall * 1000, 3), "cpu_ms": round(cpu * 1000 / runs, 4),
        "baseline_rss_mb": round(baseline, 2), "peak_rss_mb": round(_peak_rss_mb(), 2),
    }


async def time_async(call: Callable[[], Any], runs: int, warmup: int, baseline: float) -> Dict[str, Any]:
    for _ in range(warmup):
        await call()
    samples = []
    cpu0, wall0 = cpu_s(), time.perf_counter()
    for _ in range(runs):
        started = time.perf_counter()
        await call()
        samples.append((time.perf_counter() - started) * 1000)
    wall, cpu = time.perf_counter() - wall0, cpu_s() - cpu0
    return {
        "status": "ok", "runs": runs, **summary(samples),
        "wall_ms": round(wall * 1000, 3), "cpu_ms": round(cpu * 1000 / runs, 4),
        "baseline_rss_mb": round(baseline, 2), "peak_rss_mb": round(_peak_rss_mb(), 2),
    }


async def time_batch(call: Callable[[], Any], n: int, baseline: float) -> Dict[str, Any]:
    """N runs at once on one event loop; each run's latency, the batch's
    wall time, CPU and memory."""
    import asyncio

    async def one() -> float:
        started = time.perf_counter()
        await call()
        return (time.perf_counter() - started) * 1000

    cpu0, wall0 = cpu_s(), time.perf_counter()
    samples = await asyncio.gather(*(one() for _ in range(n)))
    wall, cpu = time.perf_counter() - wall0, cpu_s() - cpu0
    peak = _peak_rss_mb()
    return {
        "status": "ok", "runs": n, **summary(list(samples)),
        "wall_ms": round(wall * 1000, 3), "cpu_ms": round(cpu * 1000, 3),
        "baseline_rss_mb": round(baseline, 2), "peak_rss_mb": round(peak, 2),
        "rss_per_run_kb": round(max(0.0, peak - baseline) * 1024 / n, 2),
    }


def model_cost_us(call: Callable[[], Any], runs: int = 2000) -> float:
    """One call of the simulated-model adapter on its own (design 0016 §4,
    `model_us`)."""
    for _ in range(100):
        call()
    started = time.perf_counter()
    for _ in range(runs):
        call()
    return round((time.perf_counter() - started) * 1e6 / runs, 3)


def emit(result: Dict[str, Any]) -> None:
    """The child's answer: one line on stdout, marked, after whatever the
    framework printed."""
    sys.stdout.write("\nRESULT " + json.dumps(result, default=str) + "\n")
    sys.stdout.flush()


def quiet() -> None:
    """No telemetry, tracing or network from any framework while measured."""
    for key, value in {
        "CREWAI_DISABLE_TELEMETRY": "true",
        "CREWAI_TRACING_ENABLED": "false",
        "OTEL_SDK_DISABLED": "true",
        "LANGCHAIN_TRACING_V2": "false",
        "LANGSMITH_TRACING": "false",
        "ANONYMIZED_TELEMETRY": "False",
    }.items():
        os.environ.setdefault(key, value)

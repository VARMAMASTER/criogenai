"""Compares Criogen, LangGraph and CrewAI (design 0016) and writes the
results file the website shows.

    python benches/compare/setup.py           # once: one venv per framework
    python benches/compare/run.py             # everything → bench/results/<date>T<time>-<machine>.json
    (in the downloaded kit: python setup.py, then python run.py)
    python benches/compare/run.py quick       # fewer runs, a smoke test (not for publishing)
    python benches/compare/run.py --only crewai --skip load

Standard library only: every measurement runs in a child process in the
framework's own venv (child.py).
"""

import argparse
import datetime
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
# The repository root, or the kit's own folder when run from the download.
from setup import ROOT, VENVS, python_of  # noqa: E402

FRAMEWORKS = ["criogen", "langgraph", "crewai"]

SCENARIOS = [
    {"id": "agent_call", "kind": "speed", "title": "Agent, one model call",
     "detail": "One model call, no tools, the model answers at once: the framework's cost per agent turn.", "unit": "run"},
    {"id": "agent_tools", "kind": "speed", "title": "Agent with 5 tool calls",
     "detail": "The model asks for 5 tools, then answers; model and tools answer at once: the cost of a turn with tools.", "unit": "run"},
    {"id": "agent_latency", "kind": "speed", "title": "Agent with realistic delays",
     "detail": "As above, with 200 ms per model call and 300 ms of I/O per tool: about 0.7 s if the tools run at once, 1.9 s one by one.", "unit": "run"},
    {"id": "workflow_chain", "kind": "speed", "title": "Workflow: 10 steps in a row",
     "detail": "Ten trivial Python nodes in a row: the cost per step.", "unit": "run"},
    {"id": "workflow_fanout", "kind": "speed", "title": "Workflow: 20 parallel nodes",
     "detail": "Twenty Python nodes of 100 ms I/O each, then a join: about 0.1 s if they run at once.", "unit": "run"},
    {"id": "concurrent_1", "kind": "load", "n": 1, "title": "1 agent run",
     "detail": "One run (model 100 ms per call, one tool of 50 ms): the baseline for the load tests.", "unit": "batch"},
    {"id": "concurrent_100", "kind": "load", "n": 100, "title": "100 agent runs at once",
     "detail": "100 runs at the same time in one process: wall time, CPU and memory.", "unit": "batch"},
    {"id": "concurrent_1000", "kind": "load", "n": 1000, "title": "1000 agent runs at once",
     "detail": "1000 runs at the same time in one process: wall time, CPU and memory.", "unit": "batch"},
]

RELIABILITY = ["model_transient", "tool_exception", "bad_arguments", "crash_resume", "runaway_loop"]

RUNS = {  # (runs, warm-up); quick: a tenth
    "agent_call": (300, 30), "agent_tools": (200, 20), "agent_latency": (5, 1),
    "workflow_chain": (300, 30), "workflow_fanout": (5, 1),
}

# Load batches are short and CPU-bound, so one batch is noisy on a busy
# machine: each runs this many times, each in a fresh process, and the batch
# with the median wall time is reported (all wall times in batch_walls_ms).
LOAD_REPEATS = 3


def load_batches(fw, n, repeats):
    batches = [child(fw, "concurrent", {"n": n, "runs": 1, "warmup": 0}) for _ in range(repeats)]
    ok = [b for b in batches if b.get("status", "ok") == "ok" and b.get("wall_ms") is not None]
    if not ok:
        return batches[0]
    ok.sort(key=lambda b: b["wall_ms"])
    chosen = dict(ok[len(ok) // 2])
    chosen["batch_walls_ms"] = [b["wall_ms"] for b in ok]
    return chosen


IMPORTS = {
    "criogen": "import criogen",
    "langgraph": "from langchain.agents import create_agent; from langgraph.graph import StateGraph",
    "crewai": "from crewai import Agent, Crew, Task; from crewai.flow.flow import Flow",
}

QUIET_ENV = {
    "CREWAI_DISABLE_TELEMETRY": "true", "CREWAI_TRACING_ENABLED": "false", "OTEL_SDK_DISABLED": "true",
    "LANGCHAIN_TRACING_V2": "false", "LANGSMITH_TRACING": "false", "ANONYMIZED_TELEMETRY": "False",
    "PYTHONIOENCODING": "utf-8",
}


def scrub(text):
    """Logs are published: the home folder (and so the user name) becomes ~."""
    home = str(Path.home())
    for form in {home, home.replace("\\", "/"), home.replace("\\", "\\\\")}:
        text = text.replace(form, "~")
    return text


def env():
    return {**os.environ, **QUIET_ENV}


def child(fw, task, args=None, timeout=900):
    cmd = [str(python_of(VENVS / fw)), str(HERE / "child.py"), fw, task, json.dumps(args or {})]
    try:
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env(), encoding="utf-8", errors="replace")
    except subprocess.TimeoutExpired:
        return {"status": "error", "outcome": "fail", "error": f"timed out after {timeout} s"}
    for line in reversed(done.stdout.splitlines()):
        if line.startswith("RESULT "):
            return json.loads(line[len("RESULT "):])
    tail = (done.stdout[-1500:] + "\n" + done.stderr[-2500:]).strip()
    return {"status": "error", "outcome": "fail", "error": f"no result (exit {done.returncode})", "log": tail}


def cpu_name():
    try:
        if sys.platform == "win32":
            import winreg

            key = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
            return winreg.QueryValueEx(key, "ProcessorNameString")[0].strip()
        if sys.platform == "darwin":
            return subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except Exception:  # noqa: BLE001
        pass
    return platform.processor() or "unknown"


def machine():
    ram = None
    try:
        if sys.platform == "win32":
            import ctypes

            class Status(ctypes.Structure):
                _fields_ = [("length", ctypes.c_ulong), ("load", ctypes.c_ulong)] + [
                    (n, ctypes.c_ulonglong) for n in ("total", "avail", "tpage", "apage", "tvirt", "avirt", "aext")
                ]

            status = Status(length=ctypes.sizeof(Status))
            ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status))
            ram = status.total
        elif sys.platform == "darwin":
            ram = int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True))
        else:
            ram = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except Exception:  # noqa: BLE001
        pass
    system = f"{platform.system()} {platform.release()}"
    if sys.platform == "win32" and int(platform.version().split(".")[-1]) >= 22000:
        system = "Windows 11"
    return {
        "cpu": cpu_name(),
        "threads": os.cpu_count(),
        "ram_gb": round(ram / 2**30, 1) if ram else None,
        "os": system,
    }


# ---------- footprint ----------


def site_size(venv):
    return sum(path.stat().st_size for path in venv.rglob("*") if path.is_file())


def packages(venv):
    out = subprocess.run([str(python_of(venv)), "-m", "pip", "list", "--format", "json", "--disable-pip-version-check"],
                         capture_output=True, text=True, env=env())
    try:
        return {p["name"].lower() for p in json.loads(out.stdout)}
    except json.JSONDecodeError:
        return set()


def import_ms(python, statement, times):
    def once(code):
        started = time.perf_counter()
        subprocess.run([str(python), "-c", code], check=True, env=env(), capture_output=True)
        return (time.perf_counter() - started) * 1000

    once(statement)  # warm the file cache
    empty = statistics.median(once("pass") for _ in range(times))
    full = statistics.median(once(statement) for _ in range(times))
    return round(full - empty, 1)


def footprint(fw, times):
    bare = VENVS / "_bare"
    if not python_of(bare).exists():
        subprocess.run([sys.executable, "-m", "venv", str(bare)], check=True)
    venv = VENVS / fw
    base = packages(bare)
    extra = packages(venv) - base - {"psutil"}  # psutil: the benchmark's own, in every venv
    size = site_size(venv) - site_size(bare)
    return {
        "framework": fw,
        "import_ms": import_ms(python_of(venv), IMPORTS[fw], times),
        "packages": len(extra),
        "install_mb": round(size / 2**20, 1),
    }


# ---------- crash and resume ----------


def crash_test(fw):
    work = ROOT / "target" / "compare" / "crash" / fw
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    (work / "slow").write_text("1")
    log = work / "calls.log"
    cmd = [str(python_of(VENVS / fw)), str(HERE / "child.py"), fw, "crash", json.dumps({"phase": 1, "work": str(work)})]
    first = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env())
    deadline = time.time() + 120
    while time.time() < deadline and first.poll() is None:
        if log.exists() and "get_invoice started" in log.read_text():
            break
        time.sleep(0.05)
    if first.poll() is not None:
        out = first.stdout.read().decode("utf-8", "replace")[-2000:] + first.stderr.read().decode("utf-8", "replace")[-2000:]
        return {"outcome": "fail", "reason": "phase 1 ended before the second tool started", "log": out}
    time.sleep(1.0)  # the same grace for everyone to save what finished
    first.kill()
    first.wait()
    before = log.read_text().splitlines()
    (work / "slow").unlink()
    second = child(fw, "crash", {"phase": 2, "work": str(work)}, timeout=180)
    after = log.read_text().splitlines()
    record = f"phase 1 (killed): {before}\nphase 2: {after[len(before):]}\nphase 2 said: {json.dumps(second)[:1500]}"
    if second.get("outcome") or second.get("answer") is None:
        return {"outcome": second.get("outcome", "fail"), "reason": second.get("reason") or second.get("error", "did not resume"),
                "note": second.get("note"), "log": record}
    orders = after.count("get_order done")
    if "get_invoice done" not in after[len(before):]:
        return {"outcome": "fail", "reason": "resumed, but the interrupted tool did not finish", "log": record}
    if orders == 1:
        return {"outcome": "pass", "reason": f"resumed with {second.get('how')}; the finished tool did not run again", "log": record}
    return {"outcome": "partial", "reason": f"resumed with {second.get('how')}, but the finished tool ran again ({orders} times)",
            "note": second.get("note"), "log": record}


# ---------- main ----------


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", nargs="?", choices=["quick"], help="fewer runs (a smoke test)")
    parser.add_argument("--only", choices=FRAMEWORKS, action="append", help="these frameworks only")
    parser.add_argument("--skip", choices=["speed", "load", "reliability", "footprint"], action="append", default=[])
    parser.add_argument("--out", help="the results file (default bench/results/<date>-<machine>.json)")
    args = parser.parse_args()
    quick = args.mode == "quick"
    frameworks = args.only or FRAMEWORKS
    for fw in frameworks:
        if not python_of(VENVS / fw).exists():
            sys.exit(f"no venv for {fw}: run `python benches/compare/setup.py` first")

    report = {
        "schema": "criogen-compare/1",
        "recorded": datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "command": "python benches/compare/run.py" + (" quick" if quick else ""),
        "quick": quick,
        "machine": machine(),
        "python": platform.python_version(),
        "frameworks": {},
        "scenarios": SCENARIOS,
        "reliability_tests": RELIABILITY,
        "results": [],
        "reliability": [],
        "footprint": [],
    }
    for fw in frameworks:
        info = child(fw, "info")
        probe = child(fw, "probe")
        report["frameworks"][fw] = {
            "label": info.get("label", fw), "version": info.get("version"), "also": info.get("also"),
            "model_us": probe.get("model_us"), "notes": info.get("notes"),
        }
        print(f"{fw}: {info.get('label')} {info.get('version')}", flush=True)

    for sc in SCENARIOS:
        if sc["kind"] not in args.skip:
            for fw in frameworks:
                if sc["kind"] == "speed":
                    runs, warmup = RUNS[sc["id"]]
                    if quick:
                        runs, warmup = max(2, runs // 10), max(1, warmup // 10)
                    result = child(fw, sc["id"], {"runs": runs, "warmup": warmup})
                else:
                    if quick and sc["n"] > 100:
                        continue
                    result = load_batches(fw, sc["n"], 1 if quick else LOAD_REPEATS)
                result.update(scenario=sc["id"], framework=fw)
                for key in ("error", "log", "note"):
                    if isinstance(result.get(key), str):
                        result[key] = scrub(result[key])
                for key in ("task", "import_ms"):
                    result.pop(key, None)
                result.setdefault("status", "ok")
                report["results"].append(result)
                shown = result.get("median_ms") if sc["kind"] == "speed" else result.get("wall_ms")
                print(f"  {sc['id']:<16} {fw:<11} {result['status']:<11} {shown} ms {result.get('error') or ''}", flush=True)

    if "reliability" not in args.skip:
        for test in RELIABILITY:
            for fw in frameworks:
                result = crash_test(fw) if test == "crash_resume" else child(fw, f"rel_{test}", timeout=300)
                entry = {"test": test, "framework": fw, "outcome": result.get("outcome", "fail"),
                         "reason": scrub(result.get("reason") or result.get("error") or ""), "note": result.get("note"),
                         "log": scrub((result.get("log") or "")[-4000:])}
                report["reliability"].append(entry)
                print(f"  {test:<16} {fw:<11} {entry['outcome']:<8} {entry['reason']}", flush=True)

    if "footprint" not in args.skip:
        for fw in frameworks:
            entry = footprint(fw, 3 if quick else 15)
            report["footprint"].append(entry)
            print(f"  footprint {fw:<11} import {entry['import_ms']} ms, {entry['packages']} packages, {entry['install_mb']} MB", flush=True)

    host = re.sub(r"[^a-z0-9]+", "-", report["machine"]["cpu"].lower()).strip("-")[:40]
    # Date and time in the name: a later run never overwrites an earlier one.
    stamp = report["recorded"][:16].replace(":", "")
    out = Path(args.out) if args.out else ROOT / "bench" / "results" / f"{stamp}-{host}{'-quick' if quick else ''}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=1), encoding="utf-8")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()

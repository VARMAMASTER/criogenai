"""The playground's controller: one message, two frameworks, measured.

    <python with psutil> playground/server.py        # http://127.0.0.1:4600

For each run it starts one worker process per framework (worker.py, in that
framework's own virtual environment), forwards what they report, and samples
each worker's CPU and memory from outside every 100 ms. Standard library plus
psutil. Binds to this computer only, one run at a time, with limits on
everything the page can ask for.

Environments: target/compare/venvs/<name> (made by benches/compare/setup.py);
`PLAYGROUND_PYTHON_AGENTBUILD` / `PLAYGROUND_PYTHON_LANGCHAIN` override.
"""

import json
import os
import platform
import queue
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import psutil

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
VENVS = ROOT / "target" / "compare" / "venvs"
WORKER = HERE / "worker.py"
STATIC = HERE / "static"

#: framework id -> (label, venv name)
FRAMEWORKS = {"agentbuild": ("AgentBuild", "agentbuild"), "langchain": ("LangChain", "langgraph")}

SAMPLE_EVERY = 0.1  # seconds
MAX_SECONDS = 180  # a run that takes longer is stopped
RUN_LOCK = threading.Lock()  # one run at a time: overlapping runs would disturb the measurements


def kill_tree(process: "subprocess.Popen") -> None:
    """Stops a process and everything it started (on Windows a venv's
    python.exe is a launcher: killing only it would leave the real
    interpreter running)."""
    try:
        parent = psutil.Process(process.pid)
        for child in parent.children(recursive=True):
            child.kill()
        parent.kill()
    except psutil.Error:
        pass


def python_of(framework: str) -> Path:
    override = os.environ.get(f"PLAYGROUND_PYTHON_{framework.upper()}")
    if override:
        return Path(override)
    venv = VENVS / FRAMEWORKS[framework][1]
    windows = venv / "Scripts" / "python.exe"
    return windows if windows.exists() else venv / "bin" / "python"


def clamp(value, low, high, default):
    try:
        number = int(value)
    except (TypeError, ValueError):
        return default
    return max(low, min(high, number))


def read_job(body: dict) -> dict:
    """What the page asked for, checked and limited."""
    message = str(body.get("message") or "").strip()
    if not message:
        raise ValueError("write a message first")
    frameworks = [f for f in body.get("frameworks", list(FRAMEWORKS)) if f in FRAMEWORKS]
    if not frameworks:
        raise ValueError("choose at least one framework")
    for framework in frameworks:
        if not python_of(framework).exists():
            raise ValueError(f"{FRAMEWORKS[framework][0]} is not set up: run `python benches/compare/setup.py`")
    limited = {}  # field -> [asked, used]: a value the page asked for and did not get

    def pick(name, low, high, default):
        value = clamp(body.get(name), low, high, default)
        try:
            asked = int(body.get(name))
        except (TypeError, ValueError):
            asked = value
        if asked != value:
            limited[name] = [asked, value]
        return value

    job = {
        "message": message[:2000],
        "tools": pick("tools", 0, 5, 3),
        "model_delay_ms": pick("model_delay_ms", 0, 5000, 200),
        "tool_delay_ms": pick("tool_delay_ms", 0, 5000, 100),
        "runs": pick("runs", 0, 200, 0),
        "concurrency": pick("concurrency", 1, 100, 1),
        "order": "sequential" if body.get("order") == "sequential" else "parallel",
        "balanced": body.get("balanced", True) is not False,
        "frameworks": frameworks,
        "limited": limited,
    }
    if len(str(body.get("message") or "").strip()) > 2000:
        limited["message"] = [len(str(body["message"]).strip()), 2000]
    return job


class Watched:
    """One worker process: its reports, and a sampler of its CPU and memory."""

    def __init__(self, framework: str, job: dict, events: "queue.Queue", t0: float, pass_no: int = 0):
        self.framework, self.events, self.t0, self.pass_no = framework, events, t0, pass_no
        self.process = subprocess.Popen(
            [str(python_of(framework)), str(WORKER), json.dumps({**job, "framework": framework})],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            env={**os.environ, "PYTHONUNBUFFERED": "1", "OTEL_SDK_DISABLED": "true", "LANGSMITH_TRACING": "false", "LANGCHAIN_TRACING_V2": "false"},
        )
        self.peak_rss = 0.0
        self.cpu_time = 0.0
        self.done = threading.Event()
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._sample, daemon=True).start()

    def put(self, **event):
        self.events.put({"fw": self.framework, "pass": self.pass_no, "t": round((time.perf_counter() - self.t0) * 1000, 1), **event})

    def _read(self):
        for line in self.process.stdout:
            try:
                self.put(**json.loads(line))
            except ValueError:
                continue  # whatever else a framework printed
        code = self.process.wait()
        self.process.stdout.close()
        self.done.set()
        self.put(e="exit", code=code, peak_rss_mb=round(self.peak_rss, 1), cpu_time_s=round(self.cpu_time, 3))

    def _sample(self):
        """CPU and memory of the worker and everything it started. (On
        Windows a virtual environment's python.exe is a small launcher that
        runs the real interpreter as a child: the tree is what counts.)"""
        try:
            root = psutil.Process(self.process.pid)
        except psutil.Error:
            return
        known = {}  # pid -> Process: one object per process, so cpu_percent has a baseline
        spent = {}  # pid -> CPU seconds so far (a process that has exited keeps its share)
        while not self.done.is_set():
            time.sleep(SAMPLE_EVERY)
            try:
                children = root.children(recursive=True)
                # The launcher (the same stub for every framework) is not the
                # framework: when it only starts the real interpreter, measure that.
                stub = len(children) >= 1 and children[0].name() == root.name()
                tree = children if stub else [root, *children]
            except psutil.Error:
                break
            cpu = rss = peak = 0.0
            for process in tree:
                process = known.setdefault(process.pid, process)
                try:
                    with process.oneshot():
                        percent = process.cpu_percent(None)
                        info = process.memory_info()
                        times = process.cpu_times()
                except psutil.Error:
                    continue
                cpu += percent
                rss += info.rss
                peak += getattr(info, "peak_wset", 0)  # Windows keeps the true peak
                spent[process.pid] = times.user + times.system
            rss_mb = rss / 2**20
            self.peak_rss = max(self.peak_rss, rss_mb, peak / 2**20)
            self.cpu_time = sum(spent.values())
            self.put(e="sample", cpu=round(cpu, 1), rss_mb=round(rss_mb, 1))

    def stop(self):
        if self.process.poll() is None:
            kill_tree(self.process)


def start_mock() -> "tuple[subprocess.Popen, str]":
    """The shared fake model server, in its own process (its CPU is not
    charged to either framework). Returns it and its base URL."""
    process = subprocess.Popen(
        [sys.executable, str(HERE / "mock_model.py"), "--port", "0"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )
    line = process.stdout.readline().strip()  # "PORT <n>"
    if not line.startswith("PORT "):
        kill_tree(process)
        raise RuntimeError("the fake model server did not start")
    return process, f"http://127.0.0.1:{line.split()[1]}/v1"


def plan(job: dict) -> list:
    """Which framework runs when. Side by side: all at once. One after the
    other: A, B, B, A, so that going first or second cancels out."""
    if job["order"] == "parallel":
        return [job["frameworks"]]
    order = job["frameworks"]
    return [[f] for f in (order + order[::-1] if job["balanced"] else order)]


def run_job(job: dict, emit) -> None:
    """Runs the job, calling `emit(event)` for each event; stops when the
    workers finish, when `emit` raises (the page went away) or on timeout."""
    events: "queue.Queue" = queue.Queue()
    t0 = time.perf_counter()
    mock, url = start_mock()
    job = {**job, "mock_url": url}
    steps = plan(job)
    emit({"e": "job", "job": {k: v for k, v in job.items() if k != "mock_url"}, "cores": os.cpu_count(), "steps": len(steps)})
    watched = []
    seen = {}  # framework -> how many of its passes have started

    def start_step(frameworks):
        for framework in frameworks:
            pass_no = seen.get(framework, 0)
            seen[framework] = pass_no + 1
            watched.append(Watched(framework, job, events, t0, pass_no))
        return len(frameworks)

    try:
        queue_of_steps = iter(steps)
        running = start_step(next(queue_of_steps))
        deadline = time.perf_counter() + MAX_SECONDS
        while running:
            if time.perf_counter() > deadline:
                emit({"e": "timeout"})
                break
            try:
                event = events.get(timeout=0.5)
            except queue.Empty:
                continue
            emit(event)
            if event.get("e") == "exit":
                running -= 1
                if running == 0:
                    following = next(queue_of_steps, None)
                    if following:
                        running = start_step(following)
    finally:
        for w in watched:
            w.stop()
        kill_tree(mock)
    emit({"e": "end", "ms": round((time.perf_counter() - t0) * 1000, 1)})


def machine() -> dict:
    return {
        "cpu": platform.processor() or platform.machine(),
        "cores": os.cpu_count(),
        "ram_gb": round(psutil.virtual_memory().total / 2**30, 1),
        "os": f"{platform.system()} {platform.release()}",
        "python": platform.python_version(),
    }


# Pages allowed to call this server from a browser: the playground's own page
# (same origin, no header), the website in development and the deployed site.
# Anything else (another site's script) gets 403 before a run can start.
ALLOWED_ORIGINS = {
    "http://localhost:4400", "http://127.0.0.1:4400", "https://agentbuild.vercel.app",
    *filter(None, os.environ.get("PLAYGROUND_ORIGINS", "").split(",")),
}


class Handler(BaseHTTPRequestHandler):
    server_version = "AgentBuildPlayground"

    def _origin_ok(self) -> bool:
        origin = self.headers.get("origin")
        # No header: not a browser. Own page: the origin is this server's own address.
        return origin is None or origin in ALLOWED_ORIGINS or origin == f"http://{self.headers.get('host')}"

    def _cors(self):
        origin = self.headers.get("origin")
        if origin is not None and self._origin_ok():
            self.send_header("access-control-allow-origin", origin)
            self.send_header("vary", "origin")

    def do_OPTIONS(self):
        if not self._origin_ok() or self.headers.get("origin") is None:
            return self._send(403, b'{"error": "origin not allowed"}', "application/json")
        self.send_response(204)
        self._cors()
        self.send_header("access-control-allow-methods", "GET, POST, OPTIONS")
        self.send_header("access-control-allow-headers", "content-type")
        self.send_header("access-control-allow-private-network", "true")  # https page -> this computer
        self.send_header("access-control-max-age", "600")
        self.end_headers()

    def log_message(self, *args):  # quiet
        pass

    def _send(self, status: int, body: bytes, content_type: str):
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.send_header("cache-control", "no-store")
        self._cors()
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if not self._origin_ok():
            return self._send(403, b'{"error": "origin not allowed"}', "application/json")
        path = self.path.split("?")[0]
        if path == "/":
            return self._send(200, (STATIC / "index.html").read_bytes(), "text/html; charset=utf-8")
        if path == "/favicon.ico":
            self.send_response(204)
            self.end_headers()
            return
        if path == "/api/info":
            info = {"machine": machine(), "frameworks": {f: {"label": v[0], "ready": python_of(f).exists()} for f, v in FRAMEWORKS.items()}}
            return self._send(200, json.dumps(info).encode(), "application/json")
        self._send(404, b'{"error": "not found"}', "application/json")

    def do_POST(self):
        if not self._origin_ok():
            return self._send(403, b'{"error": "origin not allowed"}', "application/json")
        if self.path != "/api/run":
            return self._send(404, b'{"error": "not found"}', "application/json")
        try:
            size = int(self.headers.get("content-length") or 0)
            if size > 64 * 1024:
                raise ValueError("too large")
            job = read_job(json.loads(self.rfile.read(size) or b"{}"))
        except (ValueError, TypeError) as error:
            return self._send(400, json.dumps({"error": str(error)}).encode(), "application/json")
        if not RUN_LOCK.acquire(blocking=False):
            return self._send(409, b'{"error": "a run is in progress: one at a time, so measurements stay clean"}', "application/json")
        try:
            # Newline-delimited JSON, ended by closing the connection.
            self.send_response(200)
            self.send_header("content-type", "application/x-ndjson")
            self.send_header("cache-control", "no-store")
            self._cors()
            self.end_headers()

            def emit(event):
                self.wfile.write((json.dumps(event) + "\n").encode())
                self.wfile.flush()

            try:
                run_job(job, emit)
            except (BrokenPipeError, ConnectionError, OSError):
                pass  # the page went away: run_job's finally stopped the workers
        finally:
            RUN_LOCK.release()


def main():
    port = int(os.environ.get("PORT", "4600"))
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"playground on http://127.0.0.1:{port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

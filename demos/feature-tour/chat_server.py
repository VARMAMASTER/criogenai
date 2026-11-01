"""A small chat backend for the website's demo page: a support agent built on
the real Criogen SDK, so visitors can try the features themselves.

    <python with criogen> demos/feature-tour/chat_server.py     # http://127.0.0.1:4700

The agent has tools (look up an order, refund it), pauses before a refund for a
person to decide, remembers facts per visitor, writes an audit trail, saves
every conversation to a SQLite file and guards large refunds with a hook.
Models come from the keys in ~/.criogen-keys.env (read here, never sent to
the browser). The server listens on 127.0.0.1 only and accepts calls from the
website's pages only.
"""

import asyncio
import json
import os
import queue
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from criogen import Agent, Decision, Gemini, Memory, OpenAI, OpenRouter, Retry, Storage, ToolError, tool

PORT = int(os.environ.get("PORT", "4700"))
ALLOWED_ORIGINS = {
    "http://localhost:4400", "http://127.0.0.1:4400", "https://criogen.vercel.app",
    *filter(None, os.environ.get("PLAYGROUND_ORIGINS", "").split(",")),
}

ORDERS = {
    "A-1001": {"item": "Standing desk", "total": 249.00, "status": "shipped", "eta": "tomorrow"},
    "A-1002": {"item": "USB-C hub", "total": 39.50, "status": "processing", "eta": "in 3 days"},
    "A-1003": {"item": "Monitor arm", "total": 89.00, "status": "delivered", "eta": "delivered Monday"},
}


def load_keys() -> None:
    path = Path.home() / ".criogen-keys.env"
    if not path.exists():
        return
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip().removeprefix("export ").strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, value = line.partition("=")
            os.environ.setdefault(key.strip(), value.strip().strip('"').strip("'"))


MODELS = {
    "gemini": ("Gemini 2.5 Flash", "GEMINI_API_KEY", lambda: Gemini("gemini-2.5-flash")),
    "openrouter-free": ("OpenRouter free model (laguna-xs-2.1)", "OPENROUTER_API_KEY", lambda: OpenRouter("poolside/laguna-xs-2.1:free")),
    "openai": ("OpenAI gpt-5-mini", "OPENAI_API_KEY", lambda: OpenAI("gpt-5-mini")),
}


@tool(read_only=True)
def get_order(order_id: str) -> dict:
    """Look up an order by its id (like A-1001): item, total, status and delivery estimate."""
    order = ORDERS.get(order_id.upper())
    if order is None:
        raise ToolError(f"No order {order_id}. Known orders look like A-1001.")
    return {"order_id": order_id.upper(), **order}


@tool(destructive=True)
def issue_refund(order_id: str, amount: float) -> dict:
    """Refund part or all of an order, in dollars. A person approves it first."""
    return {"refunded": amount, "order_id": order_id.upper()}


class RefundGuard:
    """A hook: refunds over 200 never reach a person; the model is told why."""

    def before_tool(self, call):
        if call.name == "issue_refund" and float(call.args.get("amount", 0)) > 200:
            raise ToolError("Refunds over 200 dollars need a manager, not this chat.")


INSTRUCTIONS = (
    "You are a friendly support agent for an online shop. Be brief. Use get_order for order facts; never guess. "
    "When the customer asks for a refund, call issue_refund right away with the order id and amount (do not ask for confirmation: a person approves it). When the customer tells you a lasting fact about "
    "themselves (a name, a preference), save it with the remember tool."
)

# One dedicated event loop runs every SDK call; request threads submit to it.
LOOP = asyncio.new_event_loop()
threading.Thread(target=LOOP.run_forever, daemon=True).start()
DB = os.path.join(tempfile.gettempdir(), f"criogen-demo-chat-{uuid.uuid4().hex}.db")
STORAGE: Storage = None  # type: ignore[assignment]
AGENTS: dict = {}


def submit(coro):
    return asyncio.run_coroutine_threadsafe(coro, LOOP)


def available() -> list:
    return [(k, v[0]) for k, v in MODELS.items() if os.environ.get(v[1])]


def agent_for(model_id: str) -> Agent:
    if model_id not in AGENTS:
        label, _, make = MODELS[model_id]
        fallbacks = [MODELS[k][2]() for k, _ in available() if k != model_id][:1]
        AGENTS[model_id] = Agent(
            make(),
            instructions=INSTRUCTIONS,
            tools=[get_order, issue_refund],
            approve=["issue_refund"],
            hooks=[RefundGuard()],
            storage=STORAGE,
            memory=Memory.by_state("user_id", "visitors"),
            fallbacks=fallbacks,
            model_retry=Retry(max_attempts=2, initial_backoff=0.5),
            limits={"model_calls": 8, "tool_calls": 12},
        )
    return AGENTS[model_id]


async def init_storage() -> None:
    global STORAGE
    STORAGE = Storage(f"sqlite:///{DB}", audit=True)


def clean(text: str, limit: int = 4000) -> str:
    return str(text)[:limit]


async def run_turn(out: "queue.Queue", op: str, model_id: str, thread_id: str, visitor: str, payload: dict) -> None:
    """Run one operation on a thread and put what happens on `out` as events."""
    agent = agent_for(model_id)
    asked = False
    chat = agent.thread(thread_id, actor=f"visitor:{visitor}")
    try:
        if op == "edit":
            reply = await chat.decide({payload["call_id"]: Decision.edit(payload["args"])})
            out.put({"e": "text", "text": reply.text or ""})
            stream = None
        elif op == "send":
            stream = chat.stream(clean(payload["message"], 2000), user_id=visitor)
        elif op == "approve":
            stream = chat.stream_approve()
        else:
            stream = chat.stream_reject(clean(payload.get("reason") or "Rejected by the reviewer.", 300))
        if stream is not None:
            async for event in stream:
                data = event.data
                kind = data.get("type")
                if kind == "text":
                    out.put({"e": "text", "text": data.get("text", "")})
                elif kind == "tool_call_started":
                    out.put({"e": "tool_start", "tool": data.get("tool"), "args": data.get("args")})
                elif kind == "tool_call_finished":
                    out.put({"e": "tool_end", "tool": data.get("tool"), "status": data.get("status")})
                elif kind == "approval_needed":
                    asked = True
                    out.put({"e": "approval", "calls": [
                        {"call_id": r.get("call_id"), "tool": r.get("tool"), "args": r.get("args")} for r in data.get("requests", [])
                    ]})
            reply = await stream.reply()
        if getattr(reply, "needs_approval", False) and not asked:
            out.put({"e": "approval", "calls": [{"call_id": a.call_id, "tool": a.tool, "args": a.args} for a in reply.approvals]})
        out.put({"e": "done", "text": reply.text or "", "usage": reply.usage, "model_calls": reply.model_calls})
    except Exception as error:  # noqa: BLE001 - show the SDK's own error to the visitor
        code = getattr(error, "code", type(error).__name__)
        out.put({"e": "error", "code": code, "message": clean(getattr(error, "message", str(error)), 400)})
    finally:
        out.put(None)


async def snapshot(thread_id: str, visitor: str) -> dict:
    records = await STORAGE.audit_records(thread_id=thread_id)
    facts = await STORAGE.search(f"visitors/{visitor}")
    verify = await STORAGE.verify_audit()
    return {
        "audit": [{"action": r["action"], "actor": r.get("actor")} for r in records][-40:],
        "audit_ok": bool(verify.get("ok")),
        "memory": [{"key": f["key"], "value": f["value"]} for f in facts],
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "CriogenDemoChat"

    def log_message(self, *args):
        pass

    def _origin_ok(self) -> bool:
        origin = self.headers.get("origin")
        return origin is None or origin in ALLOWED_ORIGINS or origin == f"http://{self.headers.get('host')}"

    def _cors(self):
        origin = self.headers.get("origin")
        if origin is not None and self._origin_ok():
            self.send_header("access-control-allow-origin", origin)
            self.send_header("vary", "origin")

    def _json(self, status: int, body: dict):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.send_header("cache-control", "no-store")
        self._cors()
        self.end_headers()
        self.wfile.write(data)

    def do_OPTIONS(self):
        if self.headers.get("origin") is None or not self._origin_ok():
            return self._json(403, {"error": "origin not allowed"})
        self.send_response(204)
        self._cors()
        self.send_header("access-control-allow-methods", "GET, POST, OPTIONS")
        self.send_header("access-control-allow-headers", "content-type")
        self.send_header("access-control-allow-private-network", "true")
        self.send_header("access-control-max-age", "600")
        self.end_headers()

    def do_GET(self):
        if not self._origin_ok():
            return self._json(403, {"error": "origin not allowed"})
        path, _, query = self.path.partition("?")
        if path == "/api/info":
            return self._json(200, {"models": [{"id": k, "label": v} for k, v in available()], "orders": sorted(ORDERS)})
        if path == "/api/state":
            params = dict(p.split("=", 1) for p in query.split("&") if "=" in p)
            thread, visitor = params.get("thread", ""), params.get("visitor", "")
            if not thread or not visitor:
                return self._json(400, {"error": "thread and visitor are required"})
            try:
                return self._json(200, submit(snapshot(thread, visitor)).result(timeout=20))
            except Exception as error:  # noqa: BLE001
                return self._json(500, {"error": clean(str(error), 300)})
        self._json(404, {"error": "not found"})

    def do_POST(self):
        if not self._origin_ok():
            return self._json(403, {"error": "origin not allowed"})
        if self.path not in ("/api/chat", "/api/approve", "/api/reject", "/api/edit"):
            return self._json(404, {"error": "not found"})
        try:
            size = int(self.headers.get("content-length") or 0)
            if size > 16 * 1024:
                raise ValueError("too large")
            body = json.loads(self.rfile.read(size) or b"{}")
            model = body.get("model") or (available()[0][0] if available() else "")
            if model not in dict(available()):
                raise ValueError("no such model, or its key is not set")
            thread = str(body.get("thread", ""))[:64]
            visitor = str(body.get("visitor", ""))[:64]
            if not thread or not visitor:
                raise ValueError("thread and visitor are required")
            op = {"/api/chat": "send", "/api/approve": "approve", "/api/reject": "reject", "/api/edit": "edit"}[self.path]
            if op == "send" and not str(body.get("message", "")).strip():
                raise ValueError("empty message")
            if op == "edit" and not (isinstance(body.get("args"), dict) and body.get("call_id")):
                raise ValueError("call_id and args are required")
        except (ValueError, TypeError) as error:
            return self._json(400, {"error": str(error)})
        self.send_response(200)
        self.send_header("content-type", "application/x-ndjson")
        self.send_header("cache-control", "no-store")
        self._cors()
        self.end_headers()
        out: "queue.Queue" = queue.Queue()
        future = submit(run_turn(out, op, model, thread, visitor, body))
        try:
            while True:
                item = out.get()
                if item is None:
                    break
                self.wfile.write((json.dumps(item) + "\n").encode())
                self.wfile.flush()
        except (BrokenPipeError, ConnectionError, OSError):
            future.cancel()  # the page went away: stop the run (it is still saved)


def main() -> None:
    load_keys()
    submit(init_storage()).result(timeout=20)
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"demo chat on http://127.0.0.1:{PORT} with models: {[k for k, _ in available()] or 'none (add keys to ~/.criogen-keys.env)'}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        submit(STORAGE.close()).result(timeout=10)
        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(DB + suffix)
            except OSError:
                pass


if __name__ == "__main__":
    main()

"""A fake model server, the same for every framework (the playground's model).

    python playground/mock_model.py [--port 0]      # prints: PORT <number>

It speaks the OpenAI chat completions wire format on 127.0.0.1, so each
framework builds the request, sends it over HTTP, parses the reply and
extracts tool calls with its own real client, exactly as with a hosted model.
Only the model's *answers* are scripted, and they are the same for everyone:

- The model name carries the delay: `mock-<milliseconds>` (each call waits
  that long, like a model thinking).
- Asked a question with tools, it first asks for **all** the tools at once
  (arguments `{"order_id": "A-1001"}`); once every tool has answered, it
  replies with a sentence that quotes the user's message. With no tools it
  answers at once.
- Streaming (`"stream": true`) and plain replies are both supported.

It runs in its own process, so its CPU and memory are not counted against
either framework. Standard library only.
"""

import argparse
import itertools
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ORDER = "A-1001"
_ids = itertools.count(1)
_ids_lock = threading.Lock()


def next_id() -> int:
    with _ids_lock:
        return next(_ids)


def text_of(content) -> str:
    if isinstance(content, str):
        return content
    return "".join(part.get("text", "") for part in content or [] if isinstance(part, dict))


def script(body: dict) -> dict:
    """What the model says next: `{"calls": [names]}` or `{"text": answer}`."""
    messages = body.get("messages", [])
    tools = [t["function"]["name"] for t in body.get("tools") or []]
    last_user = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=-1)
    answered = sum(1 for m in messages[last_user + 1 :] if m.get("role") == "tool")
    if tools and answered < len(tools):
        return {"calls": tools}
    said = text_of(messages[last_user].get("content")) if last_user >= 0 else ""
    used = f" after {len(tools)} lookups" if tools else ""
    return {"text": f"[simulated reply{used}] You said: “{said}”."}


def delay_of(model: str) -> float:
    prefix, _, number = str(model).partition("-")
    if prefix != "mock":
        return 0.0
    try:
        return max(0, int(number)) / 1000
    except ValueError:
        return 0.0


def tool_call(name: str) -> dict:
    return {
        "id": f"call_{next_id()}",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps({"order_id": ORDER})},
    }


USAGE = {"prompt_tokens": 40, "completion_tokens": 20, "total_tokens": 60}


def completion(body: dict, step: dict) -> dict:
    message = {"role": "assistant", "content": step.get("text")}
    if "calls" in step:
        message["tool_calls"] = [tool_call(n) for n in step["calls"]]
    return {
        "id": f"chatcmpl-{next_id()}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": body.get("model", "mock"),
        "choices": [{"index": 0, "message": message, "finish_reason": "tool_calls" if "calls" in step else "stop"}],
        "usage": USAGE,
    }


def chunks(body: dict, step: dict):
    """The same reply as a stream of `chat.completion.chunk` events."""
    base = {"id": f"chatcmpl-{next_id()}", "object": "chat.completion.chunk", "created": int(time.time()), "model": body.get("model", "mock")}

    def chunk(delta, finish=None):
        return {**base, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}

    yield chunk({"role": "assistant", "content": ""})
    if "calls" in step:
        for i, name in enumerate(step["calls"]):
            call = tool_call(name)
            yield chunk({"tool_calls": [{"index": i, "id": call["id"], "type": "function", "function": {"name": name, "arguments": ""}}]})
            yield chunk({"tool_calls": [{"index": i, "function": {"arguments": call["function"]["arguments"]}}]})
        yield chunk({}, "tool_calls")
    else:
        text = step["text"]
        for start in range(0, len(text), 24):
            yield chunk({"content": text[start : start + 24]})
        yield chunk({}, "stop")
    if (body.get("stream_options") or {}).get("include_usage"):
        yield {**base, "choices": [], "usage": USAGE}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"  # keep-alive: connections are reused, as with a real API

    def log_message(self, *args):
        pass

    def do_POST(self):
        if not self.path.rstrip("/").endswith("/chat/completions"):
            return self._json(404, {"error": {"message": "not found"}})
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"{}")
        except ValueError:
            return self._json(400, {"error": {"message": "bad JSON"}})
        step = script(body)
        time.sleep(delay_of(body.get("model", "")))
        if not body.get("stream"):
            return self._json(200, completion(body, step))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()
        for event in chunks(body, step):
            self._chunk(f"data: {json.dumps(event)}\n\n".encode())
        self._chunk(b"data: [DONE]\n\n")
        self.wfile.write(b"0\r\n\r\n")

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            return self._json(200, {"object": "list", "data": [{"id": "mock-0", "object": "model"}]})
        self._json(404, {"error": {"message": "not found"}})

    def _chunk(self, data: bytes):
        self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
        self.wfile.flush()

    def _json(self, status: int, body: dict):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 512  # a burst of many runs connects at once


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    server = Server(("127.0.0.1", args.port), Handler)
    print(f"PORT {server.server_address[1]}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

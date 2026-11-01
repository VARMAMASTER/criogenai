"""Kaggle's Model Proxy (design 0020) from Python, against a local server
that plays both Kaggle's API and the proxy. No network."""

import http.server
import json
import threading

import pytest

from criogen import Agent, Kaggle, tool

API_TOKEN = "KGAT_python_test_token"


class Handler(http.server.BaseHTTPRequestHandler):
    seen = []

    def log_message(self, *args):
        pass

    def _json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _sse(self, model, message, finish):
        """The same answer as a stream, as the real proxy sends it."""
        delta = {"role": "assistant"}
        if message.get("tool_calls"):
            delta["tool_calls"] = [dict(call, index=i) for i, call in enumerate(message["tool_calls"])]
        else:
            delta["content"] = message["content"]
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for chunk in (
            {"model": model, "choices": [{"index": 0, "delta": delta}]},
            {"model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
             "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}},
        ):
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.close_connection = True

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        auth = self.headers.get("authorization")
        Handler.seen.append((self.path, auth, body))
        host = f"http://127.0.0.1:{self.server.server_address[1]}"
        if self.path.endswith("/CreateDefaultModelProxyToken"):
            if auth != f"Bearer {API_TOKEN}":
                return self._json(401, {"message": "bad token"})
            return self._json(200, {"baseUri": f"{host}/models", "token": "proxy-1", "expiryTime": "2999-01-01T00:00:00Z"})
        if self.path == "/models/openapi/chat/completions":
            if auth != "Bearer proxy-1":
                return self._json(401, {"error": {"message": "bad proxy token"}})
            calls = [m for m in body["messages"] if m.get("role") == "tool"]
            if body.get("tools") and not calls:
                message = {"role": "assistant", "content": None, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "add", "arguments": "{\"a\": 2, \"b\": 3}"}}]}
                finish = "tool_calls"
            else:
                message = {"role": "assistant", "content": f"answer from {body['model']}"}
                finish = "stop"
            if body.get("stream"):
                return self._sse(body["model"], message, finish)
            return self._json(200, {"id": "x", "object": "chat.completion", "model": body["model"],
                                    "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                                    "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}})
        self._json(404, {"message": "not found"})


@pytest.fixture(scope="module")
def server():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


@tool
def add(a: int, b: int) -> int:
    "Adds two numbers."
    return a + b


def test_an_agent_calls_a_kaggle_model_with_tools(server):
    Handler.seen.clear()
    agent = Agent(Kaggle("google/gemini-3.6-flash", api_token=API_TOKEN, api_base=server), tools=[add])
    result = agent.run_sync("What is 2 + 3?")
    assert result.text == "answer from google/gemini-3.6-flash"
    mints = [s for s in Handler.seen if s[0].endswith("/CreateDefaultModelProxyToken")]
    assert len(mints) == 1  # one token for both model calls
    assert any(m["role"] == "tool" for m in Handler.seen[-1][2]["messages"])


def test_model_strings_and_repr_never_show_a_token(server):
    assert repr(Kaggle("m", api_token=API_TOKEN)) == "Kaggle('m')"
    spec = Kaggle("m", api_token=API_TOKEN, api_base=server).spec()
    assert spec["kind"] == "kaggle" and spec["api_key"] == API_TOKEN
    agent = Agent(Kaggle("openai/gpt-oss-120b", proxy_url=f"{server}/models/openapi", proxy_token="proxy-1"))
    assert agent.run_sync("hi").text == "answer from openai/gpt-oss-120b"


def test_without_credentials_the_error_says_what_to_set(monkeypatch, tmp_path):
    for name in ("MODEL_PROXY_URL", "MODEL_PROXY_API_KEY", "KAGGLE_API_TOKEN"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("KAGGLE_CONFIG_DIR", str(tmp_path))  # no access_token file there
    with pytest.raises(Exception) as e:
        Agent("kaggle:google/gemini-3.6-flash").run_sync("hi")
    assert "KAGGLE_API_TOKEN" in str(e.value)


def test_the_token_file_is_found(monkeypatch, tmp_path, server):
    for name in ("MODEL_PROXY_URL", "MODEL_PROXY_API_KEY", "KAGGLE_API_TOKEN"):
        monkeypatch.delenv(name, raising=False)
    (tmp_path / "access_token").write_text(API_TOKEN + "\n", encoding="utf-8")
    monkeypatch.setenv("KAGGLE_CONFIG_DIR", str(tmp_path))
    agent = Agent(Kaggle("ibm/granite-4.0-h-small", api_base=server))
    assert agent.run_sync("hi").text == "answer from ibm/granite-4.0-h-small"

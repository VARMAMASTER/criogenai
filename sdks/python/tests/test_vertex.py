"""Google Vertex AI (design 0006 §19) from Python, against a local server.
No network."""

import http.server
import json
import threading

import pytest

from criogenai import Agent, Vertex
from criogenai.models import model_spec

TEXT = "Hi from Vertex"
CLAUDE_STREAM = [
    {"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "model": "c",
                                          "content": [], "usage": {"input_tokens": 1, "output_tokens": 0}}},
    {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": TEXT}},
    {"type": "content_block_stop", "index": 0},
    {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
    {"type": "message_stop"},
]


class Handler(http.server.BaseHTTPRequestHandler):
    seen = []

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        Handler.seen.append({"path": self.path, "auth": self.headers.get("authorization"), "body": body})
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        if ":streamRawPredict" in self.path:
            for event in CLAUDE_STREAM:
                self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
        else:
            for chunk in ({"choices": [{"index": 0, "delta": {"role": "assistant", "content": TEXT}}]},
                          {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}):
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")
        self.close_connection = True


@pytest.fixture(scope="module")
def host():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


def test_claude_goes_to_stream_raw_predict(host):
    Handler.seen.clear()
    model = Vertex("claude-sonnet-5", project="my-project", location="global", token="ya29.t", host=host)
    assert Agent(model).run_sync("hi").text == TEXT
    [call] = Handler.seen
    assert call["path"] == "/v1/projects/my-project/locations/global/publishers/anthropic/models/claude-sonnet-5:streamRawPredict"
    assert call["auth"] == "Bearer ya29.t"
    assert call["body"]["anthropic_version"] == "vertex-2023-10-16" and "model" not in call["body"]


def test_gemini_goes_to_the_openai_compatible_endpoint(host):
    Handler.seen.clear()
    model = Vertex("google/gemini-3.6-flash", project="my-project", location="europe-west4",
                   token="ya29.t", host=host)
    assert Agent(model).run_sync("hi").text == TEXT
    [call] = Handler.seen
    assert call["path"] == "/v1/projects/my-project/locations/europe-west4/endpoints/openapi/chat/completions"


def test_strings_options_and_refusals(monkeypatch, tmp_path):
    assert model_spec("vertex:claude-sonnet-5") == {"kind": "vertex", "model": "claude-sonnet-5", "api": "auto"}
    with pytest.raises(ValueError):
        Vertex("m", api="gemini")
    assert "ya29.secret" not in repr(Vertex("m", token="ya29.secret"))
    monkeypatch.delenv("GOOGLE_CLOUD_LOCATION", raising=False)
    with pytest.raises(Exception, match="GOOGLE_CLOUD_LOCATION"):
        Agent(Vertex("m", project="p", token="t")).run_sync("hi")
    key = tmp_path / "key.json"
    key.write_text(json.dumps({"type": "service_account", "private_key": "x"}))
    with pytest.raises(Exception, match="not supported yet"):
        Agent(Vertex("m", project="p", location="global", credentials_file=str(key))).run_sync("hi")

"""Amazon Bedrock with an API key (design 0006 §18) from Python, against a
local server. No network."""

import http.server
import json
import threading

import pytest

from criogen import Agent, Bedrock
from criogen.models import model_spec

TEXT = "Hi from Bedrock"
KEY = "bedrock-python-key"

MESSAGES_STREAM = [
    {"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant",
                                          "model": "m", "content": [],
                                          "usage": {"input_tokens": 3, "output_tokens": 0}}},
    {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": TEXT}},
    {"type": "content_block_stop", "index": 0},
    {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}},
    {"type": "message_stop"},
]
CHAT_STREAM = [
    {"choices": [{"index": 0, "delta": {"role": "assistant", "content": TEXT}}]},
    {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
]


class Handler(http.server.BaseHTTPRequestHandler):
    seen = []

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        json.loads(self.rfile.read(length) or b"{}")
        Handler.seen.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}})
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        if self.path.endswith("/messages"):
            for event in MESSAGES_STREAM:
                self.wfile.write(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n".encode())
        else:
            for chunk in CHAT_STREAM:
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")
        self.close_connection = True


@pytest.fixture(scope="module")
def base():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}/v1"
    srv.shutdown()


def test_claude_uses_the_messages_api(base):
    Handler.seen.clear()
    model = Bedrock("us.anthropic.claude-sonnet-5", region="us-east-1", api_key=KEY, base_url=base)
    assert Agent(model).run_sync("hi").text == TEXT
    [call] = Handler.seen
    assert call["path"] == "/v1/messages"
    assert call["headers"]["x-api-key"] == KEY


def test_other_models_use_chat_completions(base):
    Handler.seen.clear()
    model = Bedrock("openai.gpt-oss-120b-1:0", region="us-east-1", api_key=KEY, base_url=base)
    assert Agent(model).run_sync("hi").text == TEXT
    [call] = Handler.seen
    assert call["path"] == "/v1/chat/completions"
    assert call["headers"]["authorization"] == f"Bearer {KEY}"


def test_strings_options_and_no_default_region(monkeypatch):
    assert model_spec("bedrock:openai.gpt-oss-120b-1:0") == {
        "kind": "bedrock", "model": "openai.gpt-oss-120b-1:0", "api": "auto"}
    with pytest.raises(ValueError):
        Bedrock("m", api="converse")
    assert KEY not in repr(Bedrock("m", api_key=KEY))
    for name in ("AWS_REGION", "AWS_DEFAULT_REGION"):
        monkeypatch.delenv(name, raising=False)
    with pytest.raises(Exception, match="AWS_REGION"):
        Agent(Bedrock("m", api_key=KEY)).run_sync("hi")
    with pytest.raises(Exception, match="AWS region"):
        Agent(Bedrock("m", region="us-east-1.evil.com", api_key=KEY)).run_sync("hi")

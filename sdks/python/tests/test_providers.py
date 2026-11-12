"""Named providers (design 0006 §4.1): the SDK's key table matches the
engine's, every name works as a string and as a class, and
AnthropicCompatible speaks the Messages API. Local servers only."""

import http.server
import json
import threading

import pytest

import criogenai
from criogenai import Agent, AnthropicCompatible, DeepSeek, env
from criogenai import _native
from criogenai.models import _KEYS, model_spec

PRESETS = json.loads(_native.PRESETS)

TEXT = "Hi from the server"

# What a streaming server sends, as (event name, data) pairs.
OPENAI_STREAM = [
    (None, {"choices": [{"index": 0, "delta": {"role": "assistant", "content": TEXT}}]}),
    (None, {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2}}),
]
# The OpenAI Responses API (design 0006 §16): what `openai` speaks.
RESPONSES_STREAM = [
    ("response.created", {"type": "response.created", "response": {"id": "resp_1", "model": "m"}}),
    ("response.output_item.added", {"type": "response.output_item.added", "output_index": 0,
                                    "item": {"type": "message", "content": []}}),
    ("response.output_text.delta", {"type": "response.output_text.delta", "output_index": 0,
                                    "content_index": 0, "delta": TEXT}),
    ("response.completed", {"type": "response.completed", "response": {
        "status": "completed", "usage": {"input_tokens": 3, "output_tokens": 2}}}),
]
ANTHROPIC_STREAM = [
    ("message_start", {"type": "message_start", "message": {
        "id": "msg_1", "type": "message", "role": "assistant", "model": "m", "content": [],
        "usage": {"input_tokens": 3, "output_tokens": 0}}}),
    ("content_block_start", {"type": "content_block_start", "index": 0,
                             "content_block": {"type": "text", "text": ""}}),
    ("content_block_delta", {"type": "content_block_delta", "index": 0,
                             "delta": {"type": "text_delta", "text": TEXT}}),
    ("content_block_stop", {"type": "content_block_stop", "index": 0}),
    ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                       "usage": {"output_tokens": 2}}),
    ("message_stop", {"type": "message_stop"}),
]


class Server(http.server.BaseHTTPRequestHandler):
    seen = []

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        Server.seen.append({"path": self.path, "auth": self.headers.get("authorization"),
                            "x-api-key": self.headers.get("x-api-key"), "body": body})
        anthropic = self.path.endswith("/messages")
        responses = self.path.endswith("/responses")
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        script = ANTHROPIC_STREAM if anthropic else RESPONSES_STREAM if responses else OPENAI_STREAM
        for event, data in script:
            head = f"event: {event}\n" if event else ""
            self.wfile.write(f"{head}data: {json.dumps(data)}\n\n".encode())
        if not anthropic and not responses:
            self.wfile.write(b"data: [DONE]\n\n")
        self.close_connection = True

    def log_message(self, *args):
        pass


@pytest.fixture(scope="module")
def base():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Server)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}/v1"
    srv.shutdown()


def test_the_key_table_matches_the_engine():
    engine = {p["name"]: p["key_env"] for p in PRESETS if p["key_env"]}
    assert _KEYS == engine
    local = {p["name"] for p in PRESETS if not p["key_env"]}
    assert local == {"ollama", "lmstudio"}


def test_every_named_provider_works_as_a_string(base, monkeypatch):
    for p in PRESETS:
        if p["key_env"]:
            monkeypatch.setenv(p["key_env"], "sk-test-key")
        spec = model_spec(f"{p['name']}:some/model")
        assert spec["kind"] == p["name"] and spec["model"] == "some/model"
        if p["key_env"]:
            assert spec["api_key"] == "sk-test-key"


def test_every_named_provider_answers_through_its_class(base):
    classes = {
        name: cls for name, cls in vars(criogenai).items()
        if isinstance(cls, type) and getattr(cls, "kind", None)
    }
    by_kind = {cls.kind: cls for cls in classes.values()}
    for p in PRESETS:
        cls = by_kind[p["name"]]
        Server.seen.clear()
        result = Agent(cls("m", api_key="sk-test-key", base_url=base)).run_sync("hi")
        assert result.text == TEXT, p["name"]
        [request] = Server.seen
        if p["anthropic"]:
            assert request["path"] == "/v1/messages" and request["x-api-key"] == "sk-test-key"
        else:
            # `openai` speaks the Responses API; the others Chat Completions.
            path = "/v1/responses" if p["name"] == "openai" else "/v1/chat/completions"
            assert request["path"] == path, p["name"]
            assert request["auth"] == "Bearer sk-test-key", p["name"]


def test_a_missing_key_names_the_variable(monkeypatch):
    monkeypatch.delenv("DEEPSEEK_API_KEY", raising=False)
    with pytest.raises(ValueError, match="DEEPSEEK_API_KEY") as caught:
        Agent("deepseek:deepseek-chat")
    # Also a coded error: catchable like every other configuration error.
    from criogenai import ValidationError

    assert isinstance(caught.value, ValidationError) and caught.value.code == "invalid_value"
    assert caught.value.details == {"provider": "deepseek", "variable": "DEEPSEEK_API_KEY"}


def test_an_embedder_without_its_key_fails_when_made(monkeypatch):
    from criogenai import Embedder, ValidationError

    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    with pytest.raises(ValidationError, match="OPENAI_API_KEY"):
        Embedder.openai()
    with pytest.raises(ValueError, match="GEMINI_API_KEY"):
        Embedder.gemini()
    assert Embedder.openai(api_key="sk-test-not-real") is not None
    with pytest.raises(ValueError, match="provider one of"):
        Agent("nosuch:gpt-5")


def test_status_and_key_names_cover_the_new_providers(monkeypatch):
    for name in _KEYS.values():
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("GROQ_API_KEY", "g")
    status = env.status()
    assert status["groq"] is True and status["deepseek"] is False
    assert env.key_name("qwen") == "DASHSCOPE_API_KEY"


def test_anthropic_compatible_speaks_the_messages_api(base):
    Server.seen.clear()
    result = Agent(AnthropicCompatible("glm-5.2", base_url=base, api_key="sk-test-key")).run_sync("hi")
    assert result.text == TEXT
    [request] = Server.seen
    assert request["path"] == "/v1/messages"
    assert request["x-api-key"] == "sk-test-key" and request["auth"] is None
    assert request["body"]["model"] == "glm-5.2"
    with pytest.raises(TypeError):
        AnthropicCompatible("m")  # base_url is required


def test_reprs_never_show_the_key():
    assert "sk-" not in repr(DeepSeek("deepseek-chat", api_key="sk-secret-123456"))

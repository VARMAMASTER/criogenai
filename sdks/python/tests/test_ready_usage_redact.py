"""ready(), result facts (duration, which model answered, per-model usage),
estimate_cost(), and redact: nothing redacted leaves for a provider."""

import http.server
import json
import threading

import pytest

from criogen import Agent, FakeModel, FakeReply, Kaggle, OpenAICompatible, Retry, estimate_cost, ready_sync, tool


@tool
def add(a: int, b: int) -> int:
    "Adds two numbers."
    return a + b


def test_ready_reports_the_engine_and_a_model():
    bare = ready_sync()
    assert bare["ok"] and bare["engine"]["ok"] and "model" not in bare
    with_model = ready_sync(FakeModel(["OK"]))
    assert with_model["ok"] and with_model["model"]["ok"] and with_model["model"]["latency_ms"] >= 0
    assert with_model["model"]["model"] == "FakeModel"


def test_ready_reports_a_model_it_cannot_use_without_raising(monkeypatch, tmp_path):
    for name in ("MODEL_PROXY_URL", "MODEL_PROXY_API_KEY", "KAGGLE_API_TOKEN"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("KAGGLE_CONFIG_DIR", str(tmp_path))
    report = ready_sync(Kaggle("google/gemini-3.6-flash"))
    assert report["ok"] is False and report["engine"]["ok"] is True
    assert report["model"]["ok"] is False and "KAGGLE_API_TOKEN" in report["model"]["error"]


def test_results_say_which_model_answered_and_how_long_it_took():
    model = FakeModel([FakeReply.tool_call("add", {"a": 2, "b": 3}), "5"], name="main")
    result = Agent(model, tools=[add]).run_sync("2 + 3?")
    assert result.duration_ms >= 0
    assert result.answered_by == {"provider": "fake", "model": "main"}
    assert len(result.models) == 1 and result.models[0]["calls"] == 2
    assert result.models[0]["usage"]["input_tokens"] == result.usage["input_tokens"]


def test_a_fallback_that_answered_is_named():
    agent = Agent(
        FakeModel(["never"], fail_first=100, name="main"),
        fallbacks=[FakeModel(["from the backup"], name="backup")],
        model_retry=Retry(max_attempts=1, initial_backoff=0.01, jitter=False),
    )
    result = agent.run_sync("hi")
    assert result.text == "from the backup"
    assert result.answered_by == {"provider": "fake", "model": "backup"}


def test_estimate_cost():
    result = Agent(FakeModel(["hello"], name="main")).run_sync("hi")
    usage = result.models[0]["usage"]
    expected = (usage["input_tokens"] * 2 + usage["output_tokens"] * 8) / 1_000_000
    assert estimate_cost(result, {"fake:main": {"input": 2, "output": 8}}) == expected
    assert estimate_cost(result, {"other": {"input": 1, "output": 1}}) is None


class Provider(http.server.BaseHTTPRequestHandler):
    bodies = []

    def log_message(self, *args):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length") or 0)).decode()
        Provider.bodies.append(raw)
        body = json.loads(raw)
        tool_done = any(m.get("role") == "tool" for m in body["messages"])
        if tool_done:
            delta, finish = {"role": "assistant", "content": "The record is on file."}, "stop"
        else:
            call = {"index": 0, "id": "c1", "type": "function", "function": {"name": "get_record", "arguments": "{}"}}
            delta, finish = {"role": "assistant", "tool_calls": [call]}, "tool_calls"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for chunk in ({"choices": [{"index": 0, "delta": delta}]}, {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}):
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.close_connection = True


@pytest.fixture(scope="module")
def provider():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}/v1"
    srv.shutdown()


@tool
def get_record() -> dict:
    "The patient's record."
    return {"name": "Asha Rao", "mrn": "MRN-884213", "note": "Seen on Monday. Call +91 98450 12345."}


def scrub(text: str) -> str:
    import re

    text = re.sub(r"MRN-\d+", "[MRN]", text)
    text = re.sub(r"\+?\d[\d ]{8,}\d", "[PHONE]", text)
    return text.replace("Asha Rao", "[NAME]")


def test_redacted_text_never_reaches_the_provider(provider):
    Provider.bodies.clear()
    agent = Agent(OpenAICompatible("m", base_url=provider), tools=[get_record], redact=scrub)
    result = agent.run_sync("Summarize the record for Asha Rao; her phone is +91 98450 12345.")
    assert result.text == "The record is on file."
    assert len(Provider.bodies) == 2
    for body in Provider.bodies:
        for secret in ("Asha Rao", "MRN-884213", "98450 12345"):
            assert secret not in body, secret
    assert "[NAME]" in Provider.bodies[0] and "[MRN]" in Provider.bodies[1] and "Seen on Monday" in Provider.bodies[1]


def test_an_async_redactor_works(provider):
    Provider.bodies.clear()

    async def scrub_async(text: str) -> str:
        return scrub(text)

    Agent(OpenAICompatible("m", base_url=provider), tools=[get_record], redact=scrub_async).run_sync("Patient Asha Rao.")
    assert all("Asha Rao" not in b for b in Provider.bodies)

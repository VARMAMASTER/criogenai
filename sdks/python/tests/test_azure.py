"""Azure OpenAI and Foundry (design 0006 §17) from Python, against a local
server that plays both Entra ID and the Azure resource. No network."""

import http.server
import json
import threading
import urllib.parse

import pytest

from criogenai import Agent, Azure
from criogenai.models import model_spec

TEXT = "Hi from Azure"
SECRET = "python-client-secret-value"


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

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length)
        Handler.seen.append({"path": self.path, "headers": dict(self.headers), "raw": raw})
        if self.path.endswith("/oauth2/v2.0/token"):
            form = urllib.parse.parse_qs(raw.decode())
            if form.get("client_secret") != [SECRET]:
                return self._json(401, {"error": "invalid_client", "error_description": "bad secret"})
            return self._json(200, {"access_token": "entra-token", "expires_in": 3599})
        if self.path == "/openai/v1/responses":
            body = json.loads(raw)
            if not body.get("stream"):
                return self._json(200, {"status": "completed", "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": TEXT}]}]})
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            for event in (
                {"type": "response.output_item.added", "output_index": 0,
                 "item": {"type": "message", "content": []}},
                {"type": "response.output_text.delta", "output_index": 0, "content_index": 0,
                 "delta": TEXT},
                {"type": "response.completed", "response": {"status": "completed"}},
            ):
                self.wfile.write(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n".encode())
            self.close_connection = True
            return None
        return self._json(404, {"error": {"message": "unknown path"}})


@pytest.fixture(scope="module")
def origin():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


def test_an_api_key_goes_in_the_api_key_header(origin):
    Handler.seen.clear()
    result = Agent(Azure("my-deployment", endpoint=origin, api_key="azure-key-123")).run_sync("hi")
    assert result.text == TEXT
    [call] = Handler.seen
    assert call["path"] == "/openai/v1/responses"
    headers = {k.lower(): v for k, v in call["headers"].items()}
    assert headers["api-key"] == "azure-key-123"
    assert "authorization" not in headers


def test_a_service_principal_gets_a_bearer_token(origin):
    Handler.seen.clear()
    model = Azure("gpt-5-mini", endpoint=origin, tenant_id="my-tenant", client_id="app",
                  client_secret=SECRET, authority=origin)
    assert Agent(model).run_sync("hi").text == TEXT
    token, call = Handler.seen
    assert token["path"] == "/my-tenant/oauth2/v2.0/token"
    headers = {k.lower(): v for k, v in call["headers"].items()}
    assert headers["authorization"] == "Bearer entra-token"


def test_a_bad_secret_is_an_auth_error_without_the_secret(origin):
    from criogenai import ModelError

    model = Azure("gpt-5-mini", endpoint=origin, tenant_id="t", client_id="c",
                  client_secret="wrong-secret-value", authority=origin)
    with pytest.raises(ModelError) as caught:
        Agent(model).run_sync("hi")
    assert "wrong-secret-value" not in str(caught.value)
    assert "wrong-secret-value" not in repr(model)


def test_strings_specs_and_options(monkeypatch):
    spec = model_spec("azure:gpt-5-mini")
    assert spec == {"kind": "azure", "model": "gpt-5-mini", "api": "responses"}
    chat = Azure("DeepSeek-V4", endpoint="my-res", api="chat", managed_identity=True).spec()
    assert chat["api"] == "chat" and chat["managed_identity"] is True
    with pytest.raises(ValueError):
        Azure("m", api="completions")
    for name in ("AZURE_OPENAI_ENDPOINT", "AZURE_OPENAI_API_KEY", "AZURE_TENANT_ID",
                 "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET", "AZURE_USE_MANAGED_IDENTITY"):
        monkeypatch.delenv(name, raising=False)
    with pytest.raises(Exception, match="AZURE_OPENAI_ENDPOINT"):
        Agent("azure:gpt-5-mini").run_sync("hi")

"""Web tools (design 0019): search and page reading, against a local server only."""

import http.server
import json
import threading

import pytest

from criogen import Agent, FakeModel, FakeReply, specialists, web

SECRET = "tvly-secret-key-123"


class Handler(http.server.BaseHTTPRequestHandler):
    seen = []

    def log_message(self, *args):
        pass

    def _send(self, status, body, kind="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["content-length"])))
        Handler.seen.append(("POST", self.path, self.headers.get("authorization"), payload))
        if self.headers.get("authorization") != f"Bearer {SECRET}":
            return self._send(401, {"error": f"bad key {self.headers.get('authorization')}"})
        results = [{"title": f"Result {i}", "url": f"https://example.com/{i}", "content": "x" * 2000} for i in range(20)]
        self._send(200, {"answer": "Solar is growing.", "results": results})

    def do_GET(self):
        Handler.seen.append(("GET", self.path, self.headers.get("x-subscription-token"), None))
        if self.path.startswith("/res/v1/web/search"):
            return self._send(200, {"web": {"results": [{"title": "Brave hit", "url": "https://example.org", "description": "d"}]}})
        if self.path == "/page":
            return self._send(200, b"<html><head><title>t</title><script>evil()</script></head><body><h1>Hello</h1><p>World &amp; more</p></body></html>", "text/html")
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("location", "http://127.0.0.1:1/elsewhere")
            self.end_headers()
            return
        if self.path == "/big":
            return self._send(200, b"a" * 600_000, "text/plain")
        self._send(404, {})


@pytest.fixture(scope="module")
def server():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


def call(tool, name, args):
    agent = Agent(FakeModel([FakeReply.tool_call(name, args), "done"]), tools=[tool])
    result = agent.run_sync("go")
    return [m for m in result.messages if m["role"] == "tool"][0]


def test_tavily_returns_bounded_results_and_sends_the_key_only_as_a_header(server):
    tool = web.tavily_search(SECRET, max_results=3, base_url=server)
    out = call(tool, "web_search", {"query": "solar panels"})
    assert out["status"] == "success"
    value = out["content"][0]["value"]
    assert value["answer"] == "Solar is growing."
    assert len(value["results"]) == 3  # asked for 3 of 20
    assert all(len(r["snippet"]) <= 600 for r in value["results"])  # snippets are cut
    method, path, auth, payload = Handler.seen[-1]
    assert auth == f"Bearer {SECRET}" and payload["query"] == "solar panels"
    assert SECRET not in json.dumps(out)  # never in what the model sees


def test_a_wrong_key_gives_a_status_not_the_response_body(server):
    out = call(web.tavily_search("wrong-key", base_url=server), "web_search", {"query": "x"})
    assert out["status"] == "error"
    text = json.dumps(out)
    assert "401" in text and "wrong-key" not in text and "bad key" not in text


def test_search_without_any_key_says_what_to_set(monkeypatch):
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    monkeypatch.delenv("BRAVE_API_KEY", raising=False)
    with pytest.raises(ValueError, match="TAVILY_API_KEY"):
        web.search()
    out = call(web.tavily_search(), "web_search", {"query": "x"})
    assert out["status"] == "error" and "TAVILY_API_KEY" in json.dumps(out)
    assert web.default_tools() == []  # a researcher still works, just without the internet


def test_search_picks_the_provider_from_the_environment(monkeypatch, server):
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    monkeypatch.setenv("BRAVE_API_KEY", "brave-key")
    assert web.search().name == "web_search"
    brave = web.brave_search(base_url=server)
    out = call(brave, "web_search", {"query": "rain"})
    assert out["content"][0]["value"]["results"][0]["title"] == "Brave hit"
    assert Handler.seen[-1][2] == "brave-key"
    monkeypatch.setenv("TAVILY_API_KEY", SECRET)
    assert len(web.default_tools()) == 2  # search + read_page


def test_credentials_do_not_go_over_plain_http_to_another_machine():
    with pytest.raises(ValueError, match="https"):
        web.tavily_search("k", base_url="http://search.example.com")


def test_fetch_reads_text_skips_scripts_and_refuses_private_addresses_by_default(server):
    out = call(web.fetch(), "read_page", {"url": f"{server}/page"})
    assert out["status"] == "error" and "private network" in json.dumps(out)
    out = call(web.fetch(allow_private=True), "read_page", {"url": f"{server}/page"})
    value = out["content"][0]["value"]
    assert "Hello" in value["text"] and "World & more" in value["text"] and "evil" not in value["text"]
    bad = call(web.fetch(allow_private=True), "read_page", {"url": "file:///etc/passwd"})
    assert bad["status"] == "error"


def test_fetch_does_not_follow_redirects_and_bounds_what_it_reads(server):
    redirected = call(web.fetch(allow_private=True), "read_page", {"url": f"{server}/redirect"})
    assert redirected["status"] == "error"
    assert "reading the page failed: HTTP 302 (redirects are not followed)" in json.dumps(redirected)
    big = call(web.fetch(allow_private=True), "read_page", {"url": f"{server}/big"})
    assert big["status"] == "error" and "reading the page failed: the response is larger than 500 KB" in json.dumps(big)
    assert "search failed" not in json.dumps([redirected, big])


def test_the_researcher_gets_web_tools_only_when_a_key_is_set(monkeypatch):
    monkeypatch.setenv("TAVILY_API_KEY", SECRET)
    keyed = specialists.researcher.build(FakeModel(["ok"]))
    assert keyed.run_sync("x").text == "ok"
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    monkeypatch.delenv("BRAVE_API_KEY", raising=False)
    assert specialists.researcher.build(FakeModel(["ok"])).run_sync("x").text == "ok"
    # extend() keeps the default; explicit tools replace it
    assert specialists.researcher.extend("r2", add="Be brief.").settings["default_tools"] is not None


def test_fetch_refuses_private_addresses_in_every_spelling():
    for url in ["http://10.0.0.5/", "http://192.168.1.1/", "http://169.254.169.254/latest/meta-data", "http://[::1]/",
                "http://[::ffff:127.0.0.1]/", "http://[fd00::1]/", "http://[64:ff9b::7f00:1]/"]:
        out = call(web.fetch(), "read_page", {"url": url})
        assert out["status"] == "error" and "private network" in json.dumps(out), url

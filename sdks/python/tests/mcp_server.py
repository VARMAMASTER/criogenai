"""A scripted MCP server for the tests (design 0022): `python mcp_server.py
modern|legacy` speaks stdio; `serve_http(mode)` serves Streamable HTTP on a
local port. Never the network."""

import http.server
import json
import sys
import threading

VERSION = "2026-07-28"
TOOLS = [
    {"name": "echo", "description": "Says it back.",
     "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
     "annotations": {"readOnlyHint": True}},
    {"name": "files.read", "description": "Reads a file.",
     "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}},
    {"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object"},
     "annotations": {"destructiveHint": True}},
]


class Fake:
    def __init__(self, mode):
        self.mode = mode
        self.initialized = False

    def handle(self, msg):
        method = msg.get("method")
        if "id" not in msg:
            if method == "notifications/initialized":
                self.initialized = True
            return None
        mid = msg["id"]
        params = msg.get("params") or {}
        legacy = self.mode == "legacy"

        def ok(result):
            return {"jsonrpc": "2.0", "id": mid, "result": result}

        def err(code, message):
            return {"jsonrpc": "2.0", "id": mid, "error": {"code": code, "message": message}}

        if method == "server/discover":
            if legacy:
                return err(-32601, "Method not found")
            return ok({"supportedVersions": [VERSION], "capabilities": {"tools": {}},
                       "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "py-fake", "version": "1"}},
                       "instructions": "Use echo."})
        if method == "initialize" and legacy:
            return ok({"protocolVersion": params.get("protocolVersion"), "capabilities": {"tools": {}},
                       "serverInfo": {"name": "py-legacy", "version": "1"}})
        if not legacy and (params.get("_meta") or {}).get("io.modelcontextprotocol/protocolVersion") != VERSION:
            return err(-32600, "missing _meta")
        if legacy and not self.initialized:
            return err(-32002, "not initialized")
        if method == "tools/list":
            return ok({"tools": TOOLS})
        if method == "tools/call":
            name, args = params.get("name"), params.get("arguments") or {}
            if name == "echo":
                return ok({"content": [{"type": "text", "text": args.get("text", "")}]})
            if name == "files.read":
                return ok({"content": [], "structuredContent": {"path": args.get("path"), "size": 3}})
            if name == "fail":
                return ok({"content": [{"type": "text", "text": "disk full"}], "isError": True})
            return err(-32602, f"unknown tool {name}")
        return err(-32601, "Method not found")


def serve_stdio(mode):
    fake = Fake(mode)
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        reply = fake.handle(msg)
        if reply is not None:
            sys.stdout.write(json.dumps(reply) + "\n")
            sys.stdout.flush()


def serve_http(mode):
    """Starts a server; returns (url, seen headers list, shutdown)."""
    fake = Fake(mode)
    seen = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            msg = json.loads(self.rfile.read(int(self.headers["content-length"])))
            seen.append({k.lower(): v for k, v in self.headers.items()})
            reply = fake.handle(msg)
            data = b"" if reply is None else json.dumps(reply).encode()
            self.send_response(202 if reply is None else 200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_port}/mcp", seen, server.shutdown


if __name__ == "__main__":
    serve_stdio(sys.argv[1] if len(sys.argv) > 1 else "modern")

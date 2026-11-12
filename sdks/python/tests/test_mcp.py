"""MCP servers' tools (design 0022), against a scripted local server only."""

import asyncio
import os
import sys

import pytest

from criogenai import Agent, CriogenError, FakeModel, FakeReply, ToolError, mcp

sys.path.insert(0, os.path.dirname(__file__))
from mcp_server import serve_http  # noqa: E402

SERVER = os.path.join(os.path.dirname(__file__), "mcp_server.py")
TOKEN = "Bearer py-mcp-secret-7781"


def run(coro):
    return asyncio.run(coro)


def test_a_stdio_server_gives_an_agent_its_tools():
    async def main():
        async with await mcp.stdio(sys.executable, [SERVER, "modern"], prefix="fs") as server:
            assert server.info["protocol_version"] == "2026-07-28"
            assert server.instructions == "Use echo."
            assert [t.name for t in server.tools] == ["fs_echo", "fs_files_read", "fs_fail"]
            # Hints are the server's claims: not trusted by default.
            assert not server.tool("fs_echo").read_only
            assert server.tool("fs_fail").destructive
            agent = Agent(
                FakeModel([FakeReply.tool_call("fs_echo", {"text": "hello"}), "done"]),
                tools=[server],
            )
            result = await agent.run("go")
            tool_messages = [m for m in result.messages if m["role"] == "tool"]
            assert tool_messages[0]["content"][0]["text"] == "hello"
            assert await server.call("fs_files_read", {"path": "/a"}) == {"path": "/a", "size": 3}
            with pytest.raises(ToolError, match="disk full"):
                await server.call("fs_fail")

    run(main())


def test_one_tool_of_a_server_and_legacy_servers():
    async def main():
        server = await mcp.stdio(sys.executable, [SERVER, "legacy"], trust_annotations=True)
        try:
            assert server.info["protocol_version"] == "2025-11-25"
            assert server.tool("echo").read_only
            agent = Agent(
                FakeModel([FakeReply.tool_call("echo", {"text": "only me"}), "done"]),
                tools=[server.tool("echo")],
            )
            result = await agent.run("go")
            assert result.tool_calls == 1
            assert await server.tool("echo")(text="direct") == "direct"
        finally:
            await server.close()

    run(main())


def test_an_http_server_sends_the_protocol_headers_and_keeps_the_token_private():
    url, seen, shutdown = serve_http("modern")
    try:
        async def main():
            server = await mcp.http(url, headers={"Authorization": TOKEN}, exclude=["fail"])
            assert "secret" not in repr(server)
            assert [t.name for t in server.tools] == ["echo", "files_read"]
            assert await server.call("echo", {"text": "over http"}) == "over http"
            last = seen[-1]
            assert last["mcp-method"] == "tools/call"
            assert last["mcp-name"] == "echo"
            assert last["authorization"] == TOKEN

        run(main())
    finally:
        shutdown()


def test_failures_have_stable_codes():
    async def main():
        with pytest.raises(CriogenError) as error:
            await mcp.stdio("criogenai-no-such-command-xyz")
        assert error.value.code == "mcp_connect"
        with pytest.raises(CriogenError) as error:
            await mcp.http("http://example.com/mcp", headers={"Authorization": TOKEN})
        assert error.value.code == "mcp_connect"
        assert "secret" not in str(error.value)

    run(main())

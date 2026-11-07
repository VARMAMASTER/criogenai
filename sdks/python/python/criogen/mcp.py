"""Tools from MCP servers (design 0022).

    from criogen import Agent, mcp

    async with await mcp.stdio("npx", ["-y", "@modelcontextprotocol/server-filesystem", "."]) as files:
        agent = Agent("openai:gpt-5-mini", tools=[files])          # all its tools
        # or tools=[files.tool("read_file")], or [*files.tools[:3]]

    docs = await mcp.http("https://mcp.example.com/mcp",
                          headers={"Authorization": f"Bearer {token}"}, prefix="docs")

The client runs in the Rust engine: it speaks MCP 2026-07-28 and falls back
to the ``initialize`` handshake of older servers; stdio servers are started
as a process (and started again if they exit). It only uses tools: a server
cannot sample your model or read your files.

What a server says is data: names are mapped to names models accept (set
``prefix`` to keep two servers apart), schemas and messages are bounded, and
its ``readOnlyHint`` / ``idempotentHint`` decide nothing unless
``trust_annotations=True``. Header and env values never appear in a repr or
an error.
"""

import asyncio
import json
from typing import Any, Dict, List, Mapping, Optional, Sequence

from . import _native
from .tools import ToolError

__all__ = ["McpServer", "McpTool", "stdio", "http"]


class McpTool:
    """One tool of a connected server; pass it in ``Agent(tools=[...])``."""

    def __init__(self, server: "McpServer", spec: Dict[str, Any]):
        self.server = server
        self.name: str = spec["name"]
        self.description: str = spec.get("description") or ""
        self.schema: Dict[str, Any] = spec.get("schema") or {}
        self.read_only: bool = bool(spec.get("read_only"))
        self.destructive: bool = bool(spec.get("destructive"))
        self.idempotent: bool = bool(spec.get("idempotent"))

    async def __call__(self, **args: Any) -> Any:
        return await self.server.call(self.name, args)

    def __repr__(self) -> str:
        return f"McpTool({self.name!r}, server={self.server.name!r})"


class McpServer:
    """A connected MCP server. Get one with :func:`stdio` or :func:`http`.

    Pass it (all its tools) or some of its :attr:`tools` to ``Agent``. Close
    it with :meth:`close` or ``async with``; a stdio server also stops when
    the last agent using it and this object are gone.
    """

    def __init__(self, spec: Dict[str, Any]):
        self._spec = spec
        self._native = _native.NativeMcp()
        self.name: str = spec.get("name") or spec.get("prefix") or spec.get("command") or spec.get("url") or "server"
        self.info: Dict[str, Any] = {}
        self.tools: List[McpTool] = []

    def _load(self, described: Dict[str, Any]) -> None:
        self.info = described.get("info") or {}
        self.tools = [McpTool(self, t) for t in described.get("tools") or []]

    async def _connect(self) -> "McpServer":
        self._load(await self._native.connect(asyncio.get_running_loop(), json.dumps(self._spec)))
        return self

    @property
    def instructions(self) -> Optional[str]:
        """The server's instructions for models (not added to any prompt)."""
        return self.info.get("instructions")

    def tool(self, name: str) -> McpTool:
        """The tool the agent sees as ``name``."""
        for t in self.tools:
            if t.name == name:
                return t
        raise KeyError(f"no tool named {name!r}; this server has {[t.name for t in self.tools]}")

    async def refresh(self) -> None:
        """Lists the tools again (agents already built keep theirs)."""
        self._load(await self._native.refresh(asyncio.get_running_loop()))

    async def call(self, name: str, args: Optional[Mapping[str, Any]] = None) -> Any:
        """Calls a tool directly. Returns its text, its JSON value, or its
        content blocks; raises :class:`criogen.ToolError` when the tool fails."""
        out = await self._native.call(asyncio.get_running_loop(), name, json.dumps(dict(args or {})))
        if not out.get("ok"):
            raise ToolError(out.get("error") or "the tool failed")
        if "text" in out:
            return out["text"]
        if "value" in out:
            return out["value"]
        return out.get("content")

    async def close(self) -> None:
        """Stops the server (a process: stdin closed, killed after 2 s)."""
        await self._native.close(asyncio.get_running_loop())

    async def __aenter__(self) -> "McpServer":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.close()

    def __repr__(self) -> str:
        return f"McpServer({self.name!r}, tools={len(self.tools)})"


def _common(
    spec: Dict[str, Any],
    name: Optional[str],
    prefix: Optional[str],
    timeout: Optional[float],
    connect_timeout: Optional[float],
    max_message_bytes: Optional[int],
    trust_annotations: bool,
    only: Optional[Sequence[str]],
    exclude: Sequence[str],
) -> Dict[str, Any]:
    spec.update(
        name=name,
        prefix=prefix,
        timeout_secs=timeout,
        connect_timeout_secs=connect_timeout,
        max_message_bytes=max_message_bytes,
        trust_annotations=trust_annotations,
        only=list(only) if only is not None else None,
        exclude=list(exclude),
    )
    return {k: v for k, v in spec.items() if v is not None}


async def stdio(
    command: str,
    args: Sequence[str] = (),
    *,
    env: Optional[Mapping[str, str]] = None,
    cwd: Optional[str] = None,
    name: Optional[str] = None,
    prefix: Optional[str] = None,
    timeout: Optional[float] = None,
    connect_timeout: Optional[float] = None,
    max_message_bytes: Optional[int] = None,
    trust_annotations: bool = False,
    only: Optional[Sequence[str]] = None,
    exclude: Sequence[str] = (),
) -> McpServer:
    """Starts ``command`` with ``args`` as an MCP server (stdio) and lists
    its tools. It inherits this process's environment plus ``env``.
    ``timeout``: seconds per call (default 60); ``connect_timeout``: seconds
    to be ready (default 30); ``only`` / ``exclude``: the server's tool names."""
    spec = {"transport": "stdio", "command": command, "args": list(args), "env": dict(env or {}), "cwd": cwd}
    server = McpServer(
        _common(spec, name, prefix, timeout, connect_timeout, max_message_bytes, trust_annotations, only, exclude)
    )
    return await server._connect()


async def http(
    url: str,
    *,
    headers: Optional[Mapping[str, str]] = None,
    name: Optional[str] = None,
    prefix: Optional[str] = None,
    timeout: Optional[float] = None,
    connect_timeout: Optional[float] = None,
    max_message_bytes: Optional[int] = None,
    trust_annotations: bool = False,
    only: Optional[Sequence[str]] = None,
    exclude: Sequence[str] = (),
) -> McpServer:
    """Connects to the MCP server at ``url`` (Streamable HTTP) and lists its
    tools. Use ``https``; plain ``http`` only to this computer when
    ``headers`` are set (they usually hold a token)."""
    spec = {"transport": "http", "url": url, "headers": dict(headers or {})}
    server = McpServer(
        _common(spec, name, prefix, timeout, connect_timeout, max_message_bytes, trust_annotations, only, exclude)
    )
    return await server._connect()

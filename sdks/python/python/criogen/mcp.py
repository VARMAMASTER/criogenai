"""Tools from MCP servers (Model Context Protocol), for agents.

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
    """One tool of a connected MCP server; pass it in ``Agent(tools=[...])`` to give an agent only this tool.

    Get it from :attr:`McpServer.tools` or :meth:`McpServer.tool`. Calling it
    (``await tool(text="hi")``) calls the server directly, like
    :meth:`McpServer.call`.

    Its attributes:

    - ``server``: the :class:`McpServer` it belongs to.
    - ``name``: the name the agent sees (with the prefix, mapped to characters
      models accept).
    - ``description``: the server's description of the tool (cut to 8,000
      characters).
    - ``schema``: the tool's input JSON Schema.
    - ``read_only``: the server says the tool changes nothing (believed only
      with ``trust_annotations=True``; else ``False``).
    - ``destructive``: the server says the tool may delete or overwrite data.
    - ``idempotent``: the server says calling it twice is safe (believed only
      with ``trust_annotations=True``, or when it is read-only; else ``False``).

    Args:
        server: The server. Internal: tools are made when the server
            connects.
        spec: The tool as the server described it.
    """

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

    Its attributes:

    - ``name``: the server's name, as given, else the prefix, the command or
      the URL.
    - ``info``: what the server said about itself: ``name``, ``version``,
      ``protocol_version`` and ``instructions``.
    - ``tools``: its tools (:class:`McpTool`), after ``only`` and ``exclude``.

    Args:
        spec: The connection settings. Internal: use :func:`stdio` or
            :func:`http`, which also connect.

    Example:
        import sys
        from criogen import Agent, mcp

        async with await mcp.stdio(sys.executable, ["my_mcp_server.py"], prefix="files") as files:
            agent = Agent("openai:gpt-5-mini", tools=[files])                   # every tool
            reader = Agent("openai:gpt-5-mini", tools=[files.tool("files_read")])  # one tool
            result = await agent.run("List the files.")
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
        """The tool the agent sees as ``name``.

        Args:
            name: The tool's name with the prefix, as in :attr:`tools`.

        Raises:
            KeyError: No such tool (the message lists the names).
        """
        for t in self.tools:
            if t.name == name:
                return t
        raise KeyError(f"no tool named {name!r}; this server has {[t.name for t in self.tools]}")

    async def refresh(self) -> None:
        """Lists the tools again (agents already built keep theirs)."""
        self._load(await self._native.refresh(asyncio.get_running_loop()))

    async def call(self, name: str, args: Optional[Mapping[str, Any]] = None) -> Any:
        """Calls a tool directly, without an agent.

        Args:
            name: The tool's name as the agent sees it (with the prefix).
            args: The tool's arguments. Default none.

        Returns:
            The tool's text, its JSON value, or its content blocks.

        Raises:
            ToolError: The tool reported an error.

        Example:
            text = await server.call("files_read", {"path": "notes.txt"})
        """
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
    """Starts an MCP server as a process (speaking MCP on stdin and stdout) and lists its tools.

    The process inherits this process's environment, plus ``env``.

    Args:
        command: The program to start, e.g. ``"npx"`` or ``sys.executable``.
        args: Its arguments.
        env: Environment variables to add or override for the process (their
            values never appear in a repr or an error).
        cwd: The process's working directory. Default: this process's.
        name: The server's name in errors and in ``repr``. Default: the
            prefix, else the command or the URL.
        prefix: The agent sees each tool as ``<prefix>_<tool>``; use it to
            keep two servers' tools apart.
        timeout: The longest one request (a tool call) may take, in seconds.
            Default 60.
        connect_timeout: The longest connecting and listing the tools may
            take, in seconds. Default 30.
        max_message_bytes: The largest message read from the server, in
            bytes. Default 4 MiB.
        trust_annotations: Believe the server's ``readOnlyHint`` and
            ``idempotentHint`` (they decide whether a failed call is
            retried). Default ``False``: the hints are the server's own
            claims.
        only: Take only these tools (the server's names, before ``prefix``).
        exclude: Leave these tools out (the server's names).

    Returns:
        The connected :class:`McpServer`.

    Raises:
        CriogenError: The server could not be started or answered something
            unusable: ``mcp_connect``, ``mcp_protocol``, ``mcp_timeout``,
            ``mcp_tool_name`` (two tools map to the same name) or
            ``mcp_tool_schema``.

    Example:
        from criogen import Agent, mcp

        files = await mcp.stdio("npx", ["-y", "@modelcontextprotocol/server-filesystem", "."], prefix="fs")
        try:
            agent = Agent("openai:gpt-5-mini", tools=[files])
            result = await agent.run("Which files are here?")
        finally:
            await files.close()
    """
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
    """Connects to an MCP server over Streamable HTTP and lists its tools.

    Use ``https``; plain ``http`` is refused when ``headers`` are set, except
    to this computer (headers usually hold a token).

    Args:
        url: The server's MCP endpoint, e.g. ``"https://mcp.example.com/mcp"``.
        headers: Headers sent with every request, e.g. ``Authorization``
            (their values never appear in a repr or an error).
        name: The server's name in errors and in ``repr``. Default: the
            prefix, else the command or the URL.
        prefix: The agent sees each tool as ``<prefix>_<tool>``; use it to
            keep two servers' tools apart.
        timeout: The longest one request (a tool call) may take, in seconds.
            Default 60.
        connect_timeout: The longest connecting and listing the tools may
            take, in seconds. Default 30.
        max_message_bytes: The largest message read from the server, in
            bytes. Default 4 MiB.
        trust_annotations: Believe the server's ``readOnlyHint`` and
            ``idempotentHint`` (they decide whether a failed call is
            retried). Default ``False``: the hints are the server's own
            claims.
        only: Take only these tools (the server's names, before ``prefix``).
        exclude: Leave these tools out (the server's names).

    Returns:
        The connected :class:`McpServer`.

    Raises:
        CriogenError: The server could not be reached or answered something
            unusable: ``mcp_connect``, ``mcp_protocol``, ``mcp_timeout``,
            ``mcp_tool_name`` or ``mcp_tool_schema``.

    Example:
        import os
        from criogen import Agent, mcp

        docs = await mcp.http(
            "https://mcp.example.com/mcp",
            headers={"Authorization": f"Bearer {os.environ['DOCS_MCP_TOKEN']}"},
            prefix="docs",
        )
        agent = Agent("openai:gpt-5-mini", tools=[docs])
    """
    spec = {"transport": "http", "url": url, "headers": dict(headers or {})}
    server = McpServer(
        _common(spec, name, prefix, timeout, connect_timeout, max_message_bytes, trust_annotations, only, exclude)
    )
    return await server._connect()

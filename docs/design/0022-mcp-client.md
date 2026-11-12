# 0022: MCP client: tools from any MCP server

Status: approved by the user's request (2026-11-07, roadmap three, task 21).
Built in Rust (`criogenai_agent::mcp`, feature `mcp`), exposed in Python and
TypeScript.

## Why

The Model Context Protocol is how tool servers are shared: GitHub, file
systems, databases, browsers, internal company services. One client gives a
CriogenAI agent every one of them, so we do not need an integration catalogue
of our own. The client only **uses** tools; CriogenAI does not serve MCP here.

## The protocol we speak

Sources: the MCP specification, revision 2026-07-28 (modelcontextprotocol.io
`/specification/2026-07-28/`: basic, versioning, server/discover,
server/tools, transports), and 2025-11-25 for older servers.

- **Messages** are JSON-RPC 2.0. A modern request carries in `params._meta`:
  `io.modelcontextprotocol/protocolVersion` (`"2026-07-28"`),
  `io.modelcontextprotocol/clientCapabilities` (`{}`: we offer no sampling,
  roots or elicitation) and `io.modelcontextprotocol/clientInfo`
  (`{name: "criogenai", version}`). Modern servers are stateless: no
  handshake, no session.
- **Discovery.** `server/discover` returns the versions and capabilities a
  modern server supports. An older server answers it with an error (usually
  `-32601`): we then do the legacy handshake, `initialize` with
  `protocolVersion` `"2025-11-25"`, followed by the
  `notifications/initialized` notification. We accept 2025-11-25,
  2025-06-18, 2025-03-26 and 2024-11-05 from `initialize`; anything else is
  an error naming the version.
- **Tools.** `tools/list` (with `cursor` / `nextCursor` pages) and
  `tools/call` (`name`, `arguments`). A result holds `content` blocks (text,
  image, audio, resource link, embedded resource), optional
  `structuredContent`, and `isError`. A modern result may carry
  `resultType`; anything but `"complete"` (or none) is not supported in this
  slice and becomes a tool error the model sees.
- **Transports.**
  - *stdio* (servers only): we start the command, write one JSON message per
    line to its stdin and read one per line from its stdout. Its stderr is
    its log: we keep the last 20 lines (each cut to 500 characters) to show
    in an error when it dies, and nothing else. Lines on stdout that are not
    JSON are skipped. Shutdown: close stdin, wait up to 2 s, kill. If the
    process exits, the next call starts it again (once per call).
  - *Streamable HTTP* (servers and the browser, through the engine's
    `Transport`): each message is a POST with `Accept: application/json,
    text/event-stream`, `Content-Type: application/json` and, on modern
    servers, `MCP-Protocol-Version`, `Mcp-Method` and, for `tools/call`,
    `Mcp-Name`. A tool's input schema may mark a property with
    `x-mcp-header: "Name"`: its value is also sent as `Mcp-Param-Name`
    (base64 as `=?base64?…?=` when it is not plain printable ASCII). The
    answer is a JSON body or an SSE stream; we read events until the one
    with our request's id. Legacy servers get `MCP-Protocol-Version` with the
    negotiated version and the `Mcp-Session-Id` the server gave at
    `initialize`; a 404 on a session restarts it once. A probe that fails
    with 401 or 403 is an error, never a reason to fall back.
- **Requests from the server** (stdio, legacy servers): `ping` is answered
  `{}`, anything else `-32601`. We never let a server sample our model or
  read our files in this slice.
- **Cancellation.** When the run is cancelled or the call times out, stdio
  sends `notifications/cancelled` with the request id; HTTP drops the
  request (which closes its stream).

## The API

Rust:

```rust
use criogenai_agent::{Agent, mcp::McpServer};

let github = McpServer::stdio("npx", ["-y", "@modelcontextprotocol/server-github"])
    .env("GITHUB_TOKEN", token)
    .prefix("github")
    .connect()
    .await?;
let docs = McpServer::http("https://mcp.example.com/mcp")
    .header("Authorization", format!("Bearer {key}"))
    .connect()
    .await?;

let agent = Agent::builder(model)
    .tools(github.tools())
    .tools(docs.tools())
    .build()?;
```

- `McpServer` is the configuration: `stdio(command, args)` or `http(url)`;
  `env`, `cwd`, `header`, `prefix`, `timeout` (each request, default
  60 s), `connect_timeout` (default 30 s), `max_message_bytes` (default
  4 MiB), `trust_annotations`, `only` / `except` (tool names; `exclude` in
  the SDKs, where `except` is a keyword in Python).
- `connect()` starts or reaches the server, negotiates the version, lists the
  tools and returns an `McpClient`. `client.tools()` returns them as agent
  tools (`Arc<dyn Tool>`, `ToolKind::Mcp`); `client.refresh()` lists them
  again; `client.call(name, args)` calls one directly; `client.close()`
  stops a stdio server (dropping the last handle does it too). `info()`
  gives the server's name, version, protocol version and `instructions`.
- `AgentBuilder::tools(iter)` adds several shared tools at once.

Python: `server = await criogenai.mcp.stdio("npx", [...], env=..., prefix=...)`
or `await criogenai.mcp.http(url, headers=...)`; `server.tools` goes into
`Agent(tools=[...])`; `await server.close()`; `async with` closes it.
TypeScript: `const server = await mcp.stdio("npx", [...], {...})` or
`await mcp.http(url, {...})`; `server.tools`, `await server.close()`. In
`criogenai/web` only `mcp.http` exists; `mcp.stdio` throws `unsupported`.

## Mapping tools

- **Names.** A server's tool names may use `.` and be 128 characters long;
  model APIs accept `[A-Za-z0-9_-]{1,64}`. The agent sees
  `<prefix>_<name>` (when a prefix is set) with every other character turned
  into `_` and cut to 64 characters; the server always gets its own name. Two
  tools that map to the same name, or a name that maps to nothing, make
  `connect` fail with `mcp_tool_name` (name the conflict, suggest `prefix`
  or `except`).
- **Schemas.** `inputSchema` must be an object schema of at most 64 levels
  and 256 KB; a tool that breaks this is refused at `connect`
  (`mcp_tool_schema`), not silently dropped. Our validator checks
  arguments before the call as for any tool; it resolves only local `$ref`s,
  so a schema never makes us fetch anything.
- **Descriptions** are the server's words, cut to 8 000 characters. The title
  is used when there is no description.
- **Annotations are hints from the server, so they are not trusted by
  default.** `destructiveHint: true` is kept (it only makes us more
  careful). `readOnlyHint` and `idempotentHint` decide whether we retry a
  call, so they are ignored unless `trust_annotations(true)`.
- **Limits.** At most 1 000 tools and 100 `tools/list` pages per server.

## Results and errors

- `content` → the tool message: text → text, image → image (base64), audio →
  audio, resource link → a line `resource: <uri> (<name>)`, embedded
  resource → its text, or a line naming it if it is binary.
- No `content` but `structuredContent` → a JSON block.
- `isError: true` → a `ToolError` the model sees (its text), as the spec
  asks: the model can correct itself.
- A JSON-RPC error, a malformed answer, a timeout, a dead process or an HTTP
  failure → a `ToolError` the model sees, saying which server failed and how
  (`MCP server "github": …`), never with a header value or env value in it.
- At `connect`, failures are `McpError` with stable codes: `mcp_connect`
  (could not start or reach it), `mcp_protocol` (bad answer, unsupported
  version), `mcp_tool_name`, `mcp_tool_schema`, `mcp_timeout`, `mcp_closed`.
  The SDKs raise them as errors with these codes.

## Bounds (rule 11)

Every message read is limited (`max_message_bytes`, stdio lines and HTTP
bodies), every request has a timeout, the number of tools and pages is
capped, and a schema's depth and size are checked. Requests in flight per
server: 64 (others wait). stderr is kept to 20 short lines.

## Secrets (rule 10)

Header values and env values are never in `Debug`, an error or an event.
Headers are sent over `https`, or plain `http` only to this computer.

## Not in this slice

Resources and prompts, sampling, elicitation, roots, OAuth (pass a token in a
header), `input_required` results, `notifications/tools/list_changed`
(call `refresh`), the deprecated HTTP+SSE transport, and serving MCP.

## Tests

A scripted MCP server in the tests (modern and legacy modes), never the
network: stdio through the test binary started again as the server, HTTP on
a local socket. Covered: discovery and fallback, pages, name mapping and
conflicts, schema limits, results of every kind, `isError`, protocol errors,
timeouts, cancellation, restart after a crash, oversized messages, headers
(`Mcp-Method`, `Mcp-Name`, `Mcp-Param-*`, session id), and that no secret
appears in an error. The SDKs test the same against a small script server.

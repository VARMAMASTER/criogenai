// Tools from MCP servers (design 0022).
//
//   import { Agent, mcp } from "criogen";
//   const files = await mcp.stdio("npx", ["-y", "@modelcontextprotocol/server-filesystem", "."]);
//   const agent = new Agent({ model, tools: [files] });          // all its tools
//   // or tools: [files.tool("read_file")]
//   await files.close();
//
// The client runs in the Rust engine: it speaks MCP 2026-07-28 and falls back
// to the `initialize` handshake of older servers; stdio servers are started as
// a process (and again if they exit). It only uses tools: a server cannot
// sample your model or read your files. In criogen/web only `mcp.http` works.
//
// What a server says is data: names are mapped to names models accept (set
// `prefix` to keep two servers apart), schemas and messages are bounded, and
// its readOnlyHint / idempotentHint decide nothing unless `trustAnnotations`.
// Header and env values never appear in an error.

import { errorFor, translate } from "./errors";
import { native } from "./native";
import { ToolError } from "./tools";

/** Options for every MCP server. */
export interface McpOptions {
  /** The server's name in errors (default: the prefix, else the command or host). */
  name?: string;
  /** The agent sees `<prefix>_<tool>`: keeps two servers' tools apart. */
  prefix?: string;
  /** Seconds one call may take. Default 60. */
  timeoutSecs?: number;
  /** Seconds to be ready. Default 30. */
  connectTimeoutSecs?: number;
  /** The largest message read from the server. Default 4 MiB. */
  maxMessageBytes?: number;
  /** Believe the server's readOnlyHint / idempotentHint (they decide retries). Default false. */
  trustAnnotations?: boolean;
  /** Only these tools (the server's names). */
  only?: string[];
  /** Leave these tools out (the server's names). */
  exclude?: string[];
}

/** A server this app starts (not in the browser). */
export interface StdioOptions extends McpOptions {
  /** Added to this process's environment. */
  env?: Record<string, string>;
  cwd?: string;
}

/** A server at a URL (Streamable HTTP). */
export interface HttpOptions extends McpOptions {
  /** Sent with every request, e.g. `{ Authorization: "Bearer …" }`. */
  headers?: Record<string, string>;
}

/** What the server said about itself. */
export interface McpInfo {
  name: string | null;
  version: string | null;
  protocolVersion: string;
  /** Its instructions for models (not added to any prompt). */
  instructions: string | null;
}

interface Described {
  handle: number;
  info: { name: string | null; version: string | null; protocol_version: string; instructions: string | null };
  tools: { name: string; description: string; schema: Record<string, unknown>; read_only: boolean; destructive: boolean; idempotent: boolean }[];
}

interface McpAddon {
  mcpConnect(spec: string): Promise<string>;
  mcpRefresh(handle: number): Promise<string>;
  mcpCall(handle: number, name: string, args: string): Promise<string>;
  mcpClose(handle: number): Promise<void>;
}

function engine(): McpAddon {
  const addon = native as unknown as Partial<McpAddon>;
  if (typeof addon.mcpConnect !== "function") {
    throw errorFor(
      "unsupported",
      "MCP needs criogen's engine 0.1.5 or newer, and the engine package installed for this system is older",
    );
  }
  return addon as McpAddon;
}

/** One tool of a connected server; put it in an agent's `tools`. */
export class McpTool {
  readonly name: string;
  readonly description: string;
  readonly schema: Record<string, unknown>;
  readonly readOnly: boolean;
  readonly destructive: boolean;
  readonly idempotent: boolean;

  constructor(
    readonly server: McpServer,
    spec: Described["tools"][number],
  ) {
    this.name = spec.name;
    this.description = spec.description;
    this.schema = spec.schema;
    this.readOnly = spec.read_only;
    this.destructive = spec.destructive;
    this.idempotent = spec.idempotent;
  }

  /** Calls it directly (see `McpServer.call`). */
  call(args: Record<string, unknown> = {}): Promise<unknown> {
    return this.server.call(this.name, args);
  }
}

/** A connected MCP server: get one with `mcp.stdio` or `mcp.http`. */
export class McpServer {
  /** @internal The engine's handle. */
  handle = 0;
  info: McpInfo = { name: null, version: null, protocolVersion: "", instructions: null };
  tools: McpTool[] = [];

  /** @internal */
  constructor(readonly name: string) {}

  /** @internal */
  load(raw: string): this {
    const described = JSON.parse(raw) as Described;
    this.handle = described.handle;
    const i = described.info;
    this.info = { name: i.name, version: i.version, protocolVersion: i.protocol_version, instructions: i.instructions };
    this.tools = described.tools.map((t) => new McpTool(this, t));
    return this;
  }

  /** The server's instructions for models (not added to any prompt). */
  get instructions(): string | null {
    return this.info.instructions;
  }

  /** The tool the agent sees as `name`. */
  tool(name: string): McpTool {
    const found = this.tools.find((t) => t.name === name);
    if (!found) throw new Error(`no tool named "${name}"; this server has ${this.tools.map((t) => t.name).join(", ")}`);
    return found;
  }

  /** Lists the tools again (agents already built keep theirs). */
  async refresh(): Promise<void> {
    try {
      this.load(await engine().mcpRefresh(this.handle));
    } catch (error) {
      throw translate(error);
    }
  }

  /** Calls a tool directly: its text, its JSON value, or its content blocks. Throws ToolError when the tool fails. */
  async call(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    let out: { ok: boolean; error?: string; text?: string; value?: unknown; content?: unknown };
    try {
      out = JSON.parse(await engine().mcpCall(this.handle, name, JSON.stringify(args)));
    } catch (error) {
      throw translate(error);
    }
    if (!out.ok) throw new ToolError(out.error ?? "the tool failed");
    if ("text" in out) return out.text;
    if ("value" in out) return out.value;
    return out.content;
  }

  /** Stops the server (a process: stdin closed, killed after 2 s). */
  async close(): Promise<void> {
    await engine().mcpClose(this.handle);
  }

  /** `await using server = await mcp.stdio(...)` closes it. */
  async [Symbol.asyncDispose ?? Symbol.for("Symbol.asyncDispose")](): Promise<void> {
    await this.close();
  }
}

function common(spec: Record<string, unknown>, options: McpOptions): string {
  return JSON.stringify({
    ...spec,
    name: options.name,
    prefix: options.prefix,
    timeout_secs: options.timeoutSecs,
    connect_timeout_secs: options.connectTimeoutSecs,
    max_message_bytes: options.maxMessageBytes,
    trust_annotations: options.trustAnnotations ?? false,
    only: options.only,
    exclude: options.exclude ?? [],
  });
}

async function connect(name: string, spec: string): Promise<McpServer> {
  const addon = engine();
  try {
    return new McpServer(name).load(await addon.mcpConnect(spec));
  } catch (error) {
    throw translate(error);
  }
}

/** Starts `command` with `args` as an MCP server (stdio) and lists its tools. Servers only. */
export function stdio(command: string, args: string[] = [], options: StdioOptions = {}): Promise<McpServer> {
  const spec = common({ transport: "stdio", command, args, env: options.env ?? {}, cwd: options.cwd }, options);
  return connect(options.name ?? options.prefix ?? command, spec);
}

/** Connects to the MCP server at `url` (Streamable HTTP) and lists its tools. Use https; plain http only to this computer when headers are set. */
export function http(url: string, options: HttpOptions = {}): Promise<McpServer> {
  const spec = common({ transport: "http", url, headers: options.headers ?? {} }, options);
  return connect(options.name ?? options.prefix ?? url, spec);
}

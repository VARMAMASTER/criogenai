// Tools (design 0012 §2): JavaScript functions the engine calls. The engine
// validates the model's arguments against `input` (JSON Schema) before the
// function runs.

/** A failure the model should see (and may recover from), unlike a crash. */
export class ToolError extends Error {
  readonly retryable: boolean;

  constructor(message: string, options: { retryable?: boolean } = {}) {
    super(message);
    this.name = "ToolError";
    this.retryable = options.retryable ?? false;
  }
}

/** What a tool can know about its call. */
export interface ToolContext {
  callId: string;
  runId: string;
}

export type JsonSchema = Record<string, unknown>;

export interface ToolDefinition<Args = Record<string, unknown>> {
  name: string;
  description: string;
  /** JSON Schema of the arguments (an object schema). */
  input: JsonSchema;
  run: (args: Args, ctx: ToolContext) => unknown | Promise<unknown>;
  readOnly?: boolean;
  destructive?: boolean;
  idempotent?: boolean;
}

/** A function the model can call. Made by `tool({...})`. */
export class Tool<Args = Record<string, unknown>> {
  readonly definition: ToolDefinition<Args>;

  constructor(definition: ToolDefinition<Args>) {
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(definition.name)) {
      throw new TypeError(`tool name "${definition.name}": letters, digits, _ and - only (1–64)`);
    }
    if (typeof definition.run !== "function") throw new TypeError(`tool "${definition.name}" needs a run function`);
    this.definition = definition;
  }

  get name(): string {
    return this.definition.name;
  }

  spec(): Record<string, unknown> {
    const d = this.definition;
    return {
      name: d.name,
      description: d.description,
      input: d.input,
      readOnly: d.readOnly ?? false,
      destructive: d.destructive ?? false,
      idempotent: d.idempotent ?? false,
    };
  }

  /**
   * What the engine calls: request JSON in, envelope JSON out. A crash (an
   * exception other than ToolError) is recorded in `crashes` by call id when
   * the agent fails runs on crashes, so its `tool_failed` error can carry it
   * as `cause`; otherwise the engine returns it to the model.
   */
  adapter(crashes: Map<string, unknown> | null): (request: string) => Promise<string> {
    return async (request: string) => {
      const { args, callId, runId } = JSON.parse(request) as { args: Args; callId: string; runId: string };
      try {
        const value = await this.definition.run(args, { callId, runId });
        return JSON.stringify({ ok: value === undefined ? null : value });
      } catch (error) {
        if (error instanceof ToolError) {
          return JSON.stringify({ toolError: error.message, retryable: error.retryable });
        }
        crashes?.set(callId, error);
        const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
        return JSON.stringify({ crash: text });
      }
    };
  }
}

/** Defines a tool. */
export function tool<Args = Record<string, unknown>>(definition: ToolDefinition<Args>): Tool<Args> {
  return new Tool(definition);
}

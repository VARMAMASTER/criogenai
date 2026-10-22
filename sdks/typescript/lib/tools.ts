// Tools (design 0012 §2): JavaScript functions the engine calls. The engine
// validates the model's arguments against the tool's JSON Schema before the
// function runs; a Standard Schema input (Zod, Valibot, ArkType; design 0017
// §6) then validates them too.

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

/** One problem a Standard Schema found. */
export interface StandardIssue {
  readonly message: string;
  readonly path?: ReadonlyArray<PropertyKey | { readonly key: PropertyKey }> | undefined;
}

/** What a Standard Schema's `validate` returns. */
export type StandardResult<Output> =
  | { readonly value: Output; readonly issues?: undefined }
  | { readonly issues: ReadonlyArray<StandardIssue> };

/**
 * A schema from a library that implements Standard Schema v1
 * (standardschema.dev): Zod 4, Valibot, ArkType and others.
 */
export interface StandardSchema<Input = unknown, Output = Input> {
  readonly "~standard": {
    readonly version: 1;
    readonly vendor: string;
    readonly validate: (value: unknown) => StandardResult<Output> | Promise<StandardResult<Output>>;
    readonly types?: { readonly input: Input; readonly output: Output } | undefined;
  };
}

export interface ToolDefinition<Args = Record<string, unknown>> {
  name: string;
  description: string;
  /**
   * The arguments: a JSON Schema (an object schema), or a Standard Schema
   * (e.g. a Zod object). With a Standard Schema, `run` receives what it
   * outputs (defaults, transforms), and its issues go back to the model.
   */
  input: JsonSchema | StandardSchema<any, Args>;
  /**
   * The JSON Schema the model sees, for a Standard Schema whose library
   * cannot produce one itself (Valibot: `toJsonSchema` from
   * `@valibot/to-json-schema`). Zod 4 and ArkType produce their own.
   */
  jsonSchema?: JsonSchema;
  run: (args: Args, ctx: ToolContext) => unknown | Promise<unknown>;
  readOnly?: boolean;
  destructive?: boolean;
  idempotent?: boolean;
}

function isStandard(input: unknown): input is StandardSchema<unknown, unknown> {
  const std = (input as any)?.["~standard"];
  return typeof std === "object" && std !== null && std.version === 1 && typeof std.validate === "function";
}

/** The JSON Schema a Standard Schema's library makes (Standard JSON Schema), if it can. */
function standardJsonSchema(schema: StandardSchema<unknown, unknown>): JsonSchema | undefined {
  const make = (schema["~standard"] as any).jsonSchema?.input;
  if (typeof make !== "function") return undefined;
  const { $schema, ...rest } = make({ target: "draft-2020-12" }) as JsonSchema;
  return rest;
}

/** `orderId: Invalid string; too many`. */
function describeIssues(issues: ReadonlyArray<StandardIssue>): string {
  return issues
    .map((issue) => {
      const path = (issue.path ?? []).map((p) => String(typeof p === "object" ? p.key : p)).join(".");
      return path ? `${path}: ${issue.message}` : issue.message;
    })
    .join("; ");
}

/** A function the model can call. Made by `tool({...})`. */
export class Tool<Args = Record<string, unknown>> {
  readonly definition: ToolDefinition<Args>;
  /** The JSON Schema of the arguments, as the model and the engine see it. */
  readonly jsonSchema: JsonSchema;
  private readonly standard: StandardSchema<unknown, unknown> | undefined;

  constructor(definition: ToolDefinition<Args>) {
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(definition.name)) {
      throw new TypeError(`tool name "${definition.name}": letters, digits, _ and - only (1–64)`);
    }
    if (typeof definition.run !== "function") throw new TypeError(`tool "${definition.name}" needs a run function`);
    this.definition = definition;
    if (isStandard(definition.input)) {
      this.standard = definition.input;
      const schema = definition.jsonSchema ?? standardJsonSchema(definition.input);
      if (!schema) {
        throw new TypeError(
          `tool "${definition.name}": this ${definition.input["~standard"].vendor} schema cannot make a JSON Schema; ` +
            "pass one as jsonSchema (for Valibot: toJsonSchema from @valibot/to-json-schema)",
        );
      }
      this.jsonSchema = schema;
    } else {
      this.standard = undefined;
      this.jsonSchema = definition.jsonSchema ?? (definition.input as JsonSchema);
    }
  }

  get name(): string {
    return this.definition.name;
  }

  spec(): Record<string, unknown> {
    const d = this.definition;
    return {
      name: d.name,
      description: d.description,
      input: this.jsonSchema,
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
      const { args, callId, runId } = JSON.parse(request) as { args: unknown; callId: string; runId: string };
      try {
        let input = args as Args;
        if (this.standard) {
          const checked = await this.standard["~standard"].validate(args);
          if (checked.issues) return JSON.stringify({ invalidArguments: describeIssues(checked.issues) });
          input = checked.value as Args;
        }
        const value = await this.definition.run(input, { callId, runId });
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

/** Defines a tool. With a Standard Schema `input`, `Args` is what it outputs. */
export function tool<Args = Record<string, unknown>>(definition: ToolDefinition<Args>): Tool<Args> {
  return new Tool(definition);
}

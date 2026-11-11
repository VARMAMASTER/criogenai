// Tools (design 0012 §2): JavaScript functions the engine calls. The engine
// validates the model's arguments against the tool's JSON Schema before the
// function runs; a Standard Schema input (Zod, Valibot, ArkType; design 0017
// §6) then validates them too.

/**
 * A failure the model should see (and may recover from), unlike a crash.
 *
 * Throw it from a tool's `run` when the call cannot succeed as asked (an
 * unknown id, a closed account): the model receives `message` as the tool's
 * error result and the run goes on. Any other exception is a crash, handled
 * as the agent's `onToolException` option says.
 *
 * @example
 * import { tool, ToolError } from "criogen";
 *
 * const lookup = tool<{ orderId: string }>({
 *   name: "lookup",
 *   description: "Find an order by id.",
 *   input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
 *   run: ({ orderId }) => {
 *     throw new ToolError(`no order ${orderId}`); // the model sees "no order …"
 *   },
 * });
 */
export class ToolError extends Error {
  /** Whether trying the same call again may work; see the constructor. */
  readonly retryable: boolean;

  /**
   * @param message What the model sees as the tool's error.
   * @param options `retryable` (default false): the call may work if tried
   *   again. Tools marked `readOnly` or `idempotent` are then retried
   *   (the agent's `toolRetry`); the model also sees the flag.
   */
  constructor(message: string, options: { retryable?: boolean } = {}) {
    super(message);
    this.name = "ToolError";
    this.retryable = options.retryable ?? false;
  }
}

/** What a tool can know about its call: the second argument of `run`. */
export interface ToolContext {
  /** The id of this tool call, as in the transcript and in approval requests. */
  callId: string;
  /** The id of the run that made the call (`result.runId`). */
  runId: string;
}

/** A JSON Schema, as a plain object (e.g. `{ type: "object", properties: {...}, required: [...] }`). */
export type JsonSchema = Record<string, unknown>;

/** One problem a Standard Schema found. */
export interface StandardIssue {
  /** What is wrong, in the schema library's words. */
  readonly message: string;
  /** Where in the value the problem is (property names and array indexes). */
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
  /** The Standard Schema interface the library provides (version, vendor, validate). */
  readonly "~standard": {
    readonly version: 1;
    readonly vendor: string;
    readonly validate: (value: unknown) => StandardResult<Output> | Promise<StandardResult<Output>>;
    readonly types?: { readonly input: Input; readonly output: Output } | undefined;
  };
}

/** What `tool({...})` takes: the name, description and input schema the model sees, and the function to run. */
export interface ToolDefinition<Args = Record<string, unknown>> {
  /** The name the model calls the tool by: letters, digits, `_` and `-`, 1 to 64 characters. */
  name: string;
  /** What the tool does and when to use it; the model reads this to decide. */
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
  /**
   * The function, called with the checked arguments and a `ToolContext`.
   * Its return value (or what its Promise resolves to) is the result the
   * model sees, as JSON, so return something `JSON.stringify` accepts;
   * `undefined` means no output. Throw `ToolError` for a failure the model
   * should see.
   */
  run: (args: Args, ctx: ToolContext) => unknown | Promise<unknown>;
  /**
   * The tool has no side effects. Read-only and idempotent tools are retried
   * after a timeout or a retryable `ToolError` (the agent's `toolRetry`).
   * @default false
   */
  readOnly?: boolean;
  /**
   * The tool may destroy data. A hint for people and policies; to require a
   * human decision, list the tool in the agent's `approve` option.
   * @default false
   */
  destructive?: boolean;
  /**
   * Calling the tool twice with the same arguments has the same effect as
   * once, so it is safe to retry (see `readOnly`).
   * @default false
   */
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
  /** The definition the tool was made from. */
  readonly definition: ToolDefinition<Args>;
  /** The JSON Schema of the arguments, as the model and the engine see it. */
  readonly jsonSchema: JsonSchema;
  private readonly standard: StandardSchema<unknown, unknown> | undefined;

  /**
   * Same as `tool(definition)`.
   * @param definition The name, description, input schema and `run` function.
   * @throws {TypeError} The name has characters other than letters, digits, `_` and `-` (or is not 1 to 64 long), `run` is not a function, or a Standard Schema cannot make a JSON Schema and none was given as `jsonSchema`.
   */
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

  /** The tool's name, from its definition. */
  get name(): string {
    return this.definition.name;
  }

  /** The tool as the engine receives it: name, description, the JSON Schema of its input and the three hints. */
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
   * @internal What the engine calls: request JSON in, envelope JSON out. A crash (an
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

/**
 * Defines a tool. With a Standard Schema `input`, `Args` is what it outputs.
 *
 * The engine checks the model's arguments against the input schema before
 * `run` is called; arguments that do not match go back to the model as an
 * error, and `run` is not called. Give the tool to an agent in its `tools`
 * option.
 *
 * @param definition The name, description, input schema and `run` function.
 * @returns A `Tool` for an agent's `tools`.
 * @throws {TypeError} An invalid name, a missing `run`, or a Standard Schema with no JSON Schema.
 * @example
 * import { tool } from "criogen";
 *
 * const weather = tool<{ city: string }>({
 *   name: "get_weather",
 *   description: "Current weather for a city.",
 *   input: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
 *   readOnly: true,
 *   run: async ({ city }, ctx) => ({ city, forecast: "sunny", callId: ctx.callId }),
 * });
 * @example
 * // With Zod 4 (or another Standard Schema library): the model sees its JSON Schema.
 * import { z } from "zod";
 * import { tool } from "criogen";
 *
 * const order = tool({
 *   name: "order",
 *   description: "Orders an item.",
 *   input: z.object({ sku: z.string(), qty: z.number().int().min(1).default(1) }),
 *   run: ({ sku, qty }) => `ordered ${qty} x ${sku}`,
 * });
 */
export function tool<Args = Record<string, unknown>>(definition: ToolDefinition<Args>): Tool<Args> {
  return new Tool(definition);
}

// Hooks (design 0017 §5): code that runs around the agent loop, in order.
//
// A hook is an object with some of these methods (each may be async):
//
// - `onStart(ctx)`: once per run segment; may return `{ state }`.
// - `beforeModel(ctx)`: before each model call; may replace the transcript
//   (`{ messages }`) or end the run (`{ end: true, output }`).
// - `afterModel(ctx)`: after each model reply (`ctx.messages` ends with it);
//   may replace it (`{ message }`) or end the run.
// - `beforeTool(call)`: before each tool call; may return a changed
//   `ToolCall`, a `ToolResult` to skip the tool, or throw `ToolError`.
// - `afterTool(call, result)`: after each tool call; a value other than
//   `undefined` replaces the result.
// - `onEnd(ctx, outcome)`: always, once per segment ("completed",
//   "suspended" or "failed"); cannot change the outcome.
//
// `onStart`, `beforeModel` and `beforeTool` run in list order, the others in
// reverse. A throw in `onStart`, `beforeModel` or `afterModel` fails the run
// with `hook_failed` (what was thrown is its `cause`); one in `beforeTool` /
// `afterTool` counts as the tool's (see `onToolException`).

import { ToolError } from "./tools";

/** What a hook sees. */
export interface HookContext {
  /** The id of the run (`result.runId`). */
  runId: string;
  /** The transcript so far (as in `result.messages`). */
  messages: Record<string, unknown>[];
  /** Model calls made so far in this run. */
  modelCalls: number;
  /** What this hook saved with `{ state }`, if anything. */
  hookState: unknown;
  /** The whole run state, read-only. */
  state: Record<string, unknown>;
}

/**
 * What a hook wants: save its `state`; replace the transcript (`messages`,
 * `beforeModel`) or the new reply (`message`, `afterModel`); or `end` the
 * run, with `output`.
 */
export interface HookAction {
  /** A value to keep for this hook; later calls see it as `ctx.hookState`. */
  state?: unknown;
  /** `beforeModel` only: the transcript to send instead (and to keep). */
  messages?: Record<string, unknown>[];
  /** `afterModel` only: the assistant message to use instead of the model's reply. */
  message?: Record<string, unknown>;
  /** `true` ends the run now, with stop reason "hook_end". */
  end?: boolean;
  /** With `end`: the run's output. */
  output?: unknown;
}

/** A tool call going through `beforeTool` (change `name` or `args` and return it). */
export interface ToolCall {
  /** The call's id, as in the transcript. */
  callId: string;
  /** The tool the model called. Return a call with another name to run that tool instead. */
  name: string;
  /** The arguments the model wrote. */
  args: Record<string, unknown>;
}

/**
 * Returned by `beforeTool`: use `value` as the result and skip the tool (e.g. a cached answer).
 *
 * @example
 * import { ToolResult, type Hook } from "criogenai";
 *
 * const cache = new Map<string, unknown>([["get_weather:Paris", { forecast: "sunny" }]]);
 * const cached: Hook = {
 *   beforeTool(call) {
 *     const hit = cache.get(`${call.name}:${call.args.city}`);
 *     if (hit !== undefined) return new ToolResult(hit);
 *   },
 * };
 */
export class ToolResult {
  /** @param value The tool result the model sees, as JSON. */
  constructor(readonly value: unknown) {}
}

type Maybe<T> = T | undefined | void | Promise<T | undefined | void>;

/**
 * Code that runs around the agent loop: an object (or class instance) with
 * any of the methods below, each of which may be async. Pass hooks in the
 * agent's `hooks` option.
 *
 * `onStart`, `beforeModel` and `beforeTool` run in list order, the others in
 * reverse. A throw in `onStart`, `beforeModel` or `afterModel` fails the run
 * (code `hook_failed`; what was thrown is the error's `cause`); a throw in
 * `beforeTool` or `afterTool` counts as the tool's (see the agent's
 * `onToolException`).
 *
 * @example
 * import { Agent, type Hook } from "criogenai";
 *
 * const logTools: Hook = {
 *   name: "log-tools",
 *   beforeTool(call) {
 *     console.log("calling", call.name);
 *   },
 *   onEnd(ctx, outcome) {
 *     console.log(`run ${ctx.runId}: ${outcome} after ${ctx.modelCalls} model calls`);
 *   },
 * };
 * const agent = new Agent({ model: "openai:gpt-5-mini", hooks: [logTools] });
 */
export interface Hook {
  /** Shown in errors and audit records; defaults to the class name. */
  name?: string;
  /** Once per run segment (a run, or its continuation after approval or retry), before anything else; may return `{ state }`. */
  onStart?(ctx: HookContext): Maybe<HookAction>;
  /** Before each model call; may replace the transcript (`{ messages }`) or end the run (`{ end: true, output }`). */
  beforeModel?(ctx: HookContext): Maybe<HookAction>;
  /** After each model reply (`ctx.messages` ends with it); may replace it (`{ message }`) or end the run. */
  afterModel?(ctx: HookContext): Maybe<HookAction>;
  /**
   * Before each tool call: return a changed `ToolCall` to run that instead,
   * a `ToolResult` to skip the tool, or nothing to go on. Throwing
   * `ToolError` gives the model that error instead of running the tool.
   */
  beforeTool?(call: ToolCall): Maybe<ToolCall | ToolResult>;
  /** After each tool call; a value other than `undefined` replaces the result the model sees. */
  afterTool?(call: ToolCall, result: unknown): unknown;
  /** Always, once per run segment, with how it ended: "completed", "suspended" (waiting for approval) or "failed". Cannot change the outcome. */
  onEnd?(ctx: HookContext, outcome: "completed" | "suspended" | "failed"): void | Promise<void>;
}

/** Counts throws, for keys unique to each. */
let throws = 0;

const STAGES = {
  on_start: "onStart",
  before_model: "beforeModel",
  after_model: "afterModel",
  before_tool: "beforeTool",
  after_tool: "afterTool",
  on_end: "onEnd",
} as const;

type Stage = keyof typeof STAGES;

/**
 * @internal `{ spec, adapter }` for the engine: `adapter` takes
 * `{stage, payload}` and returns an envelope. What a hook throws is kept in
 * `crashes` under `hook:<name>:<stage>`; in a tool stage, under the call id
 * and only if the agent fails runs on crashes (as for tools).
 */
export function hookAdapter(
  hook: Hook,
  crashes: Map<string, unknown>,
  keepToolCrashes: boolean,
): { spec: { name: string; stages: string[] }; adapter: (request: string) => Promise<string> } {
  if (hook === null || typeof hook !== "object") throw new TypeError(`${String(hook)} is not a hook`);
  const stages = (Object.keys(STAGES) as Stage[]).filter((s) => typeof (hook as any)[STAGES[s]] === "function");
  if (stages.length === 0) {
    throw new TypeError(`${String(hook)} is not a hook: it has none of ${Object.values(STAGES).join(", ")}`);
  }
  const name = hook.name ?? (hook.constructor?.name && hook.constructor !== Object ? hook.constructor.name : "hook");

  const adapter = async (request: string): Promise<string> => {
    const { stage, payload } = JSON.parse(request) as { stage: Stage; payload: any };
    const method = (hook as any)[STAGES[stage]].bind(hook);
    try {
      switch (stage) {
        case "before_tool": {
          const value = await method({ callId: payload.callId, name: payload.name, args: payload.args ?? {} });
          if (value === undefined) return "{}";
          if (value instanceof ToolResult) return JSON.stringify({ result: value.value ?? null });
          if (value && typeof value === "object" && typeof value.name === "string") {
            return JSON.stringify({ call: { name: value.name, args: value.args ?? {} } });
          }
          throw new TypeError(`beforeTool returns a ToolCall, a ToolResult or nothing, not ${String(value)}`);
        }
        case "after_tool": {
          const call = payload.call;
          const value = await method({ callId: call.callId, name: call.name, args: call.args ?? {} }, payload.result);
          return value === undefined ? "{}" : JSON.stringify({ result: value });
        }
        case "on_end": {
          const { outcome, ...ctx } = payload;
          await method(ctx, outcome);
          return "{}";
        }
        default: {
          const value = await method(payload);
          if (value === undefined) return "{}";
          if (value === null || typeof value !== "object") {
            throw new TypeError(`${STAGES[stage]} returns a HookAction or nothing, not ${String(value)}`);
          }
          return JSON.stringify(value);
        }
      }
    } catch (error) {
      if (error instanceof ToolError && stage === "before_tool") {
        return JSON.stringify({ toolError: error.message });
      }
      if (stage === "before_tool" || stage === "after_tool") {
        if (keepToolCrashes) crashes.set(stage === "before_tool" ? payload.callId : payload.call.callId, error);
      }
      // A key unique to this throw: the run error repeats it, so a run gets
      // its own exception and never one thrown for a run at the same time.
      let causeKey: string | undefined;
      if (stage !== "on_end" && stage !== "before_tool" && stage !== "after_tool") {
        causeKey = `hook:${name}:${stage}:${++throws}`;
        crashes.set(causeKey, error);
      }
      const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
      return JSON.stringify({ crash: text, causeKey });
    }
  };
  return { spec: { name, stages }, adapter };
}

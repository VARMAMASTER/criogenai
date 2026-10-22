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
  runId: string;
  /** The transcript so far (as in `result.messages`). */
  messages: Record<string, unknown>[];
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
  state?: unknown;
  messages?: Record<string, unknown>[];
  message?: Record<string, unknown>;
  end?: boolean;
  output?: unknown;
}

/** A tool call going through `beforeTool` (change `name` or `args` and return it). */
export interface ToolCall {
  callId: string;
  name: string;
  args: Record<string, unknown>;
}

/** Returned by `beforeTool`: use `value` as the result and skip the tool (e.g. a cached answer). */
export class ToolResult {
  constructor(readonly value: unknown) {}
}

type Maybe<T> = T | undefined | void | Promise<T | undefined | void>;

export interface Hook {
  /** Shown in errors and audit records; defaults to the class name. */
  name?: string;
  onStart?(ctx: HookContext): Maybe<HookAction>;
  beforeModel?(ctx: HookContext): Maybe<HookAction>;
  afterModel?(ctx: HookContext): Maybe<HookAction>;
  beforeTool?(call: ToolCall): Maybe<ToolCall | ToolResult>;
  afterTool?(call: ToolCall, result: unknown): unknown;
  onEnd?(ctx: HookContext, outcome: "completed" | "suspended" | "failed"): void | Promise<void>;
}

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
      } else if (stage !== "on_end") {
        crashes.set(`hook:${name}:${stage}`, error);
      }
      const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
      return JSON.stringify({ crash: text });
    }
  };
  return { spec: { name, stages }, adapter };
}

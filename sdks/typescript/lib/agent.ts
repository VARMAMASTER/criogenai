// Agents and threads (design 0012 §2) over the Rust engine.

import { native, NativeAgent, NativeStream } from "./native";
import { translate } from "./errors";
import { Model, modelSpec } from "./models";
import { Memory, Storage } from "./storage";
import { JsonSchema, Tool } from "./tools";

export interface ApprovalRequest {
  call_id: string;
  tool: string;
  args: Record<string, unknown>;
  description?: string;
  allowed?: string[];
}

/** A reviewer's answer to one call (see `Thread.decide`). */
export type Decision =
  | { type: "approve" }
  | { type: "reject"; message?: string }
  | { type: "edit"; args: Record<string, unknown>; name?: string }
  | { type: "respond"; message: string };

export const Decision = {
  approve: (): Decision => ({ type: "approve" }),
  reject: (message?: string): Decision => ({ type: "reject", message }),
  edit: (args: Record<string, unknown>, name?: string): Decision => ({ type: "edit", args, name }),
  respond: (message: string): Decision => ({ type: "respond", message }),
};

/** What a run produced. */
export interface RunResult<Output = unknown> {
  text: string | null;
  output: Output | null;
  messages: Record<string, unknown>[];
  usage: Record<string, number>;
  stopReason: string | null;
  needsApproval: boolean;
  approvals: ApprovalRequest[];
  modelCalls: number;
  toolCalls: number;
  runId: string;
}

/** A thread call's result, saved as the thread's new head. */
export interface Reply<Output = unknown> extends RunResult<Output> {
  version: number;
  checkpointId: string;
}

export interface AgentOptions {
  model: Model;
  instructions?: string;
  tools?: Tool<any>[];
  /** Tools that need human approval before they run. */
  approve?: string[];
  /** JSON Schema of the final answer (`result.output`). */
  output?: JsonSchema;
  limits?: { modelCalls?: number; toolCalls?: number; tokens?: number; toolTimeoutSecs?: number };
  /**
   * When a tool throws an exception that is not a ToolError: "returnToModel"
   * (default; the model sees its name and message, the run goes on),
   * "returnGeneric" (the model only learns that the tool failed; the text
   * stays in the result's `artifact`) or "fail" (the run fails with
   * ToolExecutionError, the exception as its `cause`).
   */
  onToolException?: "returnToModel" | "returnGeneric" | "fail";
  /** A Storage, a URL ("sqlite:///x.db", "postgres://…"), or in memory. */
  storage?: Storage | string;
  memory?: Memory;
}

export interface CallOptions {
  /** Aborting cancels the run (a thread is still saved, and retry continues it). */
  signal?: AbortSignal;
}

export interface RunOptions extends CallOptions {
  /** Run state (e.g. `{ user_id: "ada" }` for per-user memory). */
  state?: Record<string, unknown>;
}

/** One streamed event. */
export type StreamEvent =
  | { type: "text"; messageId: string; text: string }
  | { type: "tool_call_started"; callId: string; tool: string; args: Record<string, unknown> }
  | { type: "tool_call_finished"; callId: string; tool: string; status: string }
  | { type: "message"; message: Record<string, unknown> }
  | { type: "approval_needed"; requests: ApprovalRequest[] }
  | { type: "other"; event: string };

/** Runs `call` with a cancellation id that `signal` fires. */
async function cancellable<T>(signal: AbortSignal | undefined, call: (cancelId: number) => Promise<T>): Promise<T> {
  const id = native.newCancelId();
  if (signal?.aborted) native.cancelCall(id);
  const onAbort = () => native.cancelCall(id);
  signal?.addEventListener("abort", onAbort, { once: true });
  try {
    return await call(id);
  } finally {
    signal?.removeEventListener("abort", onAbort);
    native.releaseCancelId(id);
  }
}

/** A model with tools, instructions and (optionally) approval, typed output, storage and memory. The loop runs in Rust. */
export class Agent<Output = unknown> {
  readonly storage: Storage;
  /** @internal */
  readonly native: NativeAgent;
  /** @internal What crashing tools threw, by call id. */
  readonly crashes = new Map<string, unknown>();

  constructor(options: AgentOptions) {
    const tools = options.tools ?? [];
    for (const t of tools) {
      if (!(t instanceof Tool)) throw new TypeError(`${String(t)} is not a tool: define it with tool({...})`);
    }
    this.storage = options.storage instanceof Storage ? options.storage : new Storage({ url: options.storage });
    const config = {
      model: modelSpec(options.model),
      instructions: options.instructions,
      tools: tools.map((t) => t.spec()),
      approve: options.approve ?? [],
      output: options.output,
      limits: options.onToolException ? { ...options.limits, onToolException: options.onToolException } : options.limits,
      memory: options.memory?.spec,
    };
    try {
      this.native = new native.NativeAgent(
        JSON.stringify(config),
        tools.map((t) => t.adapter(options.onToolException === "fail" ? this.crashes : null)),
        this.storage.native,
      );
    } catch (error) {
      throw translate(error);
    }
  }

  /** Runs once (no thread). */
  async run(input: string, options: RunOptions = {}): Promise<RunResult<Output>> {
    const payload = JSON.stringify({ input, state: options.state ?? {} });
    try {
      return JSON.parse(await cancellable(options.signal, (id) => this.native.run(payload, id)));
    } catch (error) {
      throw translate(error, this.crashes);
    }
  }

  /** The conversation `threadId` (created on first use); `actor` names who acts, for the audit trail. */
  thread(threadId: string, options: { actor?: string } = {}): Thread<Output> {
    return new Thread(this, threadId, options.actor);
  }
}

/**
 * A streamed thread call: `for await (const event of stream)`, then
 * `await stream.reply()`. Leaving the loop early cancels the run (it is
 * still saved, and retry continues it).
 */
export class Stream<Output = unknown> implements AsyncIterable<StreamEvent> {
  #native: NativeStream;
  #agent: Agent<Output>;
  #done = false;

  /** @internal */
  constructor(nativeStream: NativeStream, agent: Agent<Output>) {
    this.#native = nativeStream;
    this.#agent = agent;
  }

  async *[Symbol.asyncIterator](): AsyncIterator<StreamEvent> {
    try {
      for (;;) {
        const data = await this.#native.next();
        if (data === null) {
          this.#done = true;
          return;
        }
        yield JSON.parse(data) as StreamEvent;
      }
    } finally {
      if (!this.#done) this.#native.cancel();
    }
  }

  /** Waits for the call; its saved reply. */
  async reply(): Promise<Reply<Output>> {
    try {
      return JSON.parse(await this.#native.finish());
    } catch (error) {
      throw translate(error, this.#agent.crashes);
    }
  }

  /** Cancels the run. */
  cancel(): void {
    this.#native.cancel();
  }
}

/** One conversation, kept in the agent's storage. */
export class Thread<Output = unknown> {
  constructor(
    readonly agent: Agent<Output>,
    readonly id: string,
    readonly actor?: string,
  ) {}

  private async op<T>(op: string, payload: Record<string, unknown> = {}, signal?: AbortSignal): Promise<T> {
    const body = JSON.stringify({ ...payload, threadId: this.id, actor: this.actor });
    try {
      return JSON.parse(await cancellable(signal, (id) => this.agent.native.thread(op, body, id)));
    } catch (error) {
      throw translate(error, this.agent.crashes);
    }
  }

  /** Runs one turn: the stored conversation plus `input`. */
  send(input: string, options: RunOptions = {}): Promise<Reply<Output>> {
    return this.op("send", { input, state: options.state ?? {} }, options.signal);
  }

  /** Approves every pending call and continues. */
  approve(options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("approve", {}, options.signal);
  }

  /** Rejects every pending call (the model sees `message`). */
  reject(message = "rejected", options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("reject", { message }, options.signal);
  }

  /** One Decision per pending call, by call id. */
  decide(decisions: Record<string, Decision>, options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("decide", { decisions }, options.signal);
  }

  /** Continues a failed or interrupted run. */
  retry(options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("retry", {}, options.signal);
  }

  /** Checkpoints, newest first. */
  history(limit = 20): Promise<Record<string, unknown>[]> {
    return this.op("history", { limit });
  }

  /** The stored conversation. */
  messages(): Promise<Record<string, unknown>[]> {
    return this.op("messages");
  }

  /** Makes an older checkpoint current again. */
  rewind(checkpointId: string): Promise<Record<string, unknown>> {
    return this.op("rewind", { checkpointId });
  }

  /** Deletes the thread. `true` if it existed. */
  delete(): Promise<boolean> {
    return this.op("delete");
  }

  private stream_(op: string, payload: Record<string, unknown>): Stream<Output> {
    const body = JSON.stringify({ ...payload, threadId: this.id, actor: this.actor });
    try {
      return new Stream(this.agent.native.stream(op, body), this.agent);
    } catch (error) {
      throw translate(error);
    }
  }

  /** `send`, streaming its events. */
  stream(input: string, options: { state?: Record<string, unknown> } = {}): Stream<Output> {
    return this.stream_("send", { input, state: options.state ?? {} });
  }

  streamApprove(): Stream<Output> {
    return this.stream_("approve", {});
  }

  streamReject(message = "rejected"): Stream<Output> {
    return this.stream_("reject", { message });
  }

  streamDecide(decisions: Record<string, Decision>): Stream<Output> {
    return this.stream_("decide", { decisions });
  }

  streamRetry(): Stream<Output> {
    return this.stream_("retry", {});
  }
}

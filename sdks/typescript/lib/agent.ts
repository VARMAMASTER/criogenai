// Agents and threads (design 0012 §2) over the Rust engine.

import { CollectionTool } from "./collections";
import { McpServer, McpTool } from "./mcp";
import { type RunFacts, withRunFacts } from "./usage";
import { native, NativeAgent, NativeStream } from "./native";
import { translate } from "./errors";
import { Hook, hookAdapter } from "./hooks";
import { type Redactor, redactionHook } from "./redact";
import { Model, modelSpec } from "./models";
import { Memory, Storage } from "./storage";
import { JsonSchema, Tool } from "./tools";

/**
 * A tool call waiting for a reviewer's decision. A run that stops for
 * approval lists these in `reply.approvals`; answer them with
 * `thread.approve()`, `thread.reject()` or `thread.decide()`.
 */
export interface ApprovalRequest {
  /** The tool call's id. `thread.decide` takes decisions keyed by it. */
  call_id: string;
  /** The name of the tool the model wants to call. */
  tool: string;
  /** The arguments the model wrote for the call. */
  args: Record<string, unknown>;
  /** Text for the reviewer, when the approval rule provides one. */
  description?: string;
  /** The decisions the reviewer may make: "approve", "edit", "reject", "respond". */
  allowed?: string[];
}

/**
 * A reviewer's answer to one call (see `Thread.decide`).
 *
 * Approve it, reject it (the model sees `message`), run it with edited
 * `args` (and optionally another tool `name`), or skip it and give the
 * model `message` as the tool's result. `Decision.approve()`,
 * `Decision.reject(message)`, `Decision.edit(args, name)` and
 * `Decision.respond(message)` build them.
 */
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

/** What a run produced; `durationMs`, `answeredBy` and `models` say which models answered and how long it took. */
export interface RunResult<Output = unknown> extends RunFacts {
  /** The final answer as text, or `null` when the output is not text (or the run stopped for approval). */
  text: string | null;
  /** The final answer: the parsed JSON object with the `output` option, else the final text. */
  output: Output | null;
  /** The whole transcript: user, assistant and tool messages, in order. */
  messages: Record<string, unknown>[];
  /** Tokens across all model calls: input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens. */
  usage: Record<string, number>;
  /**
   * Why the run ended: "final_answer", "output" (typed output accepted),
   * "return_direct", "content_filter", "hook_end" or "limit"; `null` while
   * the run waits for approval.
   */
  stopReason: string | null;
  /** `true` when the run stopped because a tool call needs approval (see `approvals`). */
  needsApproval: boolean;
  /** The calls waiting for a decision; empty unless `needsApproval`. */
  approvals: ApprovalRequest[];
  /** Model calls the run made (retries of one call do not count). */
  modelCalls: number;
  /** Tool calls the run executed. */
  toolCalls: number;
  /** The run's id, also passed to tools (`ctx.runId`) and hooks. */
  runId: string;
}

/** A thread call's result, saved as the thread's new head. */
export interface Reply<Output = unknown> extends RunResult<Output> {
  /** The thread's version after this call; it grows with every saved checkpoint. */
  version: number;
  /** The id of the checkpoint this call saved; `thread.rewind` takes it. */
  checkpointId: string;
}

/** How often and how patiently to retry (design 0017 §2); missing fields keep the defaults. */
export interface Retry {
  /** Attempts in total, including the first. Default 3. */
  maxAttempts?: number;
  /** Backoff before the second attempt, in seconds. Default 0.5. */
  initialBackoffSecs?: number;
  /** Multiplier for each further attempt. Default 2. */
  factor?: number;
  /** Upper bound on any backoff, in seconds. Default 20. */
  maxBackoffSecs?: number;
  /** Wait a random part of the backoff. Default true. */
  jitter?: boolean;
}

/** An agent as a tool of another agent (`agent.asTool`): it takes `{ input }` and returns the sub-agent's answer. */
export class AgentTool {
  /**
   * Prefer `agent.asTool(name, description)`, which builds the same object.
   * @param agent The agent that runs when the tool is called.
   * @param name The tool name the calling model sees.
   * @param description What the tool does, for the calling model.
   */
  constructor(
    readonly agent: Agent<any>,
    readonly name: string,
    readonly description: string,
  ) {}
}

/** Everything an `Agent` is built from. Only `model` is required. */
export interface AgentOptions {
  /**
   * The model: a "provider:model" string such as "openai:gpt-5-mini" (the key
   * comes from the provider's environment variable), or a model object
   * (`openai(...)`, `azure(...)`, `FakeModel`, `FunctionModel`, ...).
   */
  model: Model;
  /** The system message the model sees on every call. */
  instructions?: string;
  /** Tools: `tool({...})`, `agent.asTool(...)`, an MCP server (all its tools) or one of its tools. */
  tools?: (Tool<any> | AgentTool | McpServer | McpTool | CollectionTool)[];
  /**
   * Names of tools that need human approval before they run. A call to one
   * stops the run (`reply.needsApproval`); use a thread to approve, reject or
   * edit it.
   */
  approve?: string[];
  /** JSON Schema of the final answer (`result.output`). */
  output?: JsonSchema;
  /**
   * Bounds on one run: `modelCalls` (default 25; retries of one call do not
   * count), `toolCalls` (default 200), `tokens` (input plus output tokens
   * across the run, checked before each model call; no limit by default) and
   * `toolTimeoutSecs` (per tool call, default 60). Reaching a limit fails the
   * run with `LimitExceededError`; a tool that times out returns an error to
   * the model instead.
   */
  limits?: { modelCalls?: number; toolCalls?: number; tokens?: number; toolTimeoutSecs?: number };
  /**
   * When a tool throws an exception that is not a ToolError: "returnToModel"
   * (default; the model sees its name and message, the run goes on),
   * "returnGeneric" (the model only learns that the tool failed; the text
   * stays in the result's `artifact`) or "fail" (the run fails with
   * ToolExecutionError, the exception as its `cause`).
   */
  onToolException?: "returnToModel" | "returnGeneric" | "fail";
  /**
   * Where threads are kept: a `Storage`, or a URL ("sqlite://x.db",
   * "postgres://…"). Without it, threads live in memory and end with the process.
   */
  storage?: Storage | string;
  /**
   * Long-term memory (`Memory.byState(...)` or `Memory.shared(...)`): facts
   * recalled before each model call, and (unless the memory's options turn
   * them off) tools for the model to remember, forget and search them. Kept
   * in the agent's storage.
   */
  memory?: Memory;
  /** Models tried in order when the previous one fails with a transient error (after its retries). */
  fallbacks?: Model[];
  /** Other error kinds that also move to the next model, e.g. "context_overflow". */
  fallbackOn?: string[];
  /** Retries of transient model errors (default: 3 attempts, backoff from 0.5 s). */
  modelRetry?: Retry;
  /** Retries of tools marked readOnly or idempotent (default: 3 attempts). */
  toolRetry?: Retry;
  /** The longest Retry-After the agent waits, in seconds (default 60). */
  maxRetryAfterSecs?: number;
  /** Exports every run of this agent (`Telemetry.langfuse()`, `Telemetry.otlp(url)`). */
  telemetry?: { readonly native: { readonly handle: number } };
  /** Code around the loop (see `Hook`): `onStart`, `beforeModel`, `afterModel`, `beforeTool`, `afterTool`, `onEnd`. */
  hooks?: Hook[];
  /**
   * Applied to every text before each model call (input, replies, tool
   * results), ahead of all hooks: e.g. strip patient identifiers before
   * anything reaches a provider. The redacted transcript is what is saved.
   */
  redact?: Redactor;
}

/** Options of every thread call that takes no input (`approve`, `reject`, `decide`, `retry`). */
export interface CallOptions {
  /** Aborting cancels the run (a thread is still saved, and retry continues it). */
  signal?: AbortSignal;
}

/** Options of `agent.run` and `thread.send`. */
export interface RunOptions extends CallOptions {
  /** Run state (e.g. `{ user_id: "ada" }` for per-user memory). */
  state?: Record<string, unknown>;
}

/**
 * One streamed event.
 *
 * `text` (a piece of the answer as it is written), `tool_call_started`,
 * `tool_call_finished`, `message` (a finished message of the transcript),
 * `approval_needed`, or `other` (engine events with no fields of their own
 * here).
 */
export type StreamEvent =
  | { type: "text"; messageId: string; text: string }
  | { type: "tool_call_started"; callId: string; tool: string; args: Record<string, unknown> }
  | { type: "tool_call_finished"; callId: string; tool: string; status: string }
  | { type: "message"; message: Record<string, unknown> }
  | { type: "approval_needed"; requests: ApprovalRequest[] }
  | { type: "other"; event: string };

/** Runs `call` with a cancellation id that `signal` fires. */
/** @internal Runs a native call that an AbortSignal can cancel. */
export async function cancellable<T>(signal: AbortSignal | undefined, call: (cancelId: number) => Promise<T>): Promise<T> {
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

/**
 * A model with tools, instructions and (optionally) approval, typed output, storage and memory. The loop runs in Rust.
 *
 * Each run calls the model, runs the tools it asks for (concurrently, in
 * call order), feeds the results back, and repeats until the model answers
 * without tools or a limit is reached. Use `run` for one-off tasks and
 * `thread(id)` for conversations that are saved between calls.
 *
 * `Output` types `result.output`; it is not checked at compile time, so
 * pass the matching JSON Schema as the `output` option.
 *
 * @example
 * import { Agent, FakeModel, FakeReply, tool } from "criogen";
 *
 * const add = tool<{ a: number; b: number }>({
 *   name: "add",
 *   description: "Add two numbers.",
 *   input: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
 *   run: ({ a, b }) => a + b,
 * });
 * const agent = new Agent({
 *   model: new FakeModel([FakeReply.toolCall("add", { a: 2, b: 3 }), "2 + 3 = 5"]), // or "openai:gpt-5-mini"
 *   instructions: "Use the add tool for arithmetic.",
 *   tools: [add],
 * });
 * const result = await agent.run("What is 2 + 3?");
 * console.log(result.text, result.toolCalls); // "2 + 3 = 5" 1
 */
export class Agent<Output = unknown> {
  /** The storage that keeps this agent's threads (in memory unless the `storage` option says otherwise). */
  readonly storage: Storage;
  /** @internal */
  readonly native: NativeAgent;
  /** @internal What crashing tools threw, by call id. */
  readonly crashes = new Map<string, unknown>();

  /**
   * @param options The model, instructions, tools and the rest (see `AgentOptions`).
   * @throws {TypeError} An entry of `tools` is not a tool, or a model string names no known provider.
   * @throws {Error} A model string's provider has no API key in the environment.
   * @throws {ValidationError} An option the engine does not accept (e.g. an unknown `fallbackOn` kind).
   */
  constructor(options: AgentOptions) {
    const all = options.tools ?? [];
    for (const t of all) {
      if (!(t instanceof Tool) && !(t instanceof AgentTool) && !(t instanceof McpServer) && !(t instanceof McpTool) && !(t instanceof CollectionTool)) {
        throw new TypeError(`${String(t)} is not a tool: define it with tool({...}), agent.asTool(...), or pass an MCP server`);
      }
    }
    const tools = all.filter((t): t is Tool<any> => t instanceof Tool);
    const agentTools = all.filter((t): t is AgentTool => t instanceof AgentTool);
    // MCP servers' tools (design 0022): a server means all its tools.
    const servers = new Map<McpServer, string[] | null>();
    for (const t of all) {
      if (t instanceof McpServer) servers.set(t, null);
      else if (t instanceof McpTool) {
        const names = servers.has(t.server) ? servers.get(t.server)! : [];
        if (names !== null) servers.set(t.server, [...names, t.name]);
      }
    }
    this.storage = options.storage instanceof Storage ? options.storage : new Storage({ url: options.storage });
    const failOnCrash = options.onToolException === "fail";
    const allHooks = options.redact ? [redactionHook(options.redact), ...(options.hooks ?? [])] : (options.hooks ?? []);
    const hooks = allHooks.map((h) => hookAdapter(h, this.crashes, failOnCrash));
    const config = {
      model: modelSpec(options.model),
      instructions: options.instructions,
      tools: tools.map((t) => t.spec()),
      approve: options.approve ?? [],
      output: options.output,
      limits: options.onToolException ? { ...options.limits, onToolException: options.onToolException } : options.limits,
      memory: options.memory?.spec,
      agentTools: agentTools.map((t) => ({ name: t.name, description: t.description, handle: t.agent.native.handle })),
      mcp: [...servers].map(([server, names]) => ({ handle: server.handle, names })),
      collections: all
        .filter((t): t is CollectionTool => t instanceof CollectionTool)
        .map((t) => ({ handle: t.collection.handle, name: t.name, description: t.description })),
      fallbacks: (options.fallbacks ?? []).map(modelSpec),
      fallbackOn: options.fallbackOn,
      modelRetry: options.modelRetry,
      toolRetry: options.toolRetry,
      maxRetryAfterSecs: options.maxRetryAfterSecs,
      telemetry: options.telemetry?.native.handle,
      hooks: hooks.map((h) => h.spec),
    };
    try {
      this.native = new native.NativeAgent(
        JSON.stringify(config),
        tools.map((t) => t.adapter(failOnCrash ? this.crashes : null)),
        this.storage.native,
        hooks.map((h) => h.adapter),
      );
    } catch (error) {
      throw translate(error);
    }
  }

  /**
   * Runs once (no thread). Nothing is saved: a run that stops for approval
   * cannot be continued, so use `thread(id).send(...)` with `approve`.
   *
   * @param input The user's message.
   * @param options `state` (run state, e.g. the key per-user memory reads) and `signal` (cancels the run).
   * @returns The answer, the transcript, token usage and call counts.
   * @throws {ModelError} The model failed after its retries and fallbacks.
   * @throws {LimitExceededError} A limit in `limits` was reached.
   * @throws {ToolExecutionError} A tool threw and `onToolException` is "fail".
   * @throws {CancelledError} `signal` was aborted.
   */
  async run(input: string, options: RunOptions = {}): Promise<RunResult<Output>> {
    const payload = JSON.stringify({ input, state: options.state ?? {} });
    try {
      const started = Date.now();
      return withRunFacts(JSON.parse(await cancellable(options.signal, (id) => this.native.run(payload, id))), started);
    } catch (error) {
      throw translate(error, this.crashes);
    }
  }

  /**
   * This agent as a tool of another agent: the tool takes `input` (text),
   * runs this agent, and returns its answer. How deep agents may call agents
   * is bounded by the nesting limit (default 8).
   *
   * @param name The tool name the calling model sees (letters, digits, `_` and `-`).
   * @param description What this agent does, so the calling model knows when to use it.
   * @example
   * import { Agent, FakeModel, FakeReply } from "criogen";
   *
   * const researcher = new Agent({ model: new FakeModel(["The order shipped on Monday."]), instructions: "Find facts." });
   * const lead = new Agent({
   *   model: new FakeModel([FakeReply.toolCall("research", { input: "When did order A-1 ship?" }), "It shipped on Monday."]),
   *   tools: [researcher.asTool("research", "Looks up facts about orders.")],
   * });
   * console.log((await lead.run("When did A-1 ship?")).text);
   */
  asTool(name: string, description: string): AgentTool {
    return new AgentTool(this, name, description);
  }

  /**
   * The conversation `threadId` (created on first use); `actor` names who acts, for the audit trail.
   *
   * Nothing is read or written until you call a method of the thread.
   *
   * @param threadId Any string that names the conversation in this agent's storage.
   * @param options `actor`: who makes the calls (a user or service id), recorded in audit records.
   * @example
   * import { Agent, FakeModel } from "criogen";
   *
   * const agent = new Agent({ model: new FakeModel(["Hi Sam.", "Your name is Sam."]) });
   * const thread = agent.thread("support-42");
   * await thread.send("My name is Sam.");
   * const reply = await thread.send("What is my name?"); // the model sees the earlier turn
   * console.log(reply.text, reply.version);
   */
  thread(threadId: string, options: { actor?: string } = {}): Thread<Output> {
    return new Thread(this, threadId, options.actor);
  }
}

/**
 * A streamed thread call: `for await (const event of stream)`, then
 * `await stream.reply()`. Leaving the loop early cancels the run (it is
 * still saved, and retry continues it).
 *
 * Made by `thread.stream(...)` and the other `thread.stream*` methods.
 *
 * @example
 * import { Agent, FakeModel } from "criogen";
 *
 * const agent = new Agent({ model: new FakeModel(["Hello there."]) });
 * const stream = agent.thread("chat-1").stream("Hello");
 * for await (const event of stream) {
 *   if (event.type === "text") process.stdout.write(event.text);
 * }
 * const reply = await stream.reply();
 */
export class Stream<Output = unknown> implements AsyncIterable<StreamEvent> {
  #native: NativeStream;
  #agent: Agent<Output>;
  #done = false;
  #started = Date.now();

  /**
   * @internal
   * @param nativeStream The engine's stream.
   * @param agent The agent the thread belongs to.
   */
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

  /**
   * Waits for the call; its saved reply.
   * @throws {CriogenError} The run failed (the same errors as `thread.send`).
   */
  async reply(): Promise<Reply<Output>> {
    try {
      return withRunFacts(JSON.parse(await this.#native.finish()), this.#started);
    } catch (error) {
      throw translate(error, this.#agent.crashes);
    }
  }

  /** Cancels the run. */
  cancel(): void {
    this.#native.cancel();
  }
}

/**
 * One conversation, kept in the agent's storage.
 *
 * Every call loads the saved conversation, runs, and saves the result as a
 * new checkpoint, so a thread survives restarts when the storage is a
 * database. A run that stops for approval waits in the thread until
 * `approve`, `reject` or `decide` continues it; a failed or cancelled run
 * is saved too, and `retry` continues it.
 *
 * @example
 * import { Agent, FakeModel, FakeReply, tool } from "criogen";
 *
 * const refund = tool<{ orderId: string }>({
 *   name: "refund",
 *   description: "Refund an order.",
 *   input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
 *   run: ({ orderId }) => `refunded ${orderId}`,
 * });
 * const agent = new Agent({
 *   model: new FakeModel([FakeReply.toolCall("refund", { orderId: "A-1" }), "The order is refunded."]),
 *   tools: [refund],
 *   approve: ["refund"],
 * });
 * const thread = agent.thread("order-A-1");
 * const first = await thread.send("Refund order A-1.");
 * console.log(first.needsApproval, first.approvals[0].tool); // true "refund"
 * const done = await thread.approve();
 * console.log(done.text); // "The order is refunded."
 */
export class Thread<Output = unknown> {
  /**
   * Prefer `agent.thread(id, { actor })`, which builds the same object.
   * @param agent The agent whose model, tools and storage the thread uses.
   * @param id The thread id in the agent's storage.
   * @param actor Who makes the calls, recorded in audit records.
   */
  constructor(
    readonly agent: Agent<Output>,
    readonly id: string,
    readonly actor?: string,
  ) {}

  private async op<T>(op: string, payload: Record<string, unknown> = {}, signal?: AbortSignal): Promise<T> {
    const body = JSON.stringify({ ...payload, threadId: this.id, actor: this.actor });
    try {
      const started = Date.now();
      return withRunFacts(JSON.parse(await cancellable(signal, (id) => this.agent.native.thread(op, body, id))), started);
    } catch (error) {
      throw translate(error, this.agent.crashes);
    }
  }

  /**
   * Runs one turn: the stored conversation plus `input`.
   *
   * @param input The user's message.
   * @param options `state` (run state, e.g. the key per-user memory reads) and `signal` (cancels the run; it is still saved).
   * @returns The reply, saved as the thread's new head.
   * @throws {ThreadSuspendedError} The thread waits for approval: answer with `approve`, `reject` or `decide` first.
   * @throws {ThreadBusyError} Another run holds the thread (see the storage's `onBusy` option).
   * @throws {ModelError} The model failed after its retries and fallbacks; `retry` continues the saved run.
   * @throws {LimitExceededError} A limit in `limits` was reached.
   * @throws {CancelledError} `signal` was aborted.
   */
  send(input: string, options: RunOptions = {}): Promise<Reply<Output>> {
    return this.op("send", { input, state: options.state ?? {} }, options.signal);
  }

  /**
   * Approves every pending call and continues.
   * @param options `signal` cancels the run.
   */
  approve(options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("approve", {}, options.signal);
  }

  /**
   * Rejects every pending call (the model sees `message`).
   * @param message What the model is told instead of each call's result.
   * @param options `signal` cancels the run.
   */
  reject(message = "rejected", options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("reject", { message }, options.signal);
  }

  /**
   * One Decision per pending call, by call id.
   *
   * @param decisions A `Decision` for each pending call, keyed by its `call_id` (from `reply.approvals`).
   * @param options `signal` cancels the run.
   * @example
   * import { Decision } from "criogen";
   *
   * const [request] = first.approvals; // first: the reply that needed approval
   * const done = await thread.decide({ [request.call_id]: Decision.reject("Refunds need a manager.") });
   */
  decide(decisions: Record<string, Decision>, options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("decide", { decisions }, options.signal);
  }

  /**
   * Continues a failed or interrupted run. Tool calls that finished (and
   * were saved) before the failure are not run again.
   * @param options `signal` cancels the run.
   */
  retry(options: CallOptions = {}): Promise<Reply<Output>> {
    return this.op("retry", {}, options.signal);
  }

  /**
   * Checkpoints, newest first.
   * @param limit The most checkpoints to return.
   * @returns One object per checkpoint of the current branch: `id`, `parent_id`, `version`, `source`, `status`, `run_id`, `step`, `created_at_ms`.
   */
  history(limit = 20): Promise<Record<string, unknown>[]> {
    return this.op("history", { limit });
  }

  /** The stored conversation. */
  messages(): Promise<Record<string, unknown>[]> {
    return this.op("messages");
  }

  /**
   * Makes an older checkpoint current again. It is saved as a new head; the
   * newer checkpoints stay stored but leave the history. The next call runs
   * from there, and nothing recorded after it is replayed.
   * @param checkpointId An `id` from `history()` or a reply's `checkpointId`.
   * @returns The new head's checkpoint, in the same shape as `history()` entries.
   */
  rewind(checkpointId: string): Promise<Record<string, unknown>> {
    return this.op("rewind", { checkpointId });
  }

  /** Deletes the thread and its checkpoints. `true` if it existed. */
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

  /**
   * `send`, streaming its events. To cancel, call `stream.cancel()` or leave
   * the `for await` loop early.
   * @param input The user's message.
   * @param options `state`: run state, as for `send`.
   */
  stream(input: string, options: { state?: Record<string, unknown> } = {}): Stream<Output> {
    return this.stream_("send", { input, state: options.state ?? {} });
  }

  /** `approve`, streaming the events of the run it continues. */
  streamApprove(): Stream<Output> {
    return this.stream_("approve", {});
  }

  /**
   * `reject`, streaming the events of the run it continues.
   * @param message What the model is told instead of each call's result.
   */
  streamReject(message = "rejected"): Stream<Output> {
    return this.stream_("reject", { message });
  }

  /**
   * `decide`, streaming the events of the run it continues.
   * @param decisions A `Decision` for each pending call, keyed by its `call_id`.
   */
  streamDecide(decisions: Record<string, Decision>): Stream<Output> {
    return this.stream_("decide", { decisions });
  }

  /** `retry`, streaming the events of the run it continues. */
  streamRetry(): Stream<Output> {
    return this.stream_("retry", {});
  }
}

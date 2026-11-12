// Workflows (design 0015): a graph of JavaScript functions and agents, run by
// the Rust engine (scheduling, joins, reducers, validation, pauses).

import { Agent, cancellable } from "./agent";
import { translate } from "./errors";
import { native, type NativeWorkflow } from "./native";
import { Storage } from "./storage";

/** The end of a workflow, as an edge or route target. */
export const END = "__end__";

type State = Record<string, unknown>;

/** What a node can know about its execution: the second argument of every node function. */
export interface NodeContext {
  /** The run's id. A paused run keeps its id when resumed. */
  runId: string;
  /** The step this node runs in, counted from 0 at the start of the run. Nodes that run in parallel share a step. */
  step: number;
  /** This node's name. */
  node: string;
  /** The attempt number, from 1. */
  attempt: number;
  /** The value given to `resume` for this node, else `undefined`. */
  resume?: unknown;
}

const SUSPEND = Symbol("suspend");
const GOTO = Symbol("goto");

/** A node's request to pause the run for a person (see `suspend`). */
export interface Suspend {
  [SUSPEND]: true;
  /** What the node asks for; it appears in the result's `interrupts`. */
  payload: unknown;
  /** State changes saved while the run waits. */
  update?: State;
}

/** A node's choice of the next nodes (see `goto`). */
export interface Goto {
  [GOTO]: true;
  /** The nodes (or `END`) to run next, from those declared with `Workflow.goto`. */
  targets: string[];
  /** State changes, as a node's returned object. */
  update?: State;
}

/**
 * Return this from a node to pause the run for a person: `payload` says
 * what is asked; `resume({node: value})` continues it, and the node runs
 * again with `ctx.resume === value`.
 *
 * @param payload What the node asks for (any JSON value): shown in `result.interrupts` and `thread.interrupts()`.
 * @param update State changes to keep while the run waits, e.g. `{ status: "waiting" }`.
 *
 * @example
 * import { END, Workflow, suspend } from "criogenai";
 *
 * const app = new Workflow()
 *   .node("approve", (state, ctx) =>
 *     ctx.resume === undefined ? suspend({ refund: state.amount }) : { approved: ctx.resume === "yes" })
 *   .start("approve")
 *   .edge("approve", END)
 *   .compile();
 * const paused = await app.run({ amount: 40 });
 * console.log(paused.interrupts); // [{ node: "approve", payload: { refund: 40 } }]
 * const done = await app.resume(paused.snapshot, { approve: "yes" });
 * console.log(done.state.approved); // true
 */
export function suspend(payload: unknown, update?: State): Suspend {
  return { [SUSPEND]: true, payload, update };
}

/**
 * Return this from a node to choose the next nodes (declared with `Workflow.goto`).
 *
 * @param targets One node name (or `END`), or several to run in parallel. Each must be among the targets declared for this node with `Workflow.goto`.
 * @param update State changes, as a node's returned object.
 *
 * @example
 * import { END, Workflow, goto } from "criogenai";
 *
 * const app = new Workflow()
 *   .node("check", (state) => goto((state.n as number) > 10 ? "big" : "small", { checked: true }))
 *   .node("big", () => ({ size: "big" }))
 *   .node("small", () => ({ size: "small" }))
 *   .start("check")
 *   .goto("check", ["big", "small"])
 *   .edge("big", END)
 *   .edge("small", END)
 *   .compile();
 * console.log((await app.run({ n: 50 })).state.size); // "big"
 */
export function goto(targets: string | string[], update?: State): Goto {
  return { [GOTO]: true, targets: typeof targets === "string" ? [targets] : targets, update };
}

/**
 * A node: a function of the state (and its context). It returns the keys it
 * changes (an object), nothing, `suspend(...)` or `goto(...)`, or a Promise of
 * one of these.
 */
export type NodeFunction = (state: State, ctx: NodeContext) => State | void | undefined | Suspend | Goto | Promise<State | void | undefined | Suspend | Goto>;

/** A router: chooses a label from the state. The label picks the next node in the table given to `Workflow.route`. */
export type Router = (state: State) => string | Promise<string>;

/** A paused node and what it asked for. */
export interface Interrupt {
  /** The paused node's name: the key to answer in `resume`. */
  node: string;
  /** What the node asked for (the `payload` of its `suspend`). */
  payload: unknown;
}

/** What a run produced. */
export interface WorkflowResult {
  /** The state when the run finished or paused. */
  state: State;
  /** How many steps the run took. */
  steps: number;
  /** `true` when nodes wait for a person: answer them with `resume`. */
  suspended: boolean;
  /** What the paused nodes asked for; empty unless `suspended`. */
  interrupts: Interrupt[];
  /** The run's id. */
  runId: string;
  /** Pass to `CompiledWorkflow.resume` (runs without storage). */
  snapshot: unknown;
  /** Threads only: the checkpoint written, and the thread's version. */
  checkpointId?: string;
  /** Threads only: the thread's version after this run (it grows with every save). */
  version?: number;
}

/** Limits for one `run` or `resume` call. */
export interface WorkflowRunOptions {
  /**
   * The most steps a run may take, counted from its start across resumes. A
   * run that needs more fails with `step_limit_exceeded` (`LimitExceededError`):
   * a guard against endless loops.
   * @default 64
   */
  maxSteps?: number;
  /** A time limit for this call, in milliseconds. Past it the run fails with `deadline_exceeded` (`LimitExceededError`). */
  deadlineMs?: number;
  /** Aborting it cancels the run, which fails with `CancelledError`. */
  signal?: AbortSignal;
}

function nodeAdapter(fn: NodeFunction, causes: Map<string, unknown>): (request: string) => Promise<string> {
  return async (request: string) => {
    const { node, state, ctx } = JSON.parse(request) as { node: string; state: State; ctx: NodeContext };
    try {
      const value = await fn(state, { ...ctx, resume: ctx.resume ?? undefined });
      if (value && typeof value === "object" && SUSPEND in value) {
        return JSON.stringify({ suspend: value.payload ?? null, update: value.update ?? null });
      }
      if (value && typeof value === "object" && GOTO in value) {
        return JSON.stringify({ goto: value.targets, update: value.update ?? null });
      }
      if (value !== undefined && value !== null && (typeof value !== "object" || Array.isArray(value))) {
        throw new TypeError(`a node returns an object (the update), suspend(...) or goto(...), not ${JSON.stringify(value)}`);
      }
      return JSON.stringify({ update: value ?? null });
    } catch (error) {
      const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
      // A key unique to this throw: the run error repeats it, so concurrent
      // runs that fail the same way never take each other's exception.
      const causeKey = `node:${node}:${++throws}`;
      causes.set(causeKey, error);
      return JSON.stringify({ crash: text, causeKey });
    }
  };
}

/** Counts throws, for keys unique to each. */
let throws = 0;

function routerAdapter(router: Router, causes: Map<string, unknown>): (request: string) => Promise<string> {
  return async (request: string) => {
    const { node, state } = JSON.parse(request) as { node: string; state: State };
    try {
      const label = await router(state);
      if (typeof label !== "string") throw new TypeError(`a router returns a label (a string), not ${JSON.stringify(label)}`);
      return JSON.stringify({ label });
    } catch (error) {
      const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
      // A key unique to this throw: the run error repeats it, so concurrent
      // runs that fail the same way never take each other's exception.
      const causeKey = `node:${node}:${++throws}`;
      causes.set(causeKey, error);
      return JSON.stringify({ crash: text, causeKey });
    }
  };
}

/**
 * A graph of nodes (functions or agents), compiled by `compile` and run by
 * the Rust engine. `reducers`: how writes to a key combine when several
 * nodes write it ("replace", the default; "append"; "upsertById" for
 * message lists).
 *
 * Each node reads the state and returns the keys it changes. Nodes that are
 * ready at the same time run concurrently, in one step. Build the graph with
 * `node`, `start`, `edge`, `route`, `join` and `goto`, then `compile` it.
 *
 * @example
 * import { END, Workflow } from "criogenai";
 *
 * const app = new Workflow()
 *   .node("clean", (state) => ({ text: String(state.text).trim().toLowerCase() }))
 *   .node("count", (state) => ({ words: String(state.text).split(" ").length }))
 *   .start("clean")
 *   .edge("clean", "count")
 *   .edge("count", END)
 *   .compile();
 * const result = await app.run({ text: "  Hello Big World " });
 * console.log(result.state); // { text: "hello big world", words: 3 }
 */
export class Workflow {
  #nodes = new Map<string, NodeFunction | Agent<any>>();
  #entry?: string;
  #edges: [string, string][] = [];
  #routes: { from: string; router: Router; routes: Record<string, string> }[] = [];
  #joins: [string[], string][] = [];
  #gotos: [string, string[]][] = [];
  #reducers: Record<string, string>;

  /**
   * @param options `reducers`: one per state key that several nodes write in
   * the same step. `"replace"` (the default for every key): a write replaces
   * the value, and two writes in one step fail with `update_conflict`.
   * `"append"`: the value is a list and each write is a list appended to it.
   * `"upsertById"`: a list of objects with a string `id`; a write with a new
   * id appends, an existing id replaces, `{ id, $remove: true }` removes.
   */
  constructor(options: { reducers?: Record<string, "replace" | "append" | "upsertById"> } = {}) {
    this.#reducers = { ...(options.reducers ?? {}) };
  }

  /**
   * Adds a function `(state, ctx)` as the node `name`.
   *
   * @param name The node's name, unique in the workflow.
   * @param fn The function. It returns the state keys it changes, nothing, `suspend(...)` or `goto(...)`. What it throws fails the run with `node_failed`, and the error's `cause` is the thrown value.
   * @throws {Error} A node with this name already exists.
   */
  node(name: string, fn: NodeFunction): this {
    return this.addNode(name, fn);
  }

  /**
   * Adds a function or an `Agent` (it reads and writes the `messages` key).
   *
   * Give `messages` the `"upsertById"` reducer when an agent is a node, so
   * messages merge by id.
   *
   * @param name The node's name, unique in the workflow.
   * @param node A node function or an `Agent`.
   * @throws {Error} A node with this name already exists.
   * @throws {TypeError} `node` is neither a function nor an `Agent`.
   */
  addNode(name: string, node: NodeFunction | Agent<any>): this {
    if (this.#nodes.has(name)) throw new Error(`node "${name}" already exists`);
    if (!(node instanceof Agent) && typeof node !== "function") throw new TypeError(`node "${name}": not a function or an Agent`);
    this.#nodes.set(name, node);
    return this;
  }

  /**
   * The node a run starts with.
   *
   * @param name A node added with `node` or `addNode`.
   */
  start(name: string): this {
    this.#entry = name;
    return this;
  }

  /**
   * After `source`, run `target` (or END).
   *
   * Several edges from one node run their targets in parallel, in the same step.
   *
   * @param source The node that runs first.
   * @param target The node to run next, or `END`.
   */
  edge(source: string, target: string): this {
    this.#edges.push([source, target]);
    return this;
  }

  /**
   * After `source`, `router(state)` returns a label; run `routes[label]`.
   *
   * A label missing from `routes` fails the run with `unknown_route`; a router
   * that throws fails it with `router_failed`.
   *
   * @param source The node after which the router runs.
   * @param router A function of the state (sync or async) that returns a label.
   * @param routes Labels and the node (or `END`) each one leads to, e.g. `{ ok: "answer", empty: END }`.
   */
  route(source: string, router: Router, routes: Record<string, string>): this {
    this.#routes.push({ from: source, router, routes: { ...routes } });
    return this;
  }

  /**
   * Run `target` once every node in `sources` has finished.
   *
   * The sources may finish in different steps; `target` runs once all of them
   * have finished since it last ran.
   *
   * @param sources The nodes to wait for.
   * @param target The node to run after them.
   */
  join(sources: string[], target: string): this {
    this.#joins.push([[...sources], target]);
    return this;
  }

  /**
   * `source` may choose among `targets` by returning `goto(...)`.
   *
   * The chosen nodes run in addition to `source`'s edges. A node with goto
   * targets and no edges or joins must choose at least one target every time
   * it runs (else `missing_goto`).
   *
   * @param source The node that chooses.
   * @param targets Every node (or `END`) it may choose.
   */
  goto(source: string, targets: string[]): this {
    this.#gotos.push([source, [...targets]]);
    return this;
  }

  /**
   * Checks the graph in Rust: every problem at once (ValidationError with `details.issues`).
   *
   * @param options `storage`: a `Storage` or a URL where threads keep their
   * runs (`app.thread(id)`). Without it, an in-memory storage is used.
   * @returns The workflow, ready to run.
   * @throws {ValidationError} The graph is invalid (`invalid_workflow`): `details.issues` lists every problem, such as a missing start node, an edge to an unknown node or a node no path reaches.
   */
  compile(options: { storage?: Storage | string } = {}): CompiledWorkflow {
    const storage = options.storage instanceof Storage ? options.storage : new Storage({ url: options.storage });
    const causes = new Map<string, unknown>();
    const functions = [...this.#nodes].filter((entry): entry is [string, NodeFunction] => !(entry[1] instanceof Agent));
    const agents = Object.fromEntries([...this.#nodes].filter(([, n]) => n instanceof Agent).map(([name, n]) => [name, (n as Agent<any>).native.handle]));
    const config = {
      entry: this.#entry,
      nodes: functions.map(([name]) => name),
      agents,
      edges: this.#edges,
      routes: this.#routes.map((r) => ({ from: r.from, routes: r.routes })),
      joins: this.#joins,
      gotos: this.#gotos,
      reducers: this.#reducers,
    };
    try {
      const nativeWorkflow = new native.NativeWorkflow(
        JSON.stringify(config),
        functions.map(([, fn]) => nodeAdapter(fn, causes)),
        this.#routes.map((r) => routerAdapter(r.router, causes)),
        storage.native,
      );
      return new CompiledWorkflow(nativeWorkflow, storage, causes);
    } catch (error) {
      throw translate(error);
    }
  }
}

function result(raw: string): WorkflowResult {
  return JSON.parse(raw) as WorkflowResult;
}

/**
 * A checked workflow, ready to run. Made by `Workflow.compile`.
 *
 * `run` and `resume` keep nothing between calls: a paused run comes back as a
 * `snapshot` to pass to `resume`. For runs saved in storage, use `thread(id)`.
 */
export class CompiledWorkflow {
  /**
   * @internal
   * @param native The engine's workflow.
   * @param storage Where threads are kept.
   * @param causes What failing nodes threw.
   */
  constructor(
    readonly native: NativeWorkflow,
    /** Where this workflow's threads are kept. */
    readonly storage: Storage,
    /** @internal What failing nodes threw, by `node:<name>:<message>`. */
    readonly causes: Map<string, unknown>,
  ) {}

  /**
   * Runs from the start node with `input` as the state.
   *
   * @param input The initial state (an object).
   * @param options A step limit, a deadline and an `AbortSignal`.
   * @returns The final state, or a paused run (`suspended: true`, with `interrupts` and a `snapshot`).
   * @throws {CriogenError} A node threw (`node_failed`; `cause` is what it threw, `details.node` the node), a router failed (`router_failed`, `unknown_route`), or two nodes wrote one key in one step (`update_conflict`).
   * @throws {LimitExceededError} `maxSteps` or `deadlineMs` was exceeded.
   * @throws {CancelledError} The signal was aborted.
   */
  async run(input: State = {}, options: WorkflowRunOptions = {}): Promise<WorkflowResult> {
    const payload = JSON.stringify({ input, maxSteps: options.maxSteps, deadlineMs: options.deadlineMs });
    try {
      return result(await cancellable(options.signal, (id) => this.native.run(payload, id)));
    } catch (error) {
      throw translate(error, this.causes);
    }
  }

  /**
   * Continues a paused run (without storage) from its snapshot: one value per paused node.
   *
   * The snapshot is plain JSON: it can be stored anywhere and resumed later,
   * in this process or another one, by the same workflow.
   *
   * @param snapshot The `snapshot` of the paused result.
   * @param values One answer per paused node, by node name; each node runs again with `ctx.resume` set to its value.
   * @param options A step limit, a deadline and an `AbortSignal`.
   * @throws {ValidationError} The values do not match the paused nodes (`resume_invalid`), or the snapshot comes from a different workflow (`workflow_mismatch`).
   */
  async resume(snapshot: unknown, values: Record<string, unknown>, options: WorkflowRunOptions = {}): Promise<WorkflowResult> {
    const payload = JSON.stringify({ snapshot, values, maxSteps: options.maxSteps, deadlineMs: options.deadlineMs });
    try {
      return result(await cancellable(options.signal, (id) => this.native.resume(payload, id)));
    } catch (error) {
      throw translate(error, this.causes);
    }
  }

  /**
   * A durable run history in the workflow's storage (created on first use).
   *
   * @param threadId The thread's id, e.g. an order or a ticket number.
   * @param options `actor`: who acts through this handle, recorded in the audit trail (e.g. `"user:u-1"`).
   *
   * @example
   * import { END, Storage, Workflow, suspend } from "criogenai";
   *
   * const app = new Workflow()
   *   .node("approve", (state, ctx) =>
   *     ctx.resume === undefined ? suspend({ refund: state.amount }) : { approved: ctx.resume === "yes" })
   *   .start("approve")
   *   .edge("approve", END)
   *   .compile({ storage: new Storage() }); // e.g. "sqlite:///data/app.db"
   * await app.thread("refund-1").run({ amount: 25 }); // pauses; saved in storage
   * // Later, from any handle on the same storage:
   * const turn = await app.thread("refund-1", { actor: "user:u-2" }).resume({ approve: "yes" });
   * console.log(turn.state.approved); // true
   */
  thread(threadId: string, options: { actor?: string } = {}): WorkflowThread {
    return new WorkflowThread(this, threadId, options.actor);
  }
}

/**
 * One durable conversation with a workflow: each `run` continues from the
 * stored state; a paused run waits in storage for `resume`.
 *
 * Progress is saved after every step. Get one with `compiled.thread(id)`.
 */
export class WorkflowThread {
  /**
   * @param app The compiled workflow.
   * @param id The thread's id.
   * @param actor Who acts through this handle, for the audit trail.
   */
  constructor(
    /** The compiled workflow this thread runs. */
    readonly app: CompiledWorkflow,
    /** The thread's id. */
    readonly id: string,
    /** Who acts through this handle, recorded in the audit trail. */
    readonly actor?: string,
  ) {}

  private async op<T>(op: string, payload: Record<string, unknown> = {}, signal?: AbortSignal): Promise<T> {
    const body = JSON.stringify({ ...payload, threadId: this.id, actor: this.actor });
    try {
      return JSON.parse(await cancellable(signal, (id) => this.app.native.thread(op, body, id))) as T;
    } catch (error) {
      throw translate(error, this.app.causes);
    }
  }

  /**
   * Runs from the start node on the stored state plus `input`.
   *
   * A key in `input` replaces the stored one.
   *
   * @param input State keys to set before the run.
   * @param options A step limit, a deadline and an `AbortSignal`.
   * @returns The run, with the thread's `version` and the `checkpointId` written.
   * @throws {ThreadSuspendedError} Nodes are waiting: answer them with `resume` first.
   * @throws {CriogenError} The last run did not finish (`thread_interrupted`): continue it with `retry`.
   * @throws {ThreadBusyError} Another call runs this thread (see `onBusy` in `StorageOptions`).
   */
  run(input: State = {}, options: WorkflowRunOptions = {}): Promise<WorkflowResult> {
    return this.op("run", { input, maxSteps: options.maxSteps, deadlineMs: options.deadlineMs }, options.signal);
  }

  /**
   * Answers the paused nodes: one value per node name.
   *
   * Each node runs again with `ctx.resume` set to its value. Values that do
   * not match the waiting nodes are refused before anything runs, and the
   * thread keeps waiting.
   *
   * @param values One answer per paused node, by node name.
   * @param options `signal`: aborting it cancels the run.
   * @throws {ValidationError} The values do not match the waiting nodes (`resume_invalid`).
   */
  resume(values: Record<string, unknown>, options: { signal?: AbortSignal } = {}): Promise<WorkflowResult> {
    return this.op("resume", { values }, options.signal);
  }

  /**
   * Continues a failed or interrupted run; finished nodes do not run again.
   *
   * @param options `signal`: aborting it cancels the run.
   */
  retry(options: { signal?: AbortSignal } = {}): Promise<WorkflowResult> {
    return this.op("retry", {}, options.signal);
  }

  /** The stored state (empty for a new thread). */
  state(): Promise<State> {
    return this.op("state");
  }

  /** What the paused nodes asked for (empty unless the thread waits). */
  interrupts(): Promise<Interrupt[]> {
    return this.op("interrupts");
  }

  /**
   * The newest checkpoints first.
   *
   * Each has `id`, `parent_id`, `version`, `source`, `status`, `run_id`,
   * `step` and `created_at_ms`.
   *
   * @param limit The most checkpoints to return.
   */
  history(limit = 20): Promise<Record<string, unknown>[]> {
    return this.op("history", { limit });
  }

  /**
   * Moves the thread back to an earlier checkpoint.
   *
   * The checkpoint's state becomes current again as a new checkpoint; the
   * later ones stay as an abandoned branch (see `dropBranches` in
   * `RetentionOptions`).
   *
   * @param checkpointId An `id` from `history()`.
   * @returns The new checkpoint's metadata, as `history()` lists it.
   * @throws {StorageError} The thread has no such checkpoint (`checkpoint_not_found`).
   */
  rewind(checkpointId: string): Promise<Record<string, unknown>> {
    return this.op("rewind", { checkpointId });
  }

  /** Deletes the thread; `true` if it existed. */
  delete(): Promise<boolean> {
    return this.op("delete");
  }
}

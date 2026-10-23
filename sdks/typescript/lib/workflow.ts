// Workflows (design 0015): a graph of JavaScript functions and agents, run by
// the Rust engine (scheduling, joins, reducers, validation, pauses).

import { Agent, cancellable } from "./agent";
import { translate } from "./errors";
import { native, type NativeWorkflow } from "./native";
import { Storage } from "./storage";

/** The end of a workflow, as an edge or route target. */
export const END = "__end__";

type State = Record<string, unknown>;

/** What a node can know about its execution. */
export interface NodeContext {
  runId: string;
  step: number;
  node: string;
  attempt: number;
  /** The value given to `resume` for this node, else `undefined`. */
  resume?: unknown;
}

const SUSPEND = Symbol("suspend");
const GOTO = Symbol("goto");

/** A node's request to pause the run for a person (see `suspend`). */
export interface Suspend {
  [SUSPEND]: true;
  payload: unknown;
  update?: State;
}

/** A node's choice of the next nodes (see `goto`). */
export interface Goto {
  [GOTO]: true;
  targets: string[];
  update?: State;
}

/**
 * Return this from a node to pause the run for a person: `payload` says
 * what is asked; `resume({node: value})` continues it, and the node runs
 * again with `ctx.resume === value`.
 */
export function suspend(payload: unknown, update?: State): Suspend {
  return { [SUSPEND]: true, payload, update };
}

/** Return this from a node to choose the next nodes (declared with `Workflow.goto`). */
export function goto(targets: string | string[], update?: State): Goto {
  return { [GOTO]: true, targets: typeof targets === "string" ? [targets] : targets, update };
}

/** A node: a function of the state (and its context). */
export type NodeFunction = (state: State, ctx: NodeContext) => State | void | undefined | Suspend | Goto | Promise<State | void | undefined | Suspend | Goto>;

/** A router: chooses a label from the state. */
export type Router = (state: State) => string | Promise<string>;

/** A paused node and what it asked for. */
export interface Interrupt {
  node: string;
  payload: unknown;
}

/** What a run produced. */
export interface WorkflowResult {
  state: State;
  steps: number;
  suspended: boolean;
  interrupts: Interrupt[];
  runId: string;
  /** Pass to `CompiledWorkflow.resume` (runs without storage). */
  snapshot: unknown;
  /** Threads only: the checkpoint written, and the thread's version. */
  checkpointId?: string;
  version?: number;
}

export interface WorkflowRunOptions {
  maxSteps?: number;
  deadlineMs?: number;
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
 */
export class Workflow {
  #nodes = new Map<string, NodeFunction | Agent<any>>();
  #entry?: string;
  #edges: [string, string][] = [];
  #routes: { from: string; router: Router; routes: Record<string, string> }[] = [];
  #joins: [string[], string][] = [];
  #gotos: [string, string[]][] = [];
  #reducers: Record<string, string>;

  constructor(options: { reducers?: Record<string, "replace" | "append" | "upsertById"> } = {}) {
    this.#reducers = { ...(options.reducers ?? {}) };
  }

  /** Adds a function `(state, ctx)` as the node `name`. */
  node(name: string, fn: NodeFunction): this {
    return this.addNode(name, fn);
  }

  /** Adds a function or an `Agent` (it reads and writes the `messages` key). */
  addNode(name: string, node: NodeFunction | Agent<any>): this {
    if (this.#nodes.has(name)) throw new Error(`node "${name}" already exists`);
    if (!(node instanceof Agent) && typeof node !== "function") throw new TypeError(`node "${name}": not a function or an Agent`);
    this.#nodes.set(name, node);
    return this;
  }

  /** The node a run starts with. */
  start(name: string): this {
    this.#entry = name;
    return this;
  }

  /** After `source`, run `target` (or END). */
  edge(source: string, target: string): this {
    this.#edges.push([source, target]);
    return this;
  }

  /** After `source`, `router(state)` returns a label; run `routes[label]`. */
  route(source: string, router: Router, routes: Record<string, string>): this {
    this.#routes.push({ from: source, router, routes: { ...routes } });
    return this;
  }

  /** Run `target` once every node in `sources` has finished. */
  join(sources: string[], target: string): this {
    this.#joins.push([[...sources], target]);
    return this;
  }

  /** `source` may choose among `targets` by returning `goto(...)`. */
  goto(source: string, targets: string[]): this {
    this.#gotos.push([source, [...targets]]);
    return this;
  }

  /** Checks the graph in Rust: every problem at once (ValidationError with `details.issues`). */
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

/** A checked workflow, ready to run. */
export class CompiledWorkflow {
  /** @internal */
  constructor(
    readonly native: NativeWorkflow,
    readonly storage: Storage,
    /** @internal What failing nodes threw, by `node:<name>:<message>`. */
    readonly causes: Map<string, unknown>,
  ) {}

  /** Runs from the start node with `input` as the state. */
  async run(input: State = {}, options: WorkflowRunOptions = {}): Promise<WorkflowResult> {
    const payload = JSON.stringify({ input, maxSteps: options.maxSteps, deadlineMs: options.deadlineMs });
    try {
      return result(await cancellable(options.signal, (id) => this.native.run(payload, id)));
    } catch (error) {
      throw translate(error, this.causes);
    }
  }

  /** Continues a paused run (without storage) from its snapshot: one value per paused node. */
  async resume(snapshot: unknown, values: Record<string, unknown>, options: WorkflowRunOptions = {}): Promise<WorkflowResult> {
    const payload = JSON.stringify({ snapshot, values, maxSteps: options.maxSteps, deadlineMs: options.deadlineMs });
    try {
      return result(await cancellable(options.signal, (id) => this.native.resume(payload, id)));
    } catch (error) {
      throw translate(error, this.causes);
    }
  }

  /** A durable run history in the workflow's storage (created on first use). */
  thread(threadId: string, options: { actor?: string } = {}): WorkflowThread {
    return new WorkflowThread(this, threadId, options.actor);
  }
}

/**
 * One durable conversation with a workflow: each `run` continues from the
 * stored state; a paused run waits in storage for `resume`.
 */
export class WorkflowThread {
  constructor(
    readonly app: CompiledWorkflow,
    readonly id: string,
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

  /** Runs from the start node on the stored state plus `input`. */
  run(input: State = {}, options: WorkflowRunOptions = {}): Promise<WorkflowResult> {
    return this.op("run", { input, maxSteps: options.maxSteps, deadlineMs: options.deadlineMs }, options.signal);
  }

  /** Answers the paused nodes: one value per node name. */
  resume(values: Record<string, unknown>, options: { signal?: AbortSignal } = {}): Promise<WorkflowResult> {
    return this.op("resume", { values }, options.signal);
  }

  /** Continues a failed or interrupted run; finished nodes do not run again. */
  retry(options: { signal?: AbortSignal } = {}): Promise<WorkflowResult> {
    return this.op("retry", {}, options.signal);
  }

  /** The stored state. */
  state(): Promise<State> {
    return this.op("state");
  }

  /** What the paused nodes asked for. */
  interrupts(): Promise<Interrupt[]> {
    return this.op("interrupts");
  }

  /** The newest checkpoints first. */
  history(limit = 20): Promise<Record<string, unknown>[]> {
    return this.op("history", { limit });
  }

  /** Moves the thread back to an earlier checkpoint. */
  rewind(checkpointId: string): Promise<Record<string, unknown>> {
    return this.op("rewind", { checkpointId });
  }

  /** Deletes the thread; `true` if it existed. */
  delete(): Promise<boolean> {
    return this.op("delete");
  }
}

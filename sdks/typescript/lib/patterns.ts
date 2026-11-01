// Design patterns for agentic systems, ready to use (design 0019).
//
//   import { patterns } from "criogen";
//   const flow = patterns.quick("researcher > writer", { model: "openai:gpt-5-mini" });
//   console.log(String(await flow.run("Write about solar panels")));
//
// Steps can be an Agent, a Specialist or its registered name (a string), a
// plain function `text => text` (sync or async), or `[name, step]`. The model
// for specialists comes from `model` or the CRIOGEN_MODEL environment
// variable. Every loop has a cap, so nothing runs forever.
//
// Workflow patterns return a Pattern: `run` for the answer, `compile(storage)`
// for durable threads, approvals and an audit trail, `workflow` to extend the
// graph. `react`, `coordinator` and `hierarchical` return an Agent, so they
// nest as steps of other patterns.

import { Agent, type AgentOptions, type AgentTool } from "./agent";
import type { Model } from "./models";
import type { Storage } from "./storage";
import type { Tool } from "./tools";
import { CompiledWorkflow, END, Workflow, goto, suspend, type NodeContext, type WorkflowResult } from "./workflow";
import * as specialists from "./specialists";

/** No loop runs more often than this, whatever the caller asks for. */
export const CEILING = 50;
export const DEFAULT_MODEL_ENV = "CRIOGEN_MODEL";

type State = Record<string, unknown>;
type Run = (text: string) => Promise<string>;
export type Step = Agent<any> | specialists.Specialist | string | ((text: string) => unknown) | [string, Step];

interface Entry {
  step: string;
  output: string;
}

/** What a pattern produced. `String(result)` is the output; `result.explain()` shows each step. */
export class PatternResult {
  constructor(
    readonly output: string | null,
    readonly trace: Entry[],
    readonly state: State,
    /** Why it ended: done, condition, approved, max_iterations, max_rounds, final, max_handoffs, unknown_handoff, rejected or suspended. */
    readonly stopReason: string,
    readonly suspended: boolean,
    /** What a person is asked, when suspended. */
    readonly waitingFor: unknown,
    /** @internal */
    readonly raw: WorkflowResult,
  ) {}

  toString(): string {
    return this.output ?? "";
  }

  /** Each step on its own line: which step, what it returned (shortened). */
  explain(): string {
    const lines = [`ended: ${this.stopReason}`];
    this.trace.forEach((entry, i) => {
      const text = String(entry.output ?? "").split(/\s+/).filter(Boolean).join(" ");
      lines.push(`${i + 1}. ${entry.step}: ${text.slice(0, 140)}${text.length > 140 ? "…" : ""}`);
    });
    return lines.join("\n");
  }
}

function defaultModel(model: Model | undefined): Model {
  if (model !== undefined) return model;
  const named = process.env[DEFAULT_MODEL_ENV];
  if (named) return named;
  throw new Error(
    `this pattern needs a model for its specialists: pass { model: "openai:gpt-5-mini" } (or any model), or set the ${DEFAULT_MODEL_ENV} environment variable`,
  );
}

function cap(value: number, name: string): number {
  if (!Number.isInteger(value) || value < 1) throw new RangeError(`${name} must be a whole number of at least 1`);
  return Math.min(value, CEILING);
}

function resolve(step: Step, model: Model | undefined, index: number): [string, Run] {
  let name: string | undefined;
  let current: unknown = step;
  if (Array.isArray(current) && current.length === 2 && typeof current[0] === "string") {
    name = current[0];
    current = current[1];
  }
  if (typeof current === "string") current = specialists.get(current);
  if (current instanceof specialists.Specialist) {
    name ??= current.name;
    current = current.build(defaultModel(model));
  }
  if (current instanceof Agent) {
    const agent = current;
    return [
      name ?? `step${index + 1}`,
      async (text) => {
        const result = await agent.run(text);
        return result.text ?? String(result.output);
      },
    ];
  }
  if (typeof current === "function") {
    const fn = current as (text: string) => unknown;
    return [name ?? (fn.name || `step${index + 1}`), async (text) => String((await fn(text)) ?? "")];
  }
  throw new TypeError(`step ${index + 1}: ${String(current)} is not an Agent, a Specialist (or its name), or a function text => text`);
}

const RESERVED = new Set(["begin", "finish", "check", "combine", "generate", "review", "dispatch", "route_first"]);

/** Node names: distinct, and never one of the pattern's own (begin, finish ...). */
function unique(names: string[]): string[] {
  const seen = new Map<string, number>();
  return names.map((raw) => {
    const n = RESERVED.has(raw) ? `${raw}-step` : raw;
    const count = (seen.get(n) ?? 0) + 1;
    seen.set(n, count);
    return count === 1 ? n : `${n}-${count}`;
  });
}

const entry = (step: string, output: string): Entry[] => [{ step, output }];

/** A workflow pattern: run it, compile it for durability, or extend it. */
export class Pattern {
  #compiled?: CompiledWorkflow;

  constructor(
    readonly name: string,
    /** The plain Workflow: add nodes and edges to extend it before the first run. */
    readonly workflow: Workflow,
    private readonly budget: number,
  ) {}

  /** The checked, durable form: `compile(storage).thread(id).run({ input })`, with approvals, resume and an audit trail. */
  compile(storage?: Storage | string): CompiledWorkflow {
    return this.workflow.compile({ storage });
  }

  /** Runs once. A `humanReview` pattern returns `suspended: true`; continue with `resume`. */
  async run(text: string, options: { maxSteps?: number; signal?: AbortSignal } = {}): Promise<PatternResult> {
    this.#compiled ??= this.compile();
    const raw = await this.#compiled.run({ input: text }, { maxSteps: options.maxSteps ?? this.budget, signal: options.signal });
    return toResult(raw);
  }

  /** Continues a `humanReview` run: "approve", `{ decision: "edit", text }` or `{ decision: "reject" }`. */
  async resume(paused: PatternResult, decision: unknown): Promise<PatternResult> {
    if (!this.#compiled || !paused.suspended || !paused.raw.interrupts.length) {
      throw new Error("resume needs the result of a run that is waiting for a person");
    }
    return toResult(await this.#compiled.resume(paused.raw.snapshot, { [paused.raw.interrupts[0].node]: decision }));
  }
}

function toResult(raw: WorkflowResult): PatternResult {
  const state = raw.state;
  return new PatternResult(
    (state.output as string | undefined) ?? null,
    (state.trace as Entry[] | undefined) ?? [],
    state,
    raw.suspended ? "suspended" : ((state.stop_reason as string | undefined) ?? "done"),
    raw.suspended,
    raw.interrupts[0]?.payload,
    raw,
  );
}

/** Custom logic: any Workflow, with the same `run` / `compile` as the ready-made patterns. Start state `{ input }`; the answer is `state.output`. */
export function custom(workflow: Workflow, options: { name?: string; maxSteps?: number } = {}): Pattern {
  return new Pattern(options.name ?? "custom", workflow, options.maxSteps ?? 64);
}

const begin = (state: State): State => ({ current: state.input, trace: [], round: 0 });

function finish(reason = "done") {
  return (state: State): State => ({ output: state.current, stop_reason: state.stop_reason ?? reason });
}

function stepNode(name: string, run: Run) {
  return async (state: State): Promise<State> => {
    const out = await run(state.current as string);
    return { current: out, trace: entry(name, out) };
  };
}

// ----------------------------------------------------------- workflow patterns

/** Runs the steps in a fixed order; each one's output is the next one's input. `quick("researcher > writer")` is the one-liner. */
export function sequential(steps: Step[], options: { model?: Model } = {}): Pattern {
  if (!steps.length) throw new Error("sequential needs at least one step");
  const resolved = steps.map((s, i) => resolve(s, options.model, i));
  const names = unique(resolved.map(([n]) => n));
  const wf = new Workflow({ reducers: { trace: "append" } }).addNode("begin", begin);
  let previous = "begin";
  names.forEach((name, i) => {
    wf.addNode(name, stepNode(name, resolved[i][1])).edge(previous, name);
    previous = name;
  });
  wf.addNode("finish", finish()).edge(previous, "finish").edge("finish", END).start("begin");
  return new Pattern("sequential", wf, 3 * steps.length + 8);
}

/**
 * Runs the branches at the same time on the same input, then combines them:
 * with a `synthesizer` step (it receives the input and every branch's answer),
 * or as a titled list in branch order.
 */
export function parallel(branches: Step[] | Record<string, Step>, options: { synthesizer?: Step; model?: Model } = {}): Pattern {
  const items: Step[] = Array.isArray(branches) ? branches : Object.entries(branches).map(([n, s]): Step => [n, s]);
  if (!items.length) throw new Error("parallel needs at least one branch");
  const resolved = items.map((s, i) => resolve(s, options.model, i));
  const names = unique(resolved.map(([n]) => n));
  const synth = options.synthesizer !== undefined ? resolve(options.synthesizer, options.model, 0) : undefined;
  const wf = new Workflow({ reducers: { trace: "append", results: "append" } }).addNode("begin", begin);
  names.forEach((name, i) => {
    wf.addNode(name, async (state: State) => {
      const out = await resolved[i][1](state.current as string);
      return { results: [{ i, name, output: out }], trace: entry(name, out) };
    }).edge("begin", name);
  });
  wf.addNode("combine", async (state: State): Promise<State> => {
    const found = [...(state.results as { i: number; name: string; output: string }[])].sort((a, b) => a.i - b.i);
    const listing = found.map((r) => `## ${r.name}\n${r.output}`).join("\n\n");
    if (!synth) return { current: listing, stop_reason: "done" };
    const out = await synth[1](`Original request:\n${state.input}\n\nResults from ${found.length} specialists:\n\n${listing}`);
    return { current: out, trace: entry(`${synth[0]} (synthesis)`, out) };
  });
  wf.join(names, "combine");
  wf.addNode("finish", finish()).edge("combine", "finish").edge("finish", END).start("begin");
  return new Pattern("parallel", wf, 2 * items.length + 10);
}

/**
 * Repeats the steps (a chain) until `until(text, round)` is true or
 * `maxIterations` is reached (default 5, at most 50). `stopReason` says which.
 */
export function loop(
  steps: Step[],
  until: (text: string, round: number) => boolean,
  options: { maxIterations?: number; model?: Model } = {},
): Pattern {
  if (!steps.length) throw new Error("loop needs at least one step");
  const limit = cap(options.maxIterations ?? 5, "maxIterations");
  const resolved = steps.map((s, i) => resolve(s, options.model, i));
  const names = unique(resolved.map(([n]) => n));
  const wf = new Workflow({ reducers: { trace: "append" } }).addNode("begin", begin);
  let previous = "begin";
  names.forEach((name, i) => {
    wf.addNode(name, stepNode(name, resolved[i][1])).edge(previous, name);
    previous = name;
  });
  wf.addNode("check", (state: State): State => {
    const round = (state.round as number) + 1;
    if (until(state.current as string, round)) return { round, again: false, stop_reason: "condition" };
    if (round >= limit) return { round, again: false, stop_reason: "max_iterations" };
    return { round, again: true };
  });
  wf.edge(previous, "check").route("check", (s) => (s.again ? "again" : "done"), { again: names[0], done: "finish" });
  wf.addNode("finish", finish()).edge("finish", END).start("begin");
  return new Pattern("loop", wf, limit * (steps.length + 2) + 8);
}

/** Improves a result step by step: `refine` gets the current text and returns a better one; stops when `score(text) >= threshold` or after `maxIterations`. */
export function iterativeRefinement(
  refine: Step,
  score: (text: string) => number,
  options: { threshold?: number; maxIterations?: number; model?: Model } = {},
): Pattern {
  const threshold = options.threshold ?? 0.8;
  return loop([refine], (text) => score(text) >= threshold, { maxIterations: options.maxIterations, model: options.model });
}

/**
 * A generator writes, a critic reviews. The critic approves (its reply starts
 * with APPROVED, or `approved(reply)` is true) or returns feedback; the
 * generator revises. Ends on approval or after `maxRounds` (default 3, at most
 * 50) with the last draft. `stopReason` is "approved" or "max_rounds".
 */
export function reviewAndCritique(
  generator: Step,
  critic: Step,
  options: { maxRounds?: number; approved?: (reply: string) => boolean; model?: Model } = {},
): Pattern {
  const limit = cap(options.maxRounds ?? 3, "maxRounds");
  const [genName, gen] = resolve(generator, options.model, 0);
  let [critName, crit] = resolve(critic, options.model, 1);
  if (genName === critName) critName += "-critic";
  const isOk = options.approved ?? ((reply: string) => reply.trim().toUpperCase().startsWith("APPROVED"));
  const wf = new Workflow({ reducers: { trace: "append" } }).addNode("begin", begin);
  wf.addNode("generate", async (state: State): Promise<State> => {
    const feedback = state.feedback as string | null | undefined;
    const prompt = feedback
      ? `${state.input}\n\nYour previous draft:\n${state.current}\n\nReviewer feedback to address:\n${feedback}\n\nWrite the improved version.`
      : (state.input as string);
    const out = await gen(prompt);
    return { current: out, trace: entry(`${genName} (draft ${(state.round as number) + 1})`, out) };
  });
  wf.addNode("review", async (state: State): Promise<State> => {
    const reply = await crit(`Request:\n${state.input}\n\nDraft:\n${state.current}`);
    const round = (state.round as number) + 1;
    const update = { round, trace: entry(critName, reply) };
    if (isOk(reply)) return { ...update, again: false, stop_reason: "approved", feedback: null };
    if (round >= limit) return { ...update, again: false, stop_reason: "max_rounds", feedback: reply };
    return { ...update, again: true, feedback: reply };
  });
  wf.edge("begin", "generate").edge("generate", "review");
  wf.route("review", (s) => (s.again ? "again" : "done"), { again: "generate", done: "finish" });
  wf.addNode("finish", finish()).edge("finish", END).start("begin");
  return new Pattern("review_and_critique", wf, limit * 4 + 8);
}

/**
 * Agents hand the work to each other with no central coordinator. Each agent is
 * told who it can hand off to and ends its reply with `HANDOFF: <name>` to pass
 * the work on or `FINAL` when the answer is ready. `start` names the first agent
 * (or is a dispatcher step that replies with a name); default: the first. Ends
 * on FINAL or after `maxHandoffs` (default 6, at most 50); a hand-off to an
 * unknown name ends the run with stopReason "unknown_handoff".
 */
export function swarm(
  agents: Record<string, Step>,
  options: { start?: string | Step; maxHandoffs?: number; model?: Model } = {},
): Pattern {
  const names = Object.keys(agents);
  if (names.length < 2) throw new Error("a swarm needs at least two agents");
  const limit = cap(options.maxHandoffs ?? 6, "maxHandoffs");
  const resolved: Record<string, Run> = Object.fromEntries(names.map((n, i) => [n, resolve([n, agents[n]], options.model, i)[1]]));
  const startIsName = typeof options.start === "string" && names.includes(options.start);
  const dispatcher = options.start !== undefined && !startIsName ? resolve(options.start, options.model, 0)[1] : undefined;
  const first = startIsName ? (options.start as string) : names[0];
  const wf = new Workflow({ reducers: { trace: "append" } });
  wf.addNode("begin", (state: State): State => ({ current: state.input, trace: [], round: 0, last: null }));
  wf.addNode("finish", finish("final"));
  for (const me of names) {
    wf.addNode(me, async (state: State) => {
      const others = names.filter((n) => n !== me);
      const note =
        `You are '${me}' in a team. You can hand the work to: ${others.join(", ")}. Do your part, then end your reply with exactly one line: ` +
        "'HANDOFF: <name>' to pass it on, or 'FINAL' if the answer is ready for the user.";
      const out = await resolved[me](`${note}\n\nRequest:\n${state.input}\n\nWork so far:\n${state.current}`);
      const round = (state.round as number) + 1;
      const update = { current: out, round, trace: entry(me, out) };
      const lines = out.trim().split("\n").map((l) => l.trim()).filter(Boolean);
      const tail = lines.length ? lines[lines.length - 1] : "FINAL";
      if (tail.toUpperCase().startsWith("HANDOFF:")) {
        const target = tail.split(":").slice(1).join(":").trim();
        if (!names.includes(target)) return goto("finish", { ...update, stop_reason: "unknown_handoff" });
        if (round > limit) return goto("finish", { ...update, stop_reason: "max_handoffs" });
        return goto(target, update);
      }
      return goto("finish", { ...update, stop_reason: "final" });
    });
    wf.goto(me, [...names.filter((n) => n !== me), "finish"]);
  }
  if (dispatcher) {
    wf.addNode("dispatch", async (state: State) => {
      const choice = (await dispatcher(`Choose who should start. Options: ${names.join(", ")}.\nRequest: ${state.input}\nReply with one name.`)).trim();
      return goto(names.includes(choice) ? choice : names[0], { trace: entry("dispatcher", choice) });
    });
    wf.goto("dispatch", names).edge("begin", "dispatch");
  } else {
    wf.addNode("route_first", () => goto(first)).goto("route_first", [first]).edge("begin", "route_first");
  }
  wf.edge("finish", END).start("begin");
  return new Pattern("swarm", wf, (limit + 3) * 2 + 8);
}

/**
 * Runs a step, then pauses for a person to look at its draft. Resume with
 * "approve", `{ decision: "edit", text }` or `{ decision: "reject" }`. With
 * `compile(storage)` the pause is durable: another process can resume it days later.
 */
export function humanReview(step: Step, options: { model?: Model } = {}): Pattern {
  const [name, run] = resolve(step, options.model, 0);
  const wf = new Workflow({ reducers: { trace: "append" } }).addNode("begin", begin).addNode(name, stepNode(name, run));
  wf.addNode("review", (state: State, ctx: NodeContext) => {
    const decision = ctx.resume as string | { decision?: string; text?: unknown } | undefined;
    if (decision === undefined || decision === null) {
      return suspend({ draft: state.current, question: "Approve, edit or reject this draft." });
    }
    const kind = typeof decision === "string" ? decision : decision.decision;
    if (kind === "approve") return { stop_reason: "approved", trace: entry("person", "approved") };
    if (kind === "edit" && typeof decision === "object" && typeof decision.text === "string") {
      return { current: decision.text, stop_reason: "approved", trace: entry("person", "edited") };
    }
    if (kind === "reject") return { current: "", stop_reason: "rejected", trace: entry("person", "rejected") };
    throw new Error('decision must be "approve", { decision: "edit", text } or { decision: "reject" }');
  });
  wf.addNode("finish", finish());
  wf.edge("begin", name).edge(name, "review").edge("review", "finish").edge("finish", END).start("begin");
  return new Pattern("human_review", wf, 16);
}

/**
 * A pattern from one line of registered specialist names (plug and play):
 *
 *   quick("researcher > writer")   one after the other
 *   quick("sentiment | keywords")  at the same time
 *   quick("writer <-> critic")     write, review, revise until approved
 *   quick("summarizer")            a single specialist
 *
 * One kind of connector per line (combine patterns with the functions above).
 */
export function quick(spec: string, options: { model?: Model } = {}): Pattern {
  if (typeof spec !== "string" || !spec.trim()) throw new Error("quick needs a line like 'researcher > writer'");
  const pieces = spec.split(/(<->|>|\|)/);
  const parts = pieces.filter((_, i) => i % 2 === 0).map((p) => p.trim());
  const connectors = new Set(pieces.filter((_, i) => i % 2 === 1));
  if (connectors.size > 1) throw new Error(`"${spec}": use one kind of connector per line (>, | or <->)`);
  if (parts.some((p) => !p)) throw new Error(`"${spec}": a name is empty`);
  const kind = connectors.size ? [...connectors][0] : ">";
  if (kind === "<->") {
    if (parts.length !== 2) throw new Error(`"${spec}": <-> connects exactly two names (a writer and a reviewer)`);
    return reviewAndCritique(parts[0], parts[1], options);
  }
  if (kind === "|") return parallel(parts, options);
  return sequential(parts, options);
}

// -------------------------------------------------------------- agent patterns

const REACT =
  "Work in a loop: think about what you need next, call a tool, read the result, and repeat. " +
  "Stop and answer as soon as you can; if a tool result looks wrong, say so instead of building on it.";

type AgentExtras = Omit<AgentOptions, "model" | "tools" | "instructions">;

/** A single agent in a reason-act loop with a hard cap on model calls (default 8, at most 50). */
export function react(
  model: Model,
  tools: (Tool<any> | AgentTool)[],
  options: AgentExtras & { maxSteps?: number; instructions?: string } = {},
): Agent<any> {
  const { maxSteps, instructions, limits, ...rest } = options;
  const text = instructions ? `${instructions.trimEnd()}\n\n${REACT}` : REACT;
  return new Agent({ ...rest, model, instructions: text, tools, limits: { ...limits, modelCalls: cap(maxSteps ?? 8, "maxSteps") } });
}

/**
 * An agent that reads each request and hands it to the right specialist, which
 * it calls as a tool. `specialists`: `{ name: Agent | Specialist | "registered-name" | [Agent, "when to use it"] }`.
 * A model decides the routing (more flexible than a fixed workflow, and more model calls).
 */
export function coordinator(
  model: Model,
  team: Record<string, Agent<any> | specialists.Specialist | string | [Agent<any> | specialists.Specialist | string, string]>,
  options: AgentExtras & { instructions?: string } = {},
): Agent<any> {
  const entries = Object.entries(team);
  if (!entries.length) throw new Error("a coordinator needs at least one specialist");
  const tools: AgentTool[] = entries.map(([name, raw]) => {
    let spec: unknown = raw;
    let description: string | undefined;
    if (Array.isArray(spec) && spec.length === 2 && typeof spec[1] === "string") [spec, description] = spec as [unknown, string];
    if (typeof spec === "string") spec = specialists.get(spec);
    if (spec instanceof specialists.Specialist) {
      description ??= spec.description;
      spec = spec.build(defaultModel(model));
    }
    if (!(spec instanceof Agent)) throw new TypeError(`specialist "${name}": expected an Agent, a Specialist or its name`);
    return spec.asTool(name, description ?? `Handles ${name} requests`);
  });
  const roster = tools.map((t) => `- ${t.name}: ${t.description}`).join("\n");
  const base = options.instructions ?? "You are a coordinator. Decide which specialist should handle the request.";
  const { instructions: _ignored, ...rest } = options;
  return new Agent({
    ...rest,
    model,
    instructions: `${base.trimEnd()}\n\nSpecialists you can call (pass the request as input):\n${roster}\nCall the best one; if none fits, answer yourself briefly.`,
    tools,
  });
}

type TreeNode = Agent<any> | specialists.Specialist | string | [Agent<any>, string] | { _description?: string; [name: string]: unknown };

/**
 * A coordinator of coordinators: the root splits the work, each level hands it
 * further down, workers do it. `tree` maps names to a worker (as for
 * `coordinator`) or to a nested object, which becomes a sub-coordinator
 * (describe it with a `_description` key).
 */
export function hierarchical(model: Model, tree: Record<string, TreeNode>, options: AgentExtras & { instructions?: string } = {}): Agent<any> {
  const build = (level: Record<string, TreeNode>): Record<string, any> => {
    const out: Record<string, any> = {};
    for (const [name, node] of Object.entries(level)) {
      if (name === "_description") continue;
      const nested = node && typeof node === "object" && !(node instanceof Agent) && !(node instanceof specialists.Specialist) && !Array.isArray(node);
      out[name] = nested
        ? [coordinator(model, build(node as Record<string, TreeNode>), { instructions: `You lead the ${name} team.` }), (node as any)._description ?? `The ${name} team`]
        : node;
    }
    return out;
  };
  return coordinator(model, build(tree), options);
}

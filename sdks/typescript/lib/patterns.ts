// Design patterns for agentic systems, ready to use (design 0019).
//
//     import { patterns } from "criogen";
//     const flow = patterns.quick("researcher > writer", { model: "openai:gpt-5-mini" });
//     console.log(String(await flow.run("Write about solar panels")));
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
import { envVar } from "./host-env";
import type { Model } from "./models";
import type { Storage } from "./storage";
import type { Tool } from "./tools";
import { CompiledWorkflow, END, Workflow, goto, suspend, type NodeContext, type WorkflowResult } from "./workflow";
import * as specialists from "./specialists";

/** No loop runs more often than this, whatever the caller asks for. */
export const CEILING = 50;
/** The environment variable that names the model for specialists when a pattern gets no `model`. */
export const DEFAULT_MODEL_ENV = "CRIOGEN_MODEL";

type State = Record<string, unknown>;
type Run = (text: string) => Promise<string>;
/**
 * One step of a pattern: an `Agent`, a `Specialist`, a registered specialist's
 * name (a string), a function `text => text` (sync or async), or
 * `[name, step]` to name the step in the trace.
 */
export type Step = Agent<any> | specialists.Specialist | string | ((text: string) => unknown) | [string, Step];

interface Entry {
  step: string;
  output: string;
}

/** What a pattern produced. `String(result)` is the output; `result.explain()` shows each step. */
export class PatternResult {
  /**
   * @param output The final answer, or `null` while suspended.
   * @param trace Each step and what it returned, in order.
   * @param state The workflow's final state.
   * @param stopReason Why it ended.
   * @param suspended `true` when a person must answer first.
   * @param waitingFor What the person is asked, when suspended.
   * @param raw The workflow result it was made from.
   */
  constructor(
    /** The final answer, or `null` while the run waits for a person. */
    readonly output: string | null,
    /** Each step and what it returned (`{ step, output }`), in order. */
    readonly trace: Entry[],
    /** The workflow's final state. */
    readonly state: State,
    /** Why it ended: done, condition, approved, max_iterations, max_rounds, final, max_handoffs, unknown_handoff, rejected or suspended. */
    readonly stopReason: string,
    /** `true` when a `humanReview` run waits for a person: continue with `pattern.resume`. */
    readonly suspended: boolean,
    /** What a person is asked, when suspended. */
    readonly waitingFor: unknown,
    /** @internal */
    readonly raw: WorkflowResult,
  ) {}

  /** The output as text (`""` when there is none), so `String(result)` is the answer. */
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
  const named = envVar(DEFAULT_MODEL_ENV);
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

/**
 * A workflow pattern: run it, compile it for durability, or extend it.
 *
 * Returned by `sequential`, `parallel`, `loop`, `iterativeRefinement`,
 * `reviewAndCritique`, `swarm`, `humanReview`, `quick` and `custom`.
 */
export class Pattern {
  #compiled?: CompiledWorkflow;

  /**
   * @param name The pattern's name, e.g. `"sequential"`.
   * @param workflow The workflow that implements it.
   * @param budget The step limit for `run` when none is given.
   */
  constructor(
    /** The pattern's name, e.g. `"sequential"` or `"review_and_critique"`. */
    readonly name: string,
    /** The plain Workflow: add nodes and edges to extend it before the first run. */
    readonly workflow: Workflow,
    private readonly budget: number,
  ) {}

  /**
   * The checked, durable form: `compile(storage).thread(id).run({ input })`, with approvals, resume and an audit trail.
   *
   * The state starts as `{ input }`; the answer is in `state.output`.
   *
   * @param storage A `Storage` or a URL for the threads; in memory without one.
   * @throws {ValidationError} The workflow (after your changes) does not validate.
   */
  compile(storage?: Storage | string): CompiledWorkflow {
    return this.workflow.compile({ storage });
  }

  /**
   * Runs once. A `humanReview` pattern returns `suspended: true`; continue with `resume`.
   *
   * @param text The request: the first step's input.
   * @param options `maxSteps`: the run's step limit (each pattern sets one from its own caps). `signal`: aborting it cancels the run.
   * @returns The answer (`String(result)`), the trace and why it ended.
   */
  async run(text: string, options: { maxSteps?: number; signal?: AbortSignal } = {}): Promise<PatternResult> {
    this.#compiled ??= this.compile();
    const raw = await this.#compiled.run({ input: text }, { maxSteps: options.maxSteps ?? this.budget, signal: options.signal });
    return toResult(raw);
  }

  /**
   * Continues a `humanReview` run: "approve", `{ decision: "edit", text }` or `{ decision: "reject" }`.
   *
   * Works on the result of this object's own `run`. For a pause that must
   * outlive the process, use `compile(storage).thread(id)` instead.
   *
   * @param paused The suspended result of `run`.
   * @param decision `"approve"` keeps the draft, `{ decision: "edit", text }` replaces it, `{ decision: "reject" }` ends with an empty output and `stopReason` "rejected".
   * @throws {Error} `paused` is not a run of this pattern that waits for a person.
   */
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

/**
 * Custom logic: any Workflow, with the same `run` / `compile` as the ready-made patterns. Start state `{ input }`; the answer is `state.output`.
 *
 * @param workflow A workflow whose nodes read `state.input` and write `state.output` (and optionally `trace` and `stop_reason`).
 * @param options `name` (default `"custom"`) and `maxSteps`, the step limit for `run` (default 64).
 *
 * @example
 * import { END, Workflow, patterns } from "criogen";
 *
 * const wf = new Workflow()
 *   .node("shout", (state) => ({ output: String(state.input).toUpperCase() }))
 *   .start("shout")
 *   .edge("shout", END);
 * const result = await patterns.custom(wf).run("hello");
 * console.log(String(result)); // "HELLO"
 */
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

/**
 * Runs the steps in a fixed order; each one's output is the next one's input. `quick("researcher > writer")` is the one-liner.
 *
 * @param steps At least one step.
 * @param options `model`: the model for specialist steps (else the `CRIOGEN_MODEL` environment variable).
 * @throws {Error} No steps, or a specialist step and no model.
 * @throws {TypeError} A step is not an Agent, a Specialist (or its name) or a function.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * const flow = patterns.sequential([
 *   ["trim", (text: string) => text.trim()],
 *   ["shout", async (text: string) => text.toUpperCase()],
 * ]);
 * const result = await flow.run("  hello  ");
 * console.log(String(result)); // "HELLO"
 * console.log(result.explain()); // ended: done, then one line per step
 */
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
 *
 * @param branches The steps, as a list or as `{ name: step }` (the names title the combined list).
 * @param options `synthesizer`: a step that writes one answer from the request and the branches' answers. `model`: the model for specialist steps.
 * @throws {Error} No branches.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * const flow = patterns.parallel({
 *   words: (text: string) => `${text.split(" ").length} words`,
 *   letters: (text: string) => `${text.length} characters`,
 * });
 * console.log(String(await flow.run("the quick brown fox")));
 * // ## words
 * // 4 words
 * //
 * // ## letters
 * // 19 characters
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
 *
 * @param steps The chain to repeat; each round starts from the last round's output.
 * @param until Checked after each round with the output and the round number (from 1): `true` stops (`stopReason` "condition").
 * @param options `maxIterations` (a whole number of at least 1; values above 50 count as 50) and `model` for specialist steps.
 * @throws {RangeError} `maxIterations` is not a whole number of at least 1.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * const flow = patterns.loop([(text: string) => text + "!"], (text) => text.endsWith("!!!"), { maxIterations: 10 });
 * const result = await flow.run("go");
 * console.log(String(result), result.stopReason); // "go!!!" "condition"
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

/**
 * Improves a result step by step: `refine` gets the current text and returns a better one; stops when `score(text) >= threshold` or after `maxIterations`.
 *
 * @param refine The step that rewrites the text.
 * @param score Rates a text, typically from 0 to 1.
 * @param options `threshold` (default 0.8), `maxIterations` (default 5, at most 50) and `model` for a specialist step.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * const flow = patterns.iterativeRefinement(
 *   (text: string) => text + " More detail.",
 *   (text) => Math.min(text.length / 40, 1),
 * );
 * console.log((await flow.run("A short note.")).stopReason); // "condition"
 */
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
 *
 * @param generator The step that writes and revises. From the second round it gets the request, its previous draft and the critic's feedback.
 * @param critic The step that reviews; it gets the request and the draft.
 * @param options `maxRounds` (default 3, at most 50), `approved` (decides from the critic's reply; default: it starts with "APPROVED"), `model` for specialist steps.
 * @throws {RangeError} `maxRounds` is not a whole number of at least 1.
 *
 * @example
 * import { Agent, FakeModel, patterns } from "criogen";
 *
 * const writer = new Agent({ model: new FakeModel(["Draft one.", "Draft two, with sources."]) });
 * const critic = new Agent({ model: new FakeModel(["Add sources.", "APPROVED"]) });
 * const result = await patterns.reviewAndCritique(writer, critic).run("Write a short note on hand washing.");
 * console.log(String(result), result.stopReason); // "Draft two, with sources." "approved"
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
 *
 * @param agents At least two named steps, `{ name: step }`.
 * @param options `start`: the first agent's name, or a dispatcher step that replies with a name. `maxHandoffs` (default 6, at most 50). `model` for specialist steps.
 * @throws {Error} Fewer than two agents.
 * @throws {RangeError} `maxHandoffs` is not a whole number of at least 1.
 *
 * @example
 * import { Agent, FakeModel, patterns } from "criogen";
 *
 * const triage = new Agent({ model: new FakeModel(["This is a billing question.\nHANDOFF: billing"]) });
 * const billing = new Agent({ model: new FakeModel(["Your invoice was sent on Monday.\nFINAL"]) });
 * const result = await patterns.swarm({ triage, billing }).run("Where is my invoice?");
 * console.log(result.stopReason, result.trace.map((t) => t.step)); // "final" ["triage", "billing"]
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
 *
 * While paused, `waitingFor` is `{ draft, question }`.
 *
 * @param step The step that writes the draft.
 * @param options `model` for a specialist step.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * const flow = patterns.humanReview((text: string) => `Reply: ${text}`);
 * const paused = await flow.run("Thanks for your order.");
 * console.log(paused.suspended, paused.waitingFor); // true { draft: "Reply: ...", question: "..." }
 * const done = await flow.resume(paused, { decision: "edit", text: "Thank you for your order!" });
 * console.log(String(done), done.stopReason); // "Thank you for your order!" "approved"
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
 *     quick("researcher > writer")   one after the other
 *     quick("sentiment | keywords")  at the same time
 *     quick("writer <-> critic")     write, review, revise until approved
 *     quick("summarizer")            a single specialist
 *
 * One kind of connector per line (combine patterns with the functions above).
 *
 * The specialists need a model: `options.model`, or the `CRIOGEN_MODEL`
 * environment variable.
 *
 * @param spec The line, e.g. `"researcher > writer"`.
 * @param options `model`: a model or a model string such as `"openai:gpt-5-mini"`.
 * @throws {Error} The line is empty, mixes connectors, has an empty name, or uses `<->` with other than two names; a name is not registered; or there is no model.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * // Needs OPENAI_API_KEY.
 * const flow = patterns.quick("researcher > writer", { model: "openai:gpt-5-mini" });
 * const result = await flow.run("Write about solar panels");
 * console.log(String(result));
 * console.log(result.explain());
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

/**
 * A single agent in a reason-act loop with a hard cap on model calls (default 8, at most 50).
 *
 * Returns an `Agent`, so it can be a step of another pattern.
 *
 * @param model The agent's model.
 * @param tools The tools it may call.
 * @param options Any other `Agent` option, plus `maxSteps` (the cap on model calls) and `instructions` (put before the reason-act instructions).
 * @throws {RangeError} `maxSteps` is not a whole number of at least 1.
 *
 * @example
 * import { FakeModel, FakeReply, patterns, tool } from "criogen";
 *
 * const lookup = tool({
 *   name: "order_status",
 *   description: "Status of an order.",
 *   input: { type: "object", properties: { id: { type: "string" } }, required: ["id"] },
 *   run: async ({ id }: { id: string }) => ({ id, status: "shipped" }),
 * });
 * const model = new FakeModel([FakeReply.toolCall("order_status", { id: "A-1" }), "Order A-1 has shipped."]);
 * const agent = patterns.react(model, [lookup], { maxSteps: 4 });
 * console.log((await agent.run("Where is order A-1?")).text);
 */
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
 *
 * Returns an `Agent`; each specialist becomes one of its tools.
 *
 * @param model The coordinator's model (also used to build specialists given as a `Specialist` or a name).
 * @param team The specialists by tool name. A description (from the pair, or the Specialist's own) tells the model when to call each.
 * @param options Any other `Agent` option, plus `instructions` to replace the default coordinator instructions.
 * @throws {Error} The team is empty, or a name is not a registered specialist.
 * @throws {TypeError} A team member is not an Agent, a Specialist or its name.
 *
 * @example
 * import { Agent, FakeModel, FakeReply, patterns } from "criogen";
 *
 * const billing = new Agent({ model: new FakeModel(["Your invoice was sent on Monday."]) });
 * const lead = patterns.coordinator(
 *   new FakeModel([FakeReply.toolCall("billing", { input: "Where is my invoice?" }), "It was sent on Monday."]),
 *   { billing: [billing, "Invoices and payments"] },
 * );
 * console.log((await lead.run("Where is my invoice?")).text);
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
 *
 * Returns the root coordinator, an `Agent`.
 *
 * @param model The model of every coordinator (and of specialists given as a `Specialist` or a name).
 * @param tree Names mapped to workers or to nested objects (sub-teams).
 * @param options Any other `Agent` option for the root, plus its `instructions`.
 *
 * @example
 * import { patterns } from "criogen";
 *
 * // Needs OPENAI_API_KEY; "researcher" and "writer" are registered specialists.
 * const lead = patterns.hierarchical("openai:gpt-5-mini", {
 *   research: { _description: "Finds facts", web: "researcher" },
 *   writing: { _description: "Writes the text", writer: "writer" },
 * });
 * console.log((await lead.run("A one-page brief on heat pumps")).text);
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

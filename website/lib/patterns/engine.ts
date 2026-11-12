// The playground's story context: what a run reports (lines, the diagram's
// active node, counts, a person's decision). Patterns run on the CriogenAI
// engine in the browser (live.ts runs their code sample); `Ctx` is how that
// run is told on the page.
import type { EngineSpec } from "./live";

export type Kind = "user" | "model" | "tool" | "note" | "state" | "final" | "error" | "human";

export interface Line {
  id: number;
  who: string;
  kind: Kind;
  text: string;
  node?: string;
  live?: boolean;
  /** How long the call took, in ms (model and tool lines). */
  ms?: number;
}

/**
 * Where a run's time went, by wall clock (parallel calls overlap, so nothing
 * is counted twice): a model was answering; else a tool was running; else the
 * playground was pausing to tell the story (or waiting for you); else
 * CriogenAI and the page (the engine, the bridge, the transcript).
 */
export interface Timing {
  total: number;
  model: number;
  tool: number;
  wait: number;
  engine: number;
  /** A real provider answered (else the scripted model, whose time is a pause). */
  live: boolean;
  modelCalls: number;
  toolCalls: number;
}

export interface GNode {
  id: string;
  label: string;
  x: number;
  y: number;
  kind?: "io" | "model" | "tool" | "agent" | "logic" | "human" | "memory";
}

export interface Graph {
  w: number;
  h: number;
  nodes: GNode[];
  /** [from, to, label?]; drawn as arrows. */
  edges: [string, string, string?][];
}

export interface Param {
  id: string;
  label: string;
  min: number;
  max: number;
  step?: number;
  value: number;
}

export type Decision = { kind: "approve" } | { kind: "edit"; text: string } | { kind: "reject" };

export interface Ctx {
  /** A real model answers (the visitor's key) instead of the script. */
  live: boolean;
  /** Slider values, for patterns with parameters. */
  params: Record<string, number>;
  /** Highlights a node (and the edge from the last one). */
  at(node: string): void;
  say(who: string, kind: Kind, text: string, node?: string, ms?: number): void;
  pause(ms?: number): Promise<void>;
  /** Waits for the visitor to approve, edit or reject. */
  human(draft: string): Promise<Decision>;
  /** Counts a model or tool call. */
  count(kind: "model" | "tool"): void;
  /** Shows that a model is answering. */
  thinking(on: boolean): void;
  /** Reports where the run's time went. */
  timing(t: Timing): void;
}

export interface PatternDef {
  id: string;
  title: string;
  group: "Reasoning" | "Multi-agent" | "IBM agent types";
  /** One line for the sidebar and the gallery. */
  short: string;
  /** What it is, in plain words (paragraphs). */
  what: string[];
  use: string[];
  avoid: string[];
  /** The input the visitor can edit. */
  task: string;
  /** False for agents that make no model call (reflex agents and friends). */
  usesModel: boolean;
  /** How CriogenAI does it: a ready helper, or built from Workflow and Agent. */
  criogenai: { kind: "helper" | "primitives"; api: string };
  graph: Graph;
  params?: Param[];
  /** How the pattern runs on the engine: its code sample and cast (casts.ts). */
  engine?: EngineSpec;
}

export interface Hooks {
  line(l: Line): void;
  active(node: string | null, edge: [string, string] | null): void;
  visited(node: string): void;
  counts(c: { model: number; tool: number }): void;
  busy(b: boolean): void;
  waitHuman(draft: string, answer: (d: Decision) => void): void;
  timing(t: Timing): void;
}

/** A visitor's key never pays for a runaway loop. */
export const MAX_MODEL_CALLS = 16;

export class Stopped extends Error {
  constructor() {
    super("stopped");
    this.name = "AbortError";
  }
}

export function makeCtx(opts: {
  live: boolean;
  speed: number;
  params: Record<string, number>;
  signal: AbortSignal;
  hooks: Hooks;
}): Ctx {
  const { speed, signal, hooks } = opts;
  let id = 0;
  let last: string | null = null;
  let model = 0;
  let tools = 0;
  const check = () => {
    if (signal.aborted) throw new Stopped();
  };
  const sleep = (ms: number) =>
    new Promise<void>((resolve, reject) => {
      const t = setTimeout(resolve, ms / speed);
      signal.addEventListener("abort", () => (clearTimeout(t), reject(new Stopped())), { once: true });
    });
  const ctx: Ctx = {
    live: opts.live,
    params: opts.params,
    at(node) {
      check();
      hooks.active(node, last && last !== node ? [last, node] : null);
      hooks.visited(node);
      last = node;
    },
    say(who, kind, text, node, ms) {
      hooks.line({ id: ++id, who, kind, text, node, ms });
    },
    pause: (ms = 450) => sleep(ms),
    human(draft) {
      check();
      return new Promise<Decision>((resolve, reject) => {
        signal.addEventListener("abort", () => reject(new Stopped()), { once: true });
        hooks.waitHuman(draft, resolve);
      });
    },
    count(kind) {
      if (kind === "model") model++;
      else tools++;
      hooks.counts({ model, tool: tools });
    },
    thinking(on) {
      hooks.busy(on);
    },
    timing(t) {
      hooks.timing(t);
    },
  };
  return ctx;
}

// The playground's tiny pattern runner. Each pattern is written once as an
// async function over a `Ctx`; the context decides whether a model call is
// a scripted reply (the default, no network) or a real call from the
// browser with the visitor's key. Tools are always simulated.
//
// This is a teaching copy of each pattern's control flow in JavaScript, not
// the Criogen engine: the code tab shows the Criogen version.
import { complete, type Connection } from "./providers";

export type Kind = "user" | "model" | "tool" | "note" | "state" | "final" | "error" | "human";

export interface Line {
  id: number;
  who: string;
  kind: Kind;
  text: string;
  node?: string;
  live?: boolean;
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
  /** A real model decides (the visitor's key) instead of the script. */
  live: boolean;
  /** Slider values, for patterns with parameters. */
  params: Record<string, number>;
  /** One model call. `scripted` is the reply without a key. */
  ask(o: { node: string; who: string; system: string; prompt: string; scripted: string }): Promise<string>;
  /** One simulated tool call. */
  tool(o: { node: string; name: string; input: string; run: (input: string) => string }): Promise<string>;
  /** Highlights a node (and the edge from the last one). */
  at(node: string): void;
  say(who: string, kind: Kind, text: string, node?: string): void;
  pause(ms?: number): Promise<void>;
  /** Waits for the visitor to approve, edit or reject. */
  human(draft: string): Promise<Decision>;
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
  /** How Criogen does it: a ready helper, or built from Workflow and Agent. */
  criogen: { kind: "helper" | "primitives"; api: string };
  graph: Graph;
  params?: Param[];
  run(ctx: Ctx, input: string): Promise<string>;
}

export interface Hooks {
  line(l: Line): void;
  active(node: string | null, edge: [string, string] | null): void;
  visited(node: string): void;
  counts(c: { model: number; tool: number }): void;
  busy(b: boolean): void;
  waitHuman(draft: string, answer: (d: Decision) => void): void;
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
  conn: Connection | null;
  speed: number;
  params: Record<string, number>;
  signal: AbortSignal;
  hooks: Hooks;
}): Ctx {
  const { conn, speed, signal, hooks } = opts;
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
    live: conn != null,
    params: opts.params,
    at(node) {
      check();
      hooks.active(node, last && last !== node ? [last, node] : null);
      hooks.visited(node);
      last = node;
    },
    say(who, kind, text, node) {
      hooks.line({ id: ++id, who, kind, text, node });
    },
    pause: (ms = 450) => sleep(ms),
    async ask(o) {
      ctx.at(o.node);
      if (++model > MAX_MODEL_CALLS) throw new Error(`Stopped after ${MAX_MODEL_CALLS} model calls (the playground's cap).`);
      hooks.counts({ model, tool: tools });
      hooks.busy(true);
      try {
        let text: string;
        if (conn) {
          text = await complete(conn, o.system, o.prompt, signal);
          check();
        } else {
          await sleep(650 + Math.min(900, o.scripted.length * 4));
          text = o.scripted;
        }
        ctx.say(o.who, "model", text, o.node);
        return text;
      } finally {
        hooks.busy(false);
      }
    },
    async tool(o) {
      ctx.at(o.node);
      hooks.counts({ model, tool: ++tools });
      ctx.say(o.name, "tool", `→ ${o.input}`, o.node);
      await sleep(420);
      const out = o.run(o.input);
      ctx.say(o.name, "tool", `← ${out}`, o.node);
      return out;
    },
    human(draft) {
      check();
      return new Promise<Decision>((resolve, reject) => {
        signal.addEventListener("abort", () => reject(new Stopped()), { once: true });
        hooks.waitHuman(draft, resolve);
      });
    },
  };
  return ctx;
}

// ----------------------------------------------------------------- helpers

/** The first line that starts with `tag:` (any case), and what follows it. */
export function field(text: string, tag: string): string | null {
  const re = new RegExp(`^\\s*\\**${tag}\\**\\s*[:：]\\s*(.*)$`, "im");
  const m = text.match(re);
  return m ? m[1].trim() : null;
}

/** Everything after the first `tag:` line, to the end. */
export function after(text: string, tag: string): string | null {
  const re = new RegExp(`^\\s*\\**${tag}\\**\\s*[:：]\\s*`, "im");
  const m = re.exec(text);
  return m ? text.slice(m.index + m[0].length).trim() : null;
}

/** The first whole number in `text` within [lo, hi], or `fallback`. */
export function score(text: string, lo: number, hi: number, fallback: number): number {
  for (const m of text.matchAll(/\d+(?:\.\d+)?/g)) {
    const n = Number(m[0]);
    if (n >= lo && n <= hi) return n;
  }
  return fallback;
}

/** Which of `names` the text mentions first (case-insensitive), or `fallback`. */
export function pick(text: string, names: string[], fallback: string): string {
  const lower = text.toLowerCase();
  let best: [number, string] | null = null;
  for (const n of names) {
    const i = lower.indexOf(n.toLowerCase());
    if (i >= 0 && (!best || i < best[0])) best = [i, n];
  }
  return best ? best[1] : fallback;
}

/** A deterministic random number generator (the learning agent replays the same way). */
export function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

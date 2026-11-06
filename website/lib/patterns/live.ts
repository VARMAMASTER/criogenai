// Runs a pattern's code sample on the Criogen engine in the browser (design
// 0021): the TypeScript function in snippets/typescript/patterns.ts, as shown
// on the page, compiled to public/engine/playground.js beside the engine
// (WebAssembly). The playground swaps in the model (MODEL in the samples):
//
//   - no key: a scripted model (a FunctionModel) answers each agent from the
//     pattern's cast, matched by the agent's instructions;
//   - a key: the same FunctionModel passes each request, tools and all, to
//     the visitor's provider through the engine (Gateway.chat), from this
//     browser straight to the provider.
//
// Either way the agent loop, tools, workflows, retries and limits run in the
// engine. What it does is reported to the page (the model's calls here, the
// samples' tools, nodes and storage through engine/runtime.js) and told as a
// story: the diagram lights up, the transcript fills in.
import { MAX_MODEL_CALLS, Stopped, type Ctx, type PatternDef, type Timing } from "./engine";
import type { Connection } from "./providers";

/** A scripted answer: text, tool calls (with text before them), or typed output. */
export type Reply =
  | string
  | { say?: string; call: [name: string, args: Record<string, unknown>][] }
  | { output: Record<string, unknown> };

/** One agent of a pattern: who it is on the diagram, and what the scripted model says for it. */
export interface Actor {
  /** A phrase of the agent's instructions (or of its request). */
  match: string;
  node: string;
  who: string;
  /** One reply per turn of a conversation (the last repeats), or a function of the request. */
  replies: Reply[] | ((call: ModelCall, turn: number) => Reply);
}

/** How a pattern runs on the engine. */
export interface EngineSpec {
  /** The function in snippets/typescript/patterns.ts. */
  sample: string;
  /** Diagram nodes for the question and the answer. */
  input?: string;
  answer?: string;
  actors?: Actor[];
  /** The sample's tools and workflow nodes, by name, on the diagram. */
  tools?: Record<string, string>;
  nodes?: Record<string, string>;
  /** Tells what a workflow node of plain code does (it makes no model call to show). */
  narrate?: (node: string, phase: "start" | "end", value: Record<string, unknown>, ctx: Ctx) => void;
  /** Human review: the node that waits for the visitor, and where approval and rejection lead. */
  review?: { ask: string; approved: string; rejected: string };
  /** Extra arguments after the task (sliders), from the visitor's settings. */
  settings?: (params: Record<string, number>) => unknown;
  /** Narration of the sample's storage calls (the learning agent). */
  storage?: (op: "recall" | "remember", key: string, value: unknown, ctx: Ctx) => Promise<void> | void;
}

interface Message {
  role: string;
  content?: { type: string; text?: string; name?: string; args?: unknown; value?: unknown }[];
}

/** What a FunctionModel receives (the engine's ModelRequest). */
export interface ModelCall {
  messages: Message[];
  tools?: { name: string }[];
  [key: string]: unknown;
}

interface Engine {
  samples: Record<string, (...args: unknown[]) => Promise<string>>;
  useModel(model: unknown): void;
  observe(observer: unknown): void;
  ready(): Promise<{ ok: boolean; engine: { error?: string } }>;
  FunctionModel: new (respond: (call: ModelCall) => unknown, options?: { name?: string }) => unknown;
  Gateway: new (options?: Record<string, unknown>) => {
    chat(model: unknown, input: unknown, options?: Record<string, unknown>): Promise<{ message: Message }>;
  };
  openai(model: string, options: Record<string, unknown>): unknown;
  anthropic(model: string, options: Record<string, unknown>): unknown;
  gemini(model: string, options: Record<string, unknown>): unknown;
  openrouter(model: string, options: Record<string, unknown>): unknown;
}

let loading: Promise<Engine> | undefined;

/** The engine and the samples, loaded once (about 600 KB compressed, cached after). */
export function loadEngine(): Promise<Engine> {
  loading ??= (async () => {
    const url = "/engine/playground.js";
    const engine = (await import(/* webpackIgnore: true */ /* turbopackIgnore: true */ url)) as Engine;
    const report = await engine.ready();
    if (!report.ok) throw new Error(`The engine did not load in this browser: ${report.engine.error ?? "unknown error"}`);
    return engine;
  })().catch((error) => {
    loading = undefined;
    throw error;
  });
  return loading;
}

const text = (m: Message) =>
  (m.content ?? [])
    .filter((b) => b.type === "text")
    .map((b) => b.text ?? "")
    .join("");

/** The agent's instructions (its system message). */
function instructions(call: ModelCall): string {
  return call.messages
    .filter((m) => m.role === "system")
    .map(text)
    .join("\n");
}

/** Everything the agent was asked. */
function asked(call: ModelCall): string {
  return call.messages
    .filter((m) => m.role === "user")
    .map(text)
    .join("\n");
}

const short = (value: unknown, n = 160) => {
  const s = typeof value === "string" ? value : JSON.stringify(value);
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
};

/** A reply as the engine takes it from a FunctionModel. */
function answer(reply: Reply, turn: number): unknown {
  if (typeof reply === "string") return reply;
  if ("output" in reply) return { toolCalls: [{ name: "final_output", args: reply.output }] };
  const content: unknown[] = reply.say ? [{ type: "text", text: reply.say }] : [];
  reply.call.forEach(([name, args], i) => content.push({ type: "tool_call", id: `call_${turn}_${i}_${name}`, name, args }));
  return { message: { id: `msg_scripted_${Math.random().toString(36).slice(2)}`, role: "assistant", content } };
}

/** What the transcript says about a reply. */
function describe(message: Message, stopReason?: string | null): string {
  const parts: string[] = [];
  const said = text(message).trim();
  if (said) parts.push(said);
  for (const b of message.content ?? []) {
    if (b.type !== "tool_call") continue;
    parts.push(b.name === "final_output" ? `→ answers: ${short(b.args)}` : `→ calls ${b.name}(${short(b.args, 90)})`);
  }
  if (parts.length) return parts.join("\n");
  return stopReason === "length"
    ? "(no text: the model spent its whole token budget thinking before it wrote anything)"
    : "(no text: the model answered with nothing)";
}

type Span = { kind: "model" | "tool" | "wait"; s: number; e: number };

/**
 * Splits [start, end] by what was happening, by wall clock: parallel calls
 * overlap and are not counted twice. A model answering wins, then a tool
 * running, then the playground pausing; the rest is Criogen and the page.
 */
function breakdown(spans: Span[], start: number, end: number): Pick<Timing, "model" | "tool" | "wait" | "engine"> {
  const points = [...new Set([start, end, ...spans.flatMap((x) => [x.s, x.e])])].filter((t) => t >= start && t <= end).sort((a, b) => a - b);
  const out = { model: 0, tool: 0, wait: 0, engine: 0 };
  for (let i = 0; i < points.length - 1; i++) {
    const [a, b] = [points[i], points[i + 1]];
    const mid = (a + b) / 2;
    const on = (kind: Span["kind"]) => spans.some((x) => x.kind === kind && x.s <= mid && x.e >= mid);
    out[on("model") ? "model" : on("tool") ? "tool" : on("wait") ? "wait" : "engine"] += b - a;
  }
  return out;
}

function describeReply(reply: Reply): string {
  if (typeof reply === "string") return reply;
  if ("output" in reply) return `→ answers: ${short(reply.output)}`;
  return [reply.say, ...reply.call.map(([n, a]) => `→ calls ${n}(${short(a, 90)})`)].filter(Boolean).join("\n");
}

function realModel(engine: Engine, conn: Connection): unknown {
  const options = { apiKey: conn.key };
  switch (conn.provider) {
    case "openai":
      return engine.openai(conn.model, options);
    case "anthropic":
      return engine.anthropic(conn.model, options);
    case "gemini":
      return engine.gemini(conn.model, options);
    case "openrouter":
      return engine.openrouter(conn.model, options);
  }
}

/**
 * Runs `def`'s sample on the engine and tells it through `ctx`. Returns the
 * sample's answer.
 */
export async function runOnEngine(
  def: PatternDef & { engine: EngineSpec },
  ctx: Ctx,
  input: string,
  opts: { conn: Connection | null; speed: number; signal: AbortSignal },
): Promise<string> {
  const spec = def.engine;
  const { conn, signal } = opts;
  const engine = await loadEngine();
  const stopped = () => {
    if (signal.aborted) throw new Stopped();
  };
  const actors = spec.actors ?? [];
  // An agent is known by its instructions first, then by what it was asked
  // (text one agent passes another must not be taken for its identity).
  const actorFor = (call: ModelCall): Actor => {
    const own = instructions(call);
    const request = asked(call);
    return (
      actors.find((a) => own.includes(a.match)) ??
      actors.find((a) => request.includes(a.match)) ?? { match: "", node: spec.answer ?? "answer", who: "agent", replies: ["Done."] }
    );
  };
  let calls = 0;
  let toolCalls = 0;
  // Where the time goes: model calls, tool runs, and the playground's own
  // pauses (to tell the story, or waiting for you); the rest is Criogen.
  const spans: Span[] = [];
  const timed = async <T>(kind: Span["kind"], work: () => Promise<T>): Promise<T> => {
    const s = performance.now();
    try {
      return await work();
    } finally {
      spans.push({ kind, s, e: performance.now() });
    }
  };
  const show: Ctx = { ...ctx, pause: (ms) => timed("wait", () => ctx.pause(ms)), human: (draft) => timed("wait", () => ctx.human(draft)) };
  const gateway = conn ? new engine.Gateway({ retry: { maxAttempts: 2 } }) : null;
  const upstream = conn ? realModel(engine, conn) : null;

  const model = new engine.FunctionModel(
    async (call: ModelCall) => {
      stopped();
      const actor = actorFor(call);
      calls += 1;
      if (conn && calls > MAX_MODEL_CALLS) throw new Error(`Stopped after ${MAX_MODEL_CALLS} model calls (the playground's cap for your key).`);
      ctx.at(actor.node);
      ctx.count("model");
      ctx.thinking(true);
      try {
        if (gateway && upstream) {
          // Reasoning models think within this budget too: leave room to write.
          const s = performance.now();
          const reply = (await timed("model", () =>
            gateway.chat(upstream, call.messages, { tools: call.tools?.length ? call.tools : undefined, maxTokens: 4000 }),
          )) as { message: Message; stopReason?: string | null };
          stopped();
          ctx.say(actor.who, "model", describe(reply.message, reply.stopReason), actor.node, performance.now() - s);
          return { message: reply.message };
        }
        const turn = call.messages.filter((m) => m.role === "assistant").length;
        const reply =
          typeof actor.replies === "function" ? actor.replies(call, turn) : actor.replies[Math.min(turn, actor.replies.length - 1)];
        const said = describeReply(reply);
        // The scripted model's "thinking" is a pause, to read along.
        const s = performance.now();
        await timed("model", () => ctx.pause(600 + Math.min(900, said.length * 4)));
        ctx.say(actor.who, "model", said, actor.node, performance.now() - s);
        return answer(reply, turn);
      } finally {
        ctx.thinking(false);
      }
    },
    { name: conn ? conn.model : "scripted" },
  );

  engine.observe({
    tool: async (name: string, phase: string, value: unknown, started?: number, ended?: number) => {
      stopped();
      const node = spec.tools?.[name] ?? spec.answer ?? "answer";
      if (phase === "start") {
        // The model's line already shows the call and its arguments.
        ctx.at(node);
        ctx.count("tool");
        toolCalls += 1;
        await show.pause(380);
      } else {
        const ms = started !== undefined && ended !== undefined ? ended - started : undefined;
        if (started !== undefined && ended !== undefined) spans.push({ kind: "tool", s: started, e: ended });
        ctx.say(`${name}()`, "tool", `← ${phase === "error" ? `threw ${String(value)}` : short(value)}`, node, ms);
      }
    },
    node: async (name: string, phase: "start" | "end", value: Record<string, unknown>) => {
      stopped();
      const node = spec.nodes?.[name];
      if (node && phase === "start") ctx.at(node);
      spec.narrate?.(name, phase, value ?? {}, show);
      if (node && phase === "start" && !def.usesModel) await show.pause(320);
    },
    storage: (op: "recall" | "remember", _ns: string, key: string, value: unknown) => spec.storage?.(op, key, value, show),
    note: (note: string) => ctx.say("playground", "note", note),
  });
  engine.useModel(model);

  if (spec.input) ctx.at(spec.input);
  if (def.usesModel) ctx.say("user", "user", input, spec.input);
  const extra = spec.settings ? spec.settings(ctx.params) : undefined;
  const human = {
    askPerson: async (waiting: { draft?: string }) => {
      if (spec.review) ctx.at(spec.review.ask);
      const decision = await show.human(String(waiting?.draft ?? ""));
      if (spec.review) ctx.at(decision.kind === "reject" ? spec.review.rejected : spec.review.approved);
      if (decision.kind === "approve") return "approve";
      if (decision.kind === "edit") return { decision: "edit", text: decision.text };
      return { decision: "reject" };
    },
  };
  const sample = engine.samples[spec.sample];
  if (!sample) throw new Error(`no sample "${spec.sample}"`);
  const start = performance.now();
  try {
    const out = await sample(input, def.id === "human-in-the-loop" ? human : extra);
    stopped();
    if (spec.answer) ctx.at(spec.answer);
    return out;
  } catch (error) {
    if (signal.aborted) throw new Stopped();
    throw error;
  } finally {
    engine.observe({});
    if (!signal.aborted) {
      const end = performance.now();
      ctx.timing({ total: end - start, ...breakdown(spans, start, end), live: conn != null, modelCalls: calls, toolCalls });
    }
  }
}

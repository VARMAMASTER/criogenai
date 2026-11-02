// What a run cost: which models answered, with their calls and tokens (from
// the engine's per-message metadata), the call's wall time, and a cost
// estimate from prices the caller supplies (they change too often to ship).

/** One model used in a run. */
export interface ModelUse {
  provider: string;
  model: string;
  /** Model calls this run made to it. */
  calls: number;
  /** Tokens: input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens. */
  usage: Record<string, number>;
}

/** Facts added to every result. */
export interface RunFacts {
  /** Wall time of the call, in milliseconds. */
  durationMs: number;
  /** The provider and model that wrote the last answer (a fallback shows here). */
  answeredBy: { provider: string; model: string } | null;
  /** Each model this run called, in first-use order. */
  models: ModelUse[];
}

type Message = { role?: unknown; metadata?: { provider?: string; model?: string; usage?: Record<string, number> } };

/** Adds `durationMs`, `answeredBy` and `models` to an engine result. Only this
 * call's model calls count: the messages after the last user message. */
export function withRunFacts<T extends { messages: Record<string, unknown>[] }>(raw: T, started: number): T & RunFacts {
  // Only run results: a thread's history (a list) and other replies pass through.
  if (raw === null || typeof raw !== "object" || Array.isArray(raw) || !Array.isArray(raw.messages)) {
    return raw as T & RunFacts;
  }
  const messages = raw.messages as Message[];
  let from = 0;
  messages.forEach((m, i) => {
    if (m.role === "user") from = i + 1;
  });
  const uses = new Map<string, ModelUse>();
  let answeredBy: RunFacts["answeredBy"] = null;
  for (const m of messages.slice(from)) {
    const meta = m.metadata;
    if (m.role !== "assistant" || !meta || (!meta.provider && !meta.model)) continue;
    const provider = meta.provider ?? "";
    const model = meta.model ?? "";
    const key = `${provider}\u0000${model}`;
    const use = uses.get(key) ?? { provider, model, calls: 0, usage: {} };
    use.calls += 1;
    for (const [name, value] of Object.entries(meta.usage ?? {})) {
      if (typeof value === "number") use.usage[name] = (use.usage[name] ?? 0) + value;
    }
    uses.set(key, use);
    answeredBy = { provider, model };
  }
  return { ...raw, durationMs: Date.now() - started, answeredBy, models: [...uses.values()] };
}

/** USD per million tokens. */
export interface Price {
  input: number;
  output: number;
  /** Cached input tokens (default: the input price). */
  cacheRead?: number;
}

/**
 * The cost of a run in USD from your prices, keyed by `"provider:model"` or by
 * model name. `null` when a model that was used has no price. Reasoning tokens
 * are counted as output tokens.
 */
export function estimateCost(result: Pick<RunFacts, "models">, prices: Record<string, Price>): number | null {
  let total = 0;
  for (const use of result.models) {
    const price = prices[`${use.provider}:${use.model}`] ?? prices[use.model];
    if (!price) return null;
    const cached = use.usage.cache_read_tokens ?? 0;
    const input = Math.max((use.usage.input_tokens ?? 0) - cached, 0);
    const output = use.usage.output_tokens ?? 0;
    total += (input * price.input + cached * (price.cacheRead ?? price.input) + output * price.output) / 1_000_000;
  }
  return total;
}

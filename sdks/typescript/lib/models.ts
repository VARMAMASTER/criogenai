// Models (design 0012 §2): real providers, and FakeModel for tests. Each turns
// into a small JSON spec the engine builds the model from. Strings work too:
// "openai:gpt-5-mini" takes its key from OPENAI_API_KEY.

export interface ModelOptions {
  apiKey?: string;
  baseUrl?: string;
}

const KEYS: Record<string, string> = {
  openai: "OPENAI_API_KEY",
  gemini: "GEMINI_API_KEY",
  anthropic: "ANTHROPIC_API_KEY",
  openrouter: "OPENROUTER_API_KEY",
};

/** A model on a provider's API. */
export class ProviderModel {
  readonly kind: string;
  readonly model: string;
  readonly baseUrl?: string;
  readonly #apiKey?: string;
  readonly #name?: string;

  constructor(kind: string, model: string, options: ModelOptions & { name?: string } = {}) {
    this.kind = kind;
    this.model = model;
    this.baseUrl = options.baseUrl;
    this.#apiKey = options.apiKey ?? (KEYS[kind] ? process.env[KEYS[kind]] : undefined);
    this.#name = options.name;
  }

  /** The JSON spec (with the key; never logged). */
  spec(): Record<string, unknown> {
    return { kind: this.kind, model: this.model, apiKey: this.#apiKey, baseUrl: this.baseUrl, name: this.#name };
  }

  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

export const openai = (model: string, options?: ModelOptions) => new ProviderModel("openai", model, options);
export const gemini = (model: string, options?: ModelOptions) => new ProviderModel("gemini", model, options);
export const anthropic = (model: string, options?: ModelOptions) => new ProviderModel("anthropic", model, options);
export const openrouter = (model: string, options?: ModelOptions) => new ProviderModel("openrouter", model, options);
export const ollama = (model: string, options?: { baseUrl?: string }) => new ProviderModel("ollama", model, options);
/** Any OpenAI-compatible server (vLLM, LM Studio, a gateway). */
export const openaiCompatible = (model: string, options: ModelOptions & { baseUrl: string; name?: string }) =>
  new ProviderModel("openai_compatible", model, options);

/** One scripted answer of a FakeModel. */
export type FakeReply = { text: string } | { toolCalls: { name: string; args: Record<string, unknown> }[] };

export const FakeReply = {
  text: (text: string): FakeReply => ({ text }),
  toolCall: (name: string, args: Record<string, unknown> = {}): FakeReply => ({ toolCalls: [{ name, args }] }),
  toolCalls: (calls: [string, Record<string, unknown>][]): FakeReply => ({
    toolCalls: calls.map(([name, args]) => ({ name, args })),
  }),
};

/**
 * A simulated model for tests: answers with the scripted replies in order (a
 * string is a text reply), then echoes. Never a network call.
 */
export class FakeModel {
  readonly replies: FakeReply[];
  readonly delayMs: number;
  /** The first n calls fail with `rate_limited` (transient), to test retries. */
  readonly failFirst: number;
  /**
   * Choose each reply by the conversation's turn (the first reply for a run's
   * first model call, the second for its second, then the last repeats)
   * instead of using replies up: one model serves many runs, also at once.
   */
  readonly byTurn: boolean;

  constructor(replies: (string | FakeReply)[] = [], options: { delayMs?: number; failFirst?: number; byTurn?: boolean } = {}) {
    this.replies = replies.map((r) => (typeof r === "string" ? FakeReply.text(r) : r));
    this.delayMs = options.delayMs ?? 0;
    this.failFirst = options.failFirst ?? 0;
    this.byTurn = options.byTurn ?? false;
  }

  spec(): Record<string, unknown> {
    return { kind: "fake", replies: this.replies, delayMs: this.delayMs, failFirst: this.failFirst, byTurn: this.byTurn };
  }
}

export type Model = string | ProviderModel | FakeModel;

/** The JSON spec of a model object or "provider:model" string. */
export function modelSpec(model: Model): Record<string, unknown> {
  if (typeof model === "string") {
    const at = model.indexOf(":");
    const provider = at > 0 ? model.slice(0, at) : "";
    const known = ["openai", "gemini", "anthropic", "openrouter", "ollama"];
    if (!known.includes(provider)) {
      throw new TypeError(`model "${model}": use "provider:model" with provider one of ${known.join(", ")}`);
    }
    return new ProviderModel(provider, model.slice(at + 1)).spec();
  }
  if (model && typeof (model as { spec?: unknown }).spec === "function") return model.spec();
  throw new TypeError(`not a model: ${String(model)}`);
}

/**
 * Turns text into vectors, for long-term memory searched by meaning
 * (`memory.semantic(embedder)`, design 0017 §3). Built from the same
 * providers as models; keys come from the same environment variables.
 */
export class Embedder {
  /** @internal The JSON spec (with the key; never logged). */
  readonly spec: Record<string, unknown>;

  private constructor(spec: Record<string, unknown>) {
    this.spec = spec;
  }

  private static provider(kind: string, model: string, options: ModelOptions & { dimensions?: number } = {}): Embedder {
    const apiKey = options.apiKey ?? (KEYS[kind] ? process.env[KEYS[kind]] : undefined);
    return new Embedder({ kind, model, apiKey, baseUrl: options.baseUrl, dimensions: options.dimensions });
  }

  /** OpenAI's /embeddings (key from OPENAI_API_KEY). */
  static openai(model = "text-embedding-3-small", options: { apiKey?: string; dimensions?: number } = {}): Embedder {
    return Embedder.provider("openai", model, options);
  }

  /** Gemini's OpenAI-compatible /embeddings (GEMINI_API_KEY). */
  static gemini(model = "text-embedding-004", options: { apiKey?: string } = {}): Embedder {
    return Embedder.provider("gemini", model, options);
  }

  /** Ollama on this machine (or `baseUrl`): nothing leaves your network. */
  static ollama(model = "nomic-embed-text", options: { baseUrl?: string } = {}): Embedder {
    return Embedder.provider("ollama", model, options);
  }

  /** Any OpenAI-compatible /embeddings (vLLM, a gateway). */
  static openaiCompatible(model: string, options: { baseUrl: string; apiKey?: string; dimensions?: number }): Embedder {
    const embedder = Embedder.provider("openai_compatible", model, options);
    embedder.spec.name = "custom";
    return embedder;
  }

  /**
   * For tests, never a network call: texts sharing words are close;
   * `synonyms` makes different words count as the same (`{ car: "automobile" }`);
   * `failing` makes every call fail (search then falls back to keywords).
   */
  static fake(options: { synonyms?: Record<string, string>; failing?: boolean } = {}): Embedder {
    return new Embedder({ kind: "fake", synonyms: Object.entries(options.synonyms ?? {}), failing: options.failing ?? false });
  }

  toJSON(): Record<string, unknown> {
    return { kind: this.spec.kind, model: this.spec.model };
  }
}

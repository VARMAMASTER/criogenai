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

  constructor(replies: (string | FakeReply)[] = [], options: { delayMs?: number } = {}) {
    this.replies = replies.map((r) => (typeof r === "string" ? FakeReply.text(r) : r));
    this.delayMs = options.delayMs ?? 0;
  }

  spec(): Record<string, unknown> {
    return { kind: "fake", replies: this.replies, delayMs: this.delayMs };
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

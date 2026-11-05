// Models (design 0012 §2): real providers, and FakeModel for tests. Each turns
// into a small JSON spec the engine builds the model from. Strings work too:
// "openai:gpt-5-mini" takes its key from OPENAI_API_KEY, "deepseek:deepseek-chat"
// from DEEPSEEK_API_KEY (every name in KEYS, plus ollama, lmstudio and kaggle).

import { envVar, hasEnv } from "./host-env";
import { native } from "./native";

export interface ModelOptions {
  apiKey?: string;
  baseUrl?: string;
}

/**
 * Where each named provider's key lives (design 0006 §4.1). The tests check
 * this against the engine's own table (`presets()` in the native module).
 */
export const KEYS: Record<string, string> = {
  openai: "OPENAI_API_KEY",
  gemini: "GEMINI_API_KEY",
  anthropic: "ANTHROPIC_API_KEY",
  openrouter: "OPENROUTER_API_KEY",
  deepseek: "DEEPSEEK_API_KEY",
  mistral: "MISTRAL_API_KEY",
  xai: "XAI_API_KEY",
  groq: "GROQ_API_KEY",
  cerebras: "CEREBRAS_API_KEY",
  together: "TOGETHER_API_KEY",
  fireworks: "FIREWORKS_API_KEY",
  qwen: "DASHSCOPE_API_KEY",
  moonshot: "MOONSHOT_API_KEY",
  zai: "ZAI_API_KEY",
};

/** Named providers that need no key (servers on this machine). */
const LOCAL = ["ollama", "lmstudio"];

/** A model on a provider's API. */
export class ProviderModel {
  readonly kind: string;
  readonly model: string;
  readonly baseUrl?: string;
  readonly #apiKey?: string;
  readonly #name?: string;

  readonly #allowInsecureHttp?: boolean;

  constructor(kind: string, model: string, options: ModelOptions & { name?: string; allowInsecureHttp?: boolean } = {}) {
    this.kind = kind;
    this.model = model;
    this.baseUrl = options.baseUrl;
    this.#apiKey = options.apiKey ?? (KEYS[kind] ? envVar(KEYS[kind]) : undefined);
    if (KEYS[kind] && !this.#apiKey && !options.baseUrl) {
      throw new Error(
        hasEnv()
          ? `no API key for ${kind}: set ${KEYS[kind]} in the environment or a .env file (loadEnv()), or pass apiKey`
          : `no API key for ${kind}: pass apiKey (a browser has no environment variables)`,
      );
    }
    this.#name = options.name;
    this.#allowInsecureHttp = options.allowInsecureHttp;
  }

  /** The JSON spec (with the key; never logged). */
  spec(): Record<string, unknown> {
    return {
      kind: this.kind,
      model: this.model,
      apiKey: this.#apiKey,
      baseUrl: this.baseUrl,
      name: this.#name,
      allowInsecureHttp: this.#allowInsecureHttp,
    };
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
/** LM Studio on this machine (or `baseUrl`), no key. */
export const lmstudio = (model: string, options?: { baseUrl?: string }) => new ProviderModel("lmstudio", model, options);
/** DeepSeek (DEEPSEEK_API_KEY), e.g. deepseek("deepseek-chat"). */
export const deepseek = (model: string, options?: ModelOptions) => new ProviderModel("deepseek", model, options);
/** Mistral AI (MISTRAL_API_KEY), e.g. mistral("mistral-large-latest"). */
export const mistral = (model: string, options?: ModelOptions) => new ProviderModel("mistral", model, options);
/** xAI's Grok models (XAI_API_KEY). */
export const xai = (model: string, options?: ModelOptions) => new ProviderModel("xai", model, options);
/** Groq's fast inference for open models (GROQ_API_KEY). */
export const groq = (model: string, options?: ModelOptions) => new ProviderModel("groq", model, options);
/** Cerebras's fast inference for open models (CEREBRAS_API_KEY). */
export const cerebras = (model: string, options?: ModelOptions) => new ProviderModel("cerebras", model, options);
/** Together AI's open models (TOGETHER_API_KEY). */
export const together = (model: string, options?: ModelOptions) => new ProviderModel("together", model, options);
/** Fireworks AI's open models (FIREWORKS_API_KEY). */
export const fireworks = (model: string, options?: ModelOptions) => new ProviderModel("fireworks", model, options);
/**
 * Qwen on Alibaba Cloud Model Studio, international endpoint (DASHSCOPE_API_KEY).
 * China: `baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1"`.
 */
export const qwen = (model: string, options?: ModelOptions) => new ProviderModel("qwen", model, options);
/** Moonshot AI's Kimi models (MOONSHOT_API_KEY). China keys: `baseUrl: "https://api.moonshot.cn/v1"`. */
export const moonshot = (model: string, options?: ModelOptions) => new ProviderModel("moonshot", model, options);
/** Z.ai's GLM models (ZAI_API_KEY). China keys: `baseUrl: "https://open.bigmodel.cn/api/paas/v4"`. */
export const zai = (model: string, options?: ModelOptions) => new ProviderModel("zai", model, options);
/**
 * Any OpenAI-compatible server. `allowInsecureHttp`: send the key over plain
 * `http://` to another machine (a vLLM server on your LAN); refused by
 * default, because a key sent over http is readable by anyone on the path.
 */
export const openaiCompatible = (
  model: string,
  options: ModelOptions & { baseUrl: string; name?: string; allowInsecureHttp?: boolean },
) => new ProviderModel("openai_compatible", model, options);

/**
 * Any server that speaks Anthropic's Messages API: a self-hosted gateway, or a
 * provider's Anthropic-style endpoint (DeepSeek, Z.ai, Qwen and Moonshot offer one).
 */
export const anthropicCompatible = (
  model: string,
  options: ModelOptions & { baseUrl: string; name?: string; allowInsecureHttp?: boolean },
) => new ProviderModel("anthropic_compatible", model, options);

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
  /** The model name in replies ("fake:<name>"), to tell two fake models apart. */
  readonly name: string | undefined;

  constructor(
    replies: (string | FakeReply)[] = [],
    options: { delayMs?: number; failFirst?: number; byTurn?: boolean; name?: string } = {},
  ) {
    this.replies = replies.map((r) => (typeof r === "string" ? FakeReply.text(r) : r));
    this.delayMs = options.delayMs ?? 0;
    this.failFirst = options.failFirst ?? 0;
    this.byTurn = options.byTurn ?? false;
    this.name = options.name;
  }

  spec(): Record<string, unknown> {
    return {
      kind: "fake",
      replies: this.replies,
      delayMs: this.delayMs,
      failFirst: this.failFirst,
      byTurn: this.byTurn,
      name: this.name,
    };
  }
}

/**
 * Kaggle's Model Proxy (design 0020): the hosted models your Kaggle account's
 * AI-model allowance pays for. Credentials, in order: `apiToken`; `proxyUrl`
 * and `proxyToken`; else MODEL_PROXY_URL and MODEL_PROXY_API_KEY,
 * KAGGLE_API_TOKEN, or the `access_token` file in KAGGLE_CONFIG_DIR or
 * ~/.kaggle. The engine mints the short-lived proxy token and renews it.
 * For prototypes and tests, not production traffic.
 */
export interface KaggleOptions {
  apiToken?: string;
  proxyUrl?: string;
  proxyToken?: string;
  /** Where proxy tokens are minted (tests). */
  apiBase?: string;
}

export class KaggleModel {
  readonly kind = "kaggle";
  readonly model: string;
  readonly #options: KaggleOptions;

  constructor(model: string, options: KaggleOptions = {}) {
    this.model = model;
    this.#options = options;
  }

  /** The JSON spec (with any token; never logged). */
  spec(): Record<string, unknown> {
    const o = this.#options;
    return {
      kind: this.kind,
      model: this.model,
      apiKey: o.apiToken,
      proxyUrl: o.proxyUrl,
      proxyToken: o.proxyToken,
      apiBase: o.apiBase,
    };
  }

  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

/** A model on Kaggle's Model Proxy, e.g. kaggle("google/gemini-3.6-flash"). */
export const kaggle = (model: string, options?: KaggleOptions) => new KaggleModel(model, options);


/**
 * What a model function receives: the request as the engine sends it to a
 * provider (`ModelRequest` in schemas/criogen.schema.json): the messages so
 * far, the tools it may call, the settings and the output schema.
 */
export interface ModelCall {
  messages: Record<string, unknown>[];
  tools?: Record<string, unknown>[];
  settings?: Record<string, unknown>;
  output?: Record<string, unknown> | null;
  [key: string]: unknown;
}

/**
 * A model function's answer: text, tool calls (`FakeReply.toolCall`), an
 * exact assistant message, or a classified error (`{ error: { kind:
 * "rate_limited", message } }` is retried as a provider's would be).
 */
export type ModelAnswer = string | FakeReply | { message: Record<string, unknown> } | { error: { kind: string; message: string } };

/** Releases a function model's engine handle once the object is gone. */
const releaseHandle =
  typeof FinalizationRegistry === "function"
    ? new FinalizationRegistry<number>((handle) => {
        try {
          native.releaseFunction(handle);
        } catch {
          // The engine is gone too.
        }
      })
    : undefined;

/**
 * A model written in JavaScript (design 0021 §7): for each model call the
 * engine calls `respond` with the request, and the agent loop (tools,
 * approvals, retries, limits) goes on with its answer. For a model that runs
 * in the page (WebGPU, a browser's built-in model), your own client, or
 * scripted replies chosen by what the request says (tests and demos, where
 * several agents share one model). If `respond` throws, the call fails with
 * `model_failed`.
 */
export class FunctionModel {
  readonly kind = "function";
  readonly name: string;
  readonly #respond: (call: ModelCall) => ModelAnswer | Promise<ModelAnswer>;
  #handle: number | undefined;

  constructor(respond: (call: ModelCall) => ModelAnswer | Promise<ModelAnswer>, options: { name?: string } = {}) {
    if (typeof respond !== "function") throw new TypeError("FunctionModel needs a function: (call) => answer");
    this.#respond = respond;
    this.name = options.name ?? "function";
  }

  /** The JSON spec: the function is registered with the engine on first use. */
  spec(): Record<string, unknown> {
    if (this.#handle === undefined) {
      const respond = this.#respond;
      this.#handle = native.registerFunction(async (request: string) => {
        try {
          const answer = await respond(JSON.parse(request) as ModelCall);
          return JSON.stringify(typeof answer === "string" ? { text: answer } : answer);
        } catch (error) {
          return JSON.stringify({ crash: error instanceof Error ? `${error.name}: ${error.message}` : String(error) });
        }
      });
      releaseHandle?.register(this, this.#handle);
    }
    return { kind: "function", handle: this.#handle, name: this.name };
  }

  toJSON(): Record<string, unknown> {
    return { kind: "function", name: this.name };
  }
}

export type Model = string | ProviderModel | KaggleModel | FakeModel | FunctionModel;

/** The JSON spec of a model object or "provider:model" string. */
export function modelSpec(model: Model): Record<string, unknown> {
  if (typeof model === "string") {
    const at = model.indexOf(":");
    const provider = at > 0 ? model.slice(0, at) : "";
    const known = [...Object.keys(KEYS), ...LOCAL, "kaggle"];
    if (!known.includes(provider)) {
      throw new TypeError(`model "${model}": use "provider:model" with provider one of ${known.join(", ")}`);
    }
    if (provider === "kaggle") return new KaggleModel(model.slice(at + 1)).spec();
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
    const apiKey = options.apiKey ?? (KEYS[kind] ? envVar(KEYS[kind]) : undefined);
    return new Embedder({ kind, model, apiKey, baseUrl: options.baseUrl, dimensions: options.dimensions });
  }

  /** OpenAI's /embeddings (key from OPENAI_API_KEY). */
  static openai(model = "text-embedding-3-small", options: { apiKey?: string; dimensions?: number } = {}): Embedder {
    return Embedder.provider("openai", model, options);
  }

  /** Gemini's OpenAI-compatible /embeddings (GEMINI_API_KEY). */
  static gemini(model = "gemini-embedding-001", options: { apiKey?: string } = {}): Embedder {
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

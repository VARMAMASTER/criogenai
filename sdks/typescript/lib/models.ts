// Models (design 0012 §2): real providers, and FakeModel for tests. Each turns
// into a small JSON spec the engine builds the model from. Strings work too:
// "openai:gpt-5-mini" takes its key from OPENAI_API_KEY, "deepseek:deepseek-chat"
// from DEEPSEEK_API_KEY (every name in KEYS, plus ollama, lmstudio and kaggle).

import { errorFor } from "./errors";
import { envVar, hasEnv } from "./host-env";
import { native } from "./native";

/** Options of the provider functions (`openai`, `anthropic`, `gemini`, ...). */
export interface ModelOptions {
  /** The API key. Without it, the key comes from the provider's environment variable (e.g. OPENAI_API_KEY). */
  apiKey?: string;
  /** Another address for the provider's API (a proxy, a regional endpoint). Without it, the provider's own. */
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

/**
 * A model on a provider's API. The provider functions (`openai("...")`,
 * `anthropic("...")`, ...) make these; construct one directly only for a
 * provider name held in a variable.
 *
 * The API key is kept private: it is not a property, and `toJSON()` (so
 * `JSON.stringify`) leaves it out.
 */
export class ProviderModel {
  /** The provider: "openai", "anthropic", "gemini", ..., "openai_compatible" or "anthropic_compatible". */
  readonly kind: string;
  /** The model name, as the provider knows it (e.g. "gpt-5-mini"). */
  readonly model: string;
  /** The API address, when not the provider's default. */
  readonly baseUrl?: string;
  readonly #apiKey?: string;
  readonly #name?: string;

  readonly #allowInsecureHttp?: boolean;

  /**
   * @param kind The provider name (as in "provider:model" strings), "openai_compatible" or "anthropic_compatible".
   * @param model The model name, as the provider knows it.
   * @param options `apiKey` and `baseUrl`; for the compatible kinds also `name` (shown as the provider) and `allowInsecureHttp`.
   * @throws {Error} The provider needs a key, none was given or found in its environment variable, and there is no `baseUrl`.
   */
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

  /** `{ kind, model }`, without the key: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

/**
 * A model on OpenAI's API (OPENAI_API_KEY), e.g. openai("gpt-5-mini"). The same as the string "openai:gpt-5-mini".
 * @param model The model name, e.g. "gpt-5-mini".
 * @param options `apiKey` (else OPENAI_API_KEY) and `baseUrl`.
 * @throws {Error} No key given and OPENAI_API_KEY is not set.
 * @example
 * import { Agent, openai } from "criogen";
 *
 * const agent = new Agent({ model: openai("gpt-5-mini"), instructions: "Answer in one sentence." });
 */
export const openai = (model: string, options?: ModelOptions) => new ProviderModel("openai", model, options);
/**
 * A Google Gemini model through Gemini's OpenAI-compatible API (GEMINI_API_KEY), e.g. gemini("gemini-2.5-flash").
 * @param model The model name.
 * @param options `apiKey` (else GEMINI_API_KEY) and `baseUrl`.
 * @throws {Error} No key given and GEMINI_API_KEY is not set.
 */
export const gemini = (model: string, options?: ModelOptions) => new ProviderModel("gemini", model, options);
/**
 * A Claude model on Anthropic's Messages API (ANTHROPIC_API_KEY), e.g. anthropic("claude-sonnet-5-5").
 * @param model The model name.
 * @param options `apiKey` (else ANTHROPIC_API_KEY) and `baseUrl`.
 * @throws {Error} No key given and ANTHROPIC_API_KEY is not set.
 * @example
 * import { Agent, anthropic, openai } from "criogen";
 *
 * // Claude first; OpenAI when Claude fails with a transient error after its retries.
 * const agent = new Agent({ model: anthropic("claude-sonnet-5-5"), fallbacks: [openai("gpt-5-mini")] });
 */
export const anthropic = (model: string, options?: ModelOptions) => new ProviderModel("anthropic", model, options);
/**
 * A model on OpenRouter (OPENROUTER_API_KEY), named as OpenRouter names it, e.g. openrouter("meta-llama/llama-3.3-70b-instruct").
 * @param model The model name, with its maker's prefix.
 * @param options `apiKey` (else OPENROUTER_API_KEY) and `baseUrl`.
 * @throws {Error} No key given and OPENROUTER_API_KEY is not set.
 */
export const openrouter = (model: string, options?: ModelOptions) => new ProviderModel("openrouter", model, options);
/**
 * A model on Ollama on this machine, no key, e.g. ollama("llama3.2"): nothing leaves your network.
 * @param model The model name, as `ollama list` shows it.
 * @param options `baseUrl`: another Ollama server.
 * @example
 * import { Agent, ollama } from "criogen";
 *
 * const agent = new Agent({ model: ollama("llama3.2") }); // `ollama pull llama3.2` first
 */
export const ollama = (model: string, options?: { baseUrl?: string }) => new ProviderModel("ollama", model, options);
/**
 * LM Studio on this machine (or `baseUrl`), no key.
 * @param model The model name, as LM Studio shows it.
 * @param options `baseUrl`: another LM Studio server.
 */
export const lmstudio = (model: string, options?: { baseUrl?: string }) => new ProviderModel("lmstudio", model, options);
/**
 * DeepSeek (DEEPSEEK_API_KEY), e.g. deepseek("deepseek-chat").
 * @param model The model name.
 * @param options `apiKey` (else DEEPSEEK_API_KEY) and `baseUrl`.
 */
export const deepseek = (model: string, options?: ModelOptions) => new ProviderModel("deepseek", model, options);
/**
 * Mistral AI (MISTRAL_API_KEY), e.g. mistral("mistral-large-latest").
 * @param model The model name.
 * @param options `apiKey` (else MISTRAL_API_KEY) and `baseUrl`.
 */
export const mistral = (model: string, options?: ModelOptions) => new ProviderModel("mistral", model, options);
/**
 * xAI's Grok models (XAI_API_KEY).
 * @param model The model name.
 * @param options `apiKey` (else XAI_API_KEY) and `baseUrl`.
 */
export const xai = (model: string, options?: ModelOptions) => new ProviderModel("xai", model, options);
/**
 * Groq's fast inference for open models (GROQ_API_KEY).
 * @param model The model name, e.g. "llama-3.3-70b-versatile".
 * @param options `apiKey` (else GROQ_API_KEY) and `baseUrl`.
 */
export const groq = (model: string, options?: ModelOptions) => new ProviderModel("groq", model, options);
/**
 * Cerebras's fast inference for open models (CEREBRAS_API_KEY).
 * @param model The model name.
 * @param options `apiKey` (else CEREBRAS_API_KEY) and `baseUrl`.
 */
export const cerebras = (model: string, options?: ModelOptions) => new ProviderModel("cerebras", model, options);
/**
 * Together AI's open models (TOGETHER_API_KEY).
 * @param model The model name.
 * @param options `apiKey` (else TOGETHER_API_KEY) and `baseUrl`.
 */
export const together = (model: string, options?: ModelOptions) => new ProviderModel("together", model, options);
/**
 * Fireworks AI's open models (FIREWORKS_API_KEY).
 * @param model The model name.
 * @param options `apiKey` (else FIREWORKS_API_KEY) and `baseUrl`.
 */
export const fireworks = (model: string, options?: ModelOptions) => new ProviderModel("fireworks", model, options);
/**
 * Qwen on Alibaba Cloud Model Studio, international endpoint (DASHSCOPE_API_KEY).
 * China: `baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1"`.
 * @param model The model name.
 * @param options `apiKey` (else DASHSCOPE_API_KEY) and `baseUrl`.
 */
export const qwen = (model: string, options?: ModelOptions) => new ProviderModel("qwen", model, options);
/**
 * Moonshot AI's Kimi models (MOONSHOT_API_KEY). China keys: `baseUrl: "https://api.moonshot.cn/v1"`.
 * @param model The model name.
 * @param options `apiKey` (else MOONSHOT_API_KEY) and `baseUrl`.
 */
export const moonshot = (model: string, options?: ModelOptions) => new ProviderModel("moonshot", model, options);
/**
 * Z.ai's GLM models (ZAI_API_KEY). China keys: `baseUrl: "https://open.bigmodel.cn/api/paas/v4"`.
 * @param model The model name.
 * @param options `apiKey` (else ZAI_API_KEY) and `baseUrl`.
 */
export const zai = (model: string, options?: ModelOptions) => new ProviderModel("zai", model, options);
/**
 * Any OpenAI-compatible server. `allowInsecureHttp`: send the key over plain
 * `http://` to another machine (a vLLM server on your LAN); refused by
 * default, because a key sent over http is readable by anyone on the path.
 *
 * @param model The model name the server expects.
 * @param options `baseUrl` (required: the API root, usually ending in /v1), `apiKey` if the server wants one, `name` (shown as the provider; default "custom") and `allowInsecureHttp`.
 * @example
 * import { Agent, openaiCompatible } from "criogen";
 *
 * const agent = new Agent({
 *   model: openaiCompatible("meta-llama/Llama-3.3-70B-Instruct", { baseUrl: "http://localhost:8000/v1", name: "vllm" }),
 * });
 */
export const openaiCompatible = (
  model: string,
  options: ModelOptions & { baseUrl: string; name?: string; allowInsecureHttp?: boolean },
) => new ProviderModel("openai_compatible", model, options);

/**
 * Any server that speaks Anthropic's Messages API: a self-hosted gateway, or a
 * provider's Anthropic-style endpoint (DeepSeek, Z.ai, Qwen and Moonshot offer one).
 *
 * @param model The model name the server expects.
 * @param options `baseUrl` (required), `apiKey`, `name` (shown as the provider; default "custom") and `allowInsecureHttp` (as for `openaiCompatible`).
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
 *
 * Use it in tests and examples: script text answers and tool calls
 * (`FakeReply.toolCall`), and the agent loop runs for real around them.
 * Usage is counted, and replies name the model "fake".
 *
 * @example
 * import { Agent, FakeModel, FakeReply } from "criogen";
 *
 * const model = new FakeModel([FakeReply.toolCall("lookup", { orderId: "A-1" }), "Order A-1 shipped."]);
 * // A model that fails twice with rate_limited, then answers: tests retries.
 * const flaky = new FakeModel(["Done."], { failFirst: 2 });
 * console.log((await new Agent({ model: flaky }).run("Go")).text); // "Done."
 */
export class FakeModel {
  /** The scripted replies, in order. */
  readonly replies: FakeReply[];
  /** How long each reply takes, in milliseconds. */
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

  /**
   * @param replies The answers, in order: a string is a text reply; `FakeReply.toolCall(...)` and `FakeReply.toolCalls(...)` ask for tools.
   * @param options `delayMs` (default 0), `failFirst` (default 0), `byTurn` (default false) and `name`, as described on the properties.
   */
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

  /** The JSON spec the engine builds the model from. */
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
 * Options of `kaggle(...)` and `KaggleModel`.
 *
 * Kaggle's Model Proxy serves the hosted models your Kaggle account's
 * AI-model allowance pays for. Credentials, in order: `apiToken`; `proxyUrl`
 * and `proxyToken`; else MODEL_PROXY_URL and MODEL_PROXY_API_KEY,
 * KAGGLE_API_TOKEN, or the `access_token` file in KAGGLE_CONFIG_DIR or
 * ~/.kaggle. The engine mints the short-lived proxy token and renews it.
 * For prototypes and tests, not production traffic.
 */
export interface KaggleOptions {
  /** A Kaggle API token; the engine mints the proxy token from it. Else the environment (see above). */
  apiToken?: string;
  /** The proxy's address, with `proxyToken` (a proxy token you already have). */
  proxyUrl?: string;
  /** A proxy token minted elsewhere, with `proxyUrl`. It is not renewed. */
  proxyToken?: string;
  /** Where proxy tokens are minted (tests). */
  apiBase?: string;
}

/**
 * A model on Kaggle's Model Proxy. `kaggle(model, options)` and the string
 * "kaggle:<model>" make the same thing; see `KaggleOptions` for credentials.
 * The token is not a property, and `toJSON()` leaves it out.
 *
 * @example
 * import { Agent, kaggle } from "criogen";
 *
 * // KAGGLE_API_TOKEN set, or ~/.kaggle/access_token present
 * const agent = new Agent({ model: kaggle("google/gemini-3.6-flash") });
 */
export class KaggleModel {
  /** Always "kaggle". */
  readonly kind = "kaggle";
  /** The model name on the proxy, e.g. "google/gemini-3.6-flash". */
  readonly model: string;
  readonly #options: KaggleOptions;

  /**
   * @param model The model name on the proxy.
   * @param options Credentials; without them, the environment.
   */
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

  /** `{ kind, model }`, without any token: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

/**
 * A model on Kaggle's Model Proxy, e.g. kaggle("google/gemini-3.6-flash").
 * @param model The model name on the proxy.
 * @param options Credentials (see `KaggleOptions`); without them, the environment.
 */
export const kaggle =(model: string, options?: KaggleOptions) => new KaggleModel(model, options);

/**
 * Options of `azure(...)` and `AzureModel`: the endpoint and credentials.
 *
 * For Azure OpenAI or Microsoft Foundry. `endpoint` is the
 * resource name or URL (else AZURE_OPENAI_ENDPOINT). Credentials, in order:
 * `apiKey`; `token` (a bearer token from elsewhere); `clientSecret` with
 * `tenantId` and `clientId` (a service principal); `managedIdentity` (with
 * `clientId` for a user-assigned identity); else AZURE_OPENAI_API_KEY, or
 * AZURE_TENANT_ID + AZURE_CLIENT_ID + AZURE_CLIENT_SECRET, or
 * AZURE_USE_MANAGED_IDENTITY=1. The engine obtains and renews Entra ID
 * tokens. `api: "chat"` for Foundry's models from other makers.
 */
export interface AzureOptions {
  /** The resource name ("my-resource") or its URL. Else AZURE_OPENAI_ENDPOINT. */
  endpoint?: string;
  /** An Azure OpenAI API key. Else AZURE_OPENAI_API_KEY, unless another credential is given. */
  apiKey?: string;
  /** A bearer token you obtained yourself (it is not renewed). */
  token?: string;
  /** The Entra ID tenant, with `clientId` and `clientSecret`. Else AZURE_TENANT_ID. */
  tenantId?: string;
  /** The service principal's application id; with `managedIdentity`, a user-assigned identity. Else AZURE_CLIENT_ID (for a client secret). */
  clientId?: string;
  /** A service principal's secret, with `tenantId` and `clientId`. */
  clientSecret?: string;
  /** `true`: use the managed identity of the Azure host the code runs on. */
  managedIdentity?: boolean;
  /** "responses" (OpenAI's models, the default) or "chat" (Chat Completions, for Foundry's models from other makers). */
  api?: "responses" | "chat";
  /**
   * The scope Entra ID tokens are requested for. Foundry also accepts "https://ai.azure.com/.default".
   * @default "https://cognitiveservices.azure.com/.default"
   */
  scope?: string;
  /** Where Entra ID tokens are issued (sovereign clouds; tests). */
  authority?: string;
  /** Another managed-identity endpoint (tests). */
  identityEndpoint?: string;
}

/**
 * A deployment on Azure OpenAI or Microsoft Foundry. `azure(model, options)`
 * and the string "azure:<deployment>" make the same thing; see `AzureOptions`
 * for the endpoint and credentials. Secrets are not properties, and
 * `toJSON()` leaves them out.
 *
 * Needs the 0.1.6 engine for your platform; with an older one, using the
 * model (building an agent with it, a gateway call) throws an error with
 * code `unsupported`.
 *
 * @example
 * import { Agent, AzureModel } from "criogen";
 *
 * // A service principal; the engine fetches and renews Entra ID tokens.
 * const model = new AzureModel("gpt-5-mini", {
 *   endpoint: "my-resource",
 *   tenantId: process.env.AZURE_TENANT_ID,
 *   clientId: process.env.AZURE_CLIENT_ID,
 *   clientSecret: process.env.AZURE_CLIENT_SECRET,
 * });
 * const agent = new Agent({ model });
 */
export class AzureModel {
  /** Always "azure". */
  readonly kind = "azure";
  /** The deployment name. */
  readonly model: string;
  readonly #options: AzureOptions;

  /**
   * @param model The deployment name.
   * @param options The endpoint, credentials and API (see `AzureOptions`); without them, the environment.
   * @throws {TypeError} `api` is not "responses" or "chat".
   */
  constructor(model: string, options: AzureOptions = {}) {
    if (options.api !== undefined && options.api !== "responses" && options.api !== "chat") {
      throw new TypeError('api must be "responses" or "chat"');
    }
    this.model = model;
    this.#options = options;
  }

  /** The JSON spec (with any secret; never logged). */
  spec(): Record<string, unknown> {
    requireEngine016("Azure OpenAI");
    return { kind: this.kind, model: this.model, ...this.#options };
  }

  /** `{ kind, model }`, without any secret: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

/**
 * A deployment on Azure OpenAI or Foundry, e.g. azure("gpt-5-mini", { endpoint: "my-resource" }).
 * @param model The deployment name.
 * @param options The endpoint, credentials and API (see `AzureOptions`); without them, the environment.
 * @example
 * import { Agent, azure } from "criogen";
 *
 * // AZURE_OPENAI_API_KEY set; a Foundry model from another maker goes over Chat Completions.
 * const agent = new Agent({ model: azure("my-llama-deployment", { endpoint: "my-resource", api: "chat" }) });
 */
export const azure =(model: string, options?: AzureOptions) => new AzureModel(model, options);

/**
 * Options of `bedrock(...)` and `BedrockModel`: region, key and API.
 *
 * Amazon Bedrock with an API key. `region` (else AWS_REGION
 * or AWS_DEFAULT_REGION; there is no default, so data never goes to a region
 * you did not choose); `apiKey` (else AWS_BEARER_TOKEN_BEDROCK). Claude is
 * called over the Anthropic Messages API and other makers' models over Chat
 * Completions; `api` forces one. `baseUrl` reaches bedrock-mantle or a VPC
 * endpoint. IAM roles (SigV4) are not supported yet.
 */
export interface BedrockOptions {
  /** The AWS region, e.g. "us-east-1". Else AWS_REGION or AWS_DEFAULT_REGION; there is no default. */
  region?: string;
  /** A Bedrock API key. Else AWS_BEARER_TOKEN_BEDROCK. */
  apiKey?: string;
  /**
   * Which API to call: "auto" (Anthropic's models over the Messages API,
   * the rest over Chat Completions), "messages", "chat" or "responses" (for
   * models whose model card lists the Responses API).
   * @default "auto"
   */
  api?: "auto" | "messages" | "chat" | "responses";
  /** Another endpoint: bedrock-mantle or a VPC endpoint. */
  baseUrl?: string;
}

/**
 * A model on Amazon Bedrock. `bedrock(model, options)` and the string
 * "bedrock:<model id>" make the same thing; see `BedrockOptions`. The key
 * is not a property, and `toJSON()` leaves it out.
 *
 * Needs the 0.1.6 engine for your platform; with an older one, using the
 * model throws an error with code `unsupported`.
 */
export class BedrockModel {
  /** Always "bedrock". */
  readonly kind = "bedrock";
  /** The Bedrock model id or inference profile. */
  readonly model: string;
  readonly #options: BedrockOptions;

  /**
   * @param model The Bedrock model id or inference profile, e.g. "us.anthropic.claude-sonnet-5".
   * @param options Region, key, API and endpoint; without them, the environment.
   * @throws {TypeError} `api` is not "auto", "messages", "chat" or "responses".
   */
  constructor(model: string, options: BedrockOptions = {}) {
    if (options.api !== undefined && !["auto", "messages", "chat", "responses"].includes(options.api)) {
      throw new TypeError('api must be "auto", "messages", "chat" or "responses"');
    }
    this.model = model;
    this.#options = options;
  }

  /** The JSON spec (with any key; never logged). */
  spec(): Record<string, unknown> {
    requireEngine016("Amazon Bedrock");
    return { kind: this.kind, model: this.model, ...this.#options };
  }

  /** `{ kind, model }`, without the key: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

/**
 * A model on Amazon Bedrock, e.g. bedrock("us.anthropic.claude-sonnet-5", { region: "us-east-1" }).
 * @param model The Bedrock model id or inference profile.
 * @param options Region, key, API and endpoint (see `BedrockOptions`); without them, the environment.
 * @example
 * import { Agent, bedrock } from "criogen";
 *
 * // AWS_BEARER_TOKEN_BEDROCK set; the region is always chosen explicitly.
 * const agent = new Agent({ model: bedrock("us.anthropic.claude-sonnet-5", { region: "us-east-1" }) });
 */
export const bedrock =(model: string, options?: BedrockOptions) => new BedrockModel(model, options);

/**
 * Options of `vertex(...)` and `VertexModel`: project, location and credentials.
 *
 * Google Vertex AI. `project` (else GOOGLE_CLOUD_PROJECT) and
 * `location` (else GOOGLE_CLOUD_LOCATION; no default, it decides where data is
 * processed). Credentials: `token`, `credentialsFile`, else
 * GOOGLE_APPLICATION_CREDENTIALS, gcloud's application-default file, or the
 * metadata server on Google Cloud. Service-account key files are not supported
 * yet. `claude-…` models go to Claude's endpoint, the rest to the
 * OpenAI-compatible one; `api` forces one.
 */
export interface VertexOptions {
  /** The Google Cloud project id. Else GOOGLE_CLOUD_PROJECT. */
  project?: string;
  /** The region, e.g. "us-central1" or "global"; it decides where data is processed. Else GOOGLE_CLOUD_LOCATION; there is no default. */
  location?: string;
  /** An OAuth access token you obtained yourself. */
  token?: string;
  /** A credentials file of type `authorized_user` (as `gcloud auth application-default login` writes); service-account key files are refused. Else GOOGLE_APPLICATION_CREDENTIALS, gcloud's file, or the metadata server. */
  credentialsFile?: string;
  /**
   * Which endpoint to call: "auto" (`claude-…` models to Claude's endpoint,
   * the rest to the OpenAI-compatible one), "chat" (OpenAI-compatible Chat
   * Completions) or "claude" (Anthropic Messages).
   * @default "auto"
   */
  api?: "auto" | "chat" | "claude";
  /** Another API host (a private endpoint; tests). */
  host?: string;
  /** Token endpoints elsewhere (tests). */
  tokenUrl?: string;
  /** Another metadata server address (tests). */
  metadataUrl?: string;
}

/**
 * A model on Google Vertex AI. `vertex(model, options)` and the string
 * "vertex:<model>" make the same thing; see `VertexOptions`. Tokens are not
 * properties, and `toJSON()` leaves them out.
 *
 * Needs the 0.1.6 engine for your platform; with an older one, using the
 * model throws an error with code `unsupported`.
 */
export class VertexModel {
  /** Always "vertex". */
  readonly kind = "vertex";
  /** The model name, e.g. "google/gemini-3.6-flash" or a `claude-…` model. */
  readonly model: string;
  readonly #options: VertexOptions;

  /**
   * @param model The model name.
   * @param options Project, location, credentials and API; without them, the environment.
   * @throws {TypeError} `api` is not "auto", "chat" or "claude".
   */
  constructor(model: string, options: VertexOptions = {}) {
    if (options.api !== undefined && !["auto", "chat", "claude"].includes(options.api)) {
      throw new TypeError('api must be "auto", "chat" or "claude"');
    }
    this.model = model;
    this.#options = options;
  }

  /** The JSON spec (with any token; never logged). */
  spec(): Record<string, unknown> {
    requireEngine016("Google Vertex AI");
    return { kind: this.kind, model: this.model, ...this.#options };
  }

  /** `{ kind, model }`, without any token: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: this.kind, model: this.model };
  }
}

/**
 * A model on Google Vertex AI, e.g. vertex("google/gemini-3.6-flash", { project: "p", location: "global" }).
 * @param model The model name.
 * @param options Project, location, credentials and API (see `VertexOptions`); without them, the environment.
 * @example
 * import { Agent, vertex } from "criogen";
 *
 * // Credentials from GOOGLE_APPLICATION_CREDENTIALS, gcloud, or the metadata server.
 * const agent = new Agent({ model: vertex("google/gemini-3.6-flash", { project: "my-project", location: "global" }) });
 */
export const vertex =(model: string, options?: VertexOptions) => new VertexModel(model, options);


/**
 * What a model function receives: the request the engine would send to a provider.
 *
 * It has the messages so far, the tools the model may call, the settings
 * and any output schema (`ModelRequest` in schemas/criogen.schema.json).
 */
export interface ModelCall {
  /** The conversation so far, system message first, in the transcript format of `result.messages`. */
  messages: Record<string, unknown>[];
  /** The tools the model may call, each with `name`, `description` and `input_schema`. */
  tools?: Record<string, unknown>[];
  /** Generation settings, such as temperature and the token limit, when set. */
  settings?: Record<string, unknown>;
  /**
   * A structured-output request (`{ schema, strategy? }`: the JSON Schema
   * the answer must match), when there is one. An agent's `output` schema
   * reaches a function model as a `final_output` tool in `tools` instead.
   */
  output?: Record<string, unknown> | null;
  [key: string]: unknown;
}

/**
 * A model function's answer.
 *
 * Text, tool calls (`FakeReply.toolCall`), an exact assistant message, or a
 * classified error (`{ error: { kind: "rate_limited", message } }` is
 * retried as a provider's would be).
 */
export type ModelAnswer = string | FakeReply | { message: Record<string, unknown> } | { error: { kind: string; message: string } };

/**
 * Models written in JavaScript need an engine from 0.1.4 on. The engine
 * package for this system may be older (a release can ship before every
 * platform's engine is rebuilt): say so instead of failing obscurely.
 */
export function requireFunctions(what: string): void {
  if (typeof (native as { registerFunction?: unknown }).registerFunction !== "function") {
    throw errorFor(
      "unsupported",
      `${what} needs criogen's engine 0.1.4 or newer, and the engine package installed for this system is older: ` +
        "it comes with the next release for this platform (Windows and the browser, criogen/web, have it now)",
    );
  }
}

/**
 * Throws `unsupported` when the installed engine predates 0.1.5 (collections,
 * evals' schema scorer): `validateJson` arrived with it. Linux and macOS
 * engine packages stay older until they can be built again.
 */
export function requireEngine015(what: string): void {
  if (typeof (native as { validateJson?: unknown }).validateJson !== "function") {
    throw errorFor(
      "unsupported",
      `${what} needs criogen's engine 0.1.5 or newer, and the engine package installed for this system is older: ` +
        "it comes with the next release for this platform (Windows and the browser, criogen/web, have it now)",
    );
  }
}

/**
 * Throws `unsupported` when the installed engine predates 0.1.6 (Azure,
 * Bedrock, Vertex AI): `engineVersion` arrived with it. Linux and macOS engine
 * packages stay older until they can be built again.
 */
export function requireEngine016(what: string): void {
  if (typeof (native as { engineVersion?: unknown }).engineVersion !== "function") {
    throw errorFor(
      "unsupported",
      `${what} needs criogen's engine 0.1.6 or newer, and the engine package installed for this system is older: ` +
        "it comes with the next release for this platform (Windows and the browser, criogen/web, have it now)",
    );
  }
}

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
 *
 * Needs the 0.1.4 engine or newer for your platform; with an older one,
 * using the model throws an error with code `unsupported`.
 *
 * @example
 * import { Agent, FakeReply, FunctionModel } from "criogen";
 *
 * // Ask for the weather tool once, then answer with what it returned.
 * const model = new FunctionModel((call) => {
 *   const last = call.messages[call.messages.length - 1];
 *   if (last.role === "user") return FakeReply.toolCall("get_weather", { city: "Paris" });
 *   return `The tool said: ${JSON.stringify(last.content)}`;
 * });
 * const agent = new Agent({ model });
 */
export class FunctionModel {
  /** Always "function". */
  readonly kind = "function";
  /** The model name in replies and traces. */
  readonly name: string;
  readonly #respond: (call: ModelCall) => ModelAnswer | Promise<ModelAnswer>;
  #handle: number | undefined;

  /**
   * @param respond Called for every model call with the request; returns (or resolves to) the answer: text, `FakeReply.toolCall(...)`, `{ message }` or `{ error: { kind, message } }`.
   * @param options `name`: the model name in replies and traces (default "function").
   * @throws {TypeError} `respond` is not a function.
   */
  constructor(respond: (call: ModelCall) => ModelAnswer | Promise<ModelAnswer>, options: { name?: string } = {}) {
    if (typeof respond !== "function") throw new TypeError("FunctionModel needs a function: (call) => answer");
    this.#respond = respond;
    this.name = options.name ?? "function";
  }

  /** The JSON spec: the function is registered with the engine on first use. */
  spec(): Record<string, unknown> {
    if (this.#handle === undefined) {
      requireFunctions("FunctionModel");
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

  /** `{ kind: "function", name }`: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: "function", name: this.name };
  }
}

/**
 * Anything an agent or a gateway accepts as a model: a "provider:model"
 * string or a model object.
 *
 * The string's provider is one of openai, gemini, anthropic, openrouter,
 * deepseek, mistral, xai, groq, cerebras, together, fireworks, qwen,
 * moonshot, zai (keys from their environment variables), ollama, lmstudio
 * (no key), kaggle, azure, bedrock or vertex (credentials from the
 * environment). Use the functions (`openai(...)`, `azure(...)`, ...) to pass
 * keys or options in code.
 */
export type Model =string | ProviderModel | KaggleModel | AzureModel | BedrockModel | VertexModel | FakeModel | FunctionModel;

/** The JSON spec of a model object or "provider:model" string. */
export function modelSpec(model: Model): Record<string, unknown> {
  if (typeof model === "string") {
    const at = model.indexOf(":");
    const provider = at > 0 ? model.slice(0, at) : "";
    const known = [...Object.keys(KEYS), ...LOCAL, "kaggle", "azure", "bedrock", "vertex"];
    if (!known.includes(provider)) {
      throw new TypeError(`model "${model}": use "provider:model" with provider one of ${known.join(", ")}`);
    }
    if (provider === "kaggle") return new KaggleModel(model.slice(at + 1)).spec();
    if (provider === "azure") return new AzureModel(model.slice(at + 1)).spec();
    if (provider === "bedrock") return new BedrockModel(model.slice(at + 1)).spec();
    if (provider === "vertex") return new VertexModel(model.slice(at + 1)).spec();
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

  /**
   * Use the static methods (`Embedder.openai()`, ...).
   * @param spec The engine spec.
   */
  private constructor(spec: Record<string, unknown>) {
    this.spec = spec;
  }

  private static provider(kind: string, model: string, options: ModelOptions & { dimensions?: number } = {}): Embedder {
    const apiKey = options.apiKey ?? (KEYS[kind] ? envVar(KEYS[kind]) : undefined);
    return new Embedder({ kind, model, apiKey, baseUrl: options.baseUrl, dimensions: options.dimensions });
  }

  /**
   * OpenAI's /embeddings (key from OPENAI_API_KEY).
   * @param model The embedding model.
   * @param options `apiKey` (else OPENAI_API_KEY) and `dimensions` (shorter vectors, for models that support it).
   * @example
   * import { Embedder, Gateway } from "criogen";
   *
   * const vectors = await new Gateway().embed(Embedder.openai(), ["refund policy", "shipping times"]);
   */
  static openai(model = "text-embedding-3-small", options: { apiKey?: string; dimensions?: number } = {}): Embedder {
    return Embedder.provider("openai", model, options);
  }

  /**
   * Gemini's OpenAI-compatible /embeddings (GEMINI_API_KEY).
   * @param model The embedding model.
   * @param options `apiKey` (else GEMINI_API_KEY).
   */
  static gemini(model = "gemini-embedding-001", options: { apiKey?: string } = {}): Embedder {
    return Embedder.provider("gemini", model, options);
  }

  /**
   * Ollama on this machine (or `baseUrl`): nothing leaves your network.
   * @param model The embedding model (`ollama pull` it first).
   * @param options `baseUrl`: another Ollama server.
   */
  static ollama(model = "nomic-embed-text", options: { baseUrl?: string } = {}): Embedder {
    return Embedder.provider("ollama", model, options);
  }

  /**
   * Any OpenAI-compatible /embeddings (vLLM, a gateway).
   * @param model The embedding model the server expects.
   * @param options `baseUrl` (required), `apiKey` and `dimensions`.
   */
  static openaiCompatible(model: string, options: { baseUrl: string; apiKey?: string; dimensions?: number }): Embedder {
    const embedder = Embedder.provider("openai_compatible", model, options);
    embedder.spec.name = "custom";
    return embedder;
  }

  /**
   * For tests, never a network call: texts sharing words are close;
   * `synonyms` makes different words count as the same (`{ car: "automobile" }`);
   * `failing` makes every call fail (search then falls back to keywords).
   * @param options `synonyms` (word to the word it counts as) and `failing` (default false).
   */
  static fake(options: { synonyms?: Record<string, string>; failing?: boolean } = {}): Embedder {
    return new Embedder({ kind: "fake", synonyms: Object.entries(options.synonyms ?? {}), failing: options.failing ?? false });
  }

  /** `{ kind, model }`, without the key: what `JSON.stringify` and logs show. */
  toJSON(): Record<string, unknown> {
    return { kind: this.spec.kind, model: this.spec.model };
  }
}

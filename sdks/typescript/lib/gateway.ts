// Calling a model without an agent (design 0018): `Gateway`. Retries and
// fallback models work as for an agent, in the Rust engine.

import { Retry, cancellable } from "./agent";
import { translate } from "./errors";
import { Embedder, Model, modelSpec, requireFunctions } from "./models";
import { native, NativeChatStream, NativeGateway } from "./native";

/** Text, or messages (the transcript format of `result.messages`; `content` may be a string). */
export type ChatInput = string | Record<string, unknown>[];

/** Options of `gateway.chat` and `gateway.stream`. */
export interface ChatOptions {
  /** The system message. */
  instructions?: string;
  /** Sampling temperature, passed to the provider (the provider's default when left out). */
  temperature?: number;
  /** The most tokens the answer may have (the provider's default when left out). */
  maxTokens?: number;
  /** A JSON Schema the answer must match; `reply.output` is then the parsed JSON. */
  output?: Record<string, unknown>;
  /**
   * Tools the model may call (tool specs, as a FunctionModel's `call.tools`):
   * the reply's `message` then holds its tool calls, for you to run. With
   * this, a FunctionModel can pass an agent's request on to a real model.
   */
  tools?: Record<string, unknown>[];
  /** Aborting cancels the call (the HTTP request stops). */
  signal?: AbortSignal;
}

/** A model's answer. */
export interface ChatReply<Output = unknown> {
  /** All text of the answer. */
  text: string;
  /** The assistant message. */
  message: Record<string, unknown>;
  /** Tokens of this call: input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens. */
  usage: Record<string, number>;
  /** Why the model stopped, e.g. "stop" or "length". */
  stopReason: string | null;
  /** "provider:model" that answered (a fallback, if the first failed). */
  model: string;
  /** The parsed JSON answer, with `output`. */
  output: Output | null;
}

/** A piece of a streamed answer. `restart`: a retry started the answer over, so drop the text shown so far. */
export interface Chunk {
  /** "text": more of the answer; "restart": a retry started the answer over. */
  type: "text" | "restart";
  /** The new text ("" for "restart"). */
  text: string;
}

/** Options of `new Gateway(...)`: what happens when a model call fails. They apply to every call. */
export interface GatewayOptions {
  /** Models tried in order when the one asked for fails with a transient error (after its retries). */
  fallbacks?: Model[];
  /** Other error kinds that also move on, e.g. "context_overflow". */
  fallbackOn?: string[];
  /** How transient errors are retried (default: 3 attempts, backoff from 0.5 s). */
  retry?: Retry;
  /** The longest Retry-After that is waited for, in seconds (default 60). */
  maxRetryAfterSecs?: number;
}

/** A streamed answer: `for await (const chunk of stream)`, then `await stream.reply()`. Leaving the loop early cancels the call. */
export class ChatStream<Output = unknown> implements AsyncIterable<Chunk> {
  private done = false;

  /**
   * @internal
   * @param nativeStream The engine's stream.
   */
  constructor(private readonly nativeStream: NativeChatStream) {}

  async *[Symbol.asyncIterator](): AsyncIterator<Chunk> {
    try {
      for (;;) {
        const data = await this.nativeStream.next();
        if (data === null) {
          this.done = true;
          return;
        }
        yield JSON.parse(data) as Chunk;
      }
    } finally {
      if (!this.done) this.nativeStream.cancel();
    }
  }

  /**
   * Waits for the call; the whole answer.
   * @throws {ModelError} The call failed after retries and fallbacks.
   * @throws {CancelledError} The stream was cancelled.
   */
  async reply(): Promise<ChatReply<Output>> {
    try {
      return JSON.parse(await this.nativeStream.reply());
    } catch (error) {
      throw translate(error);
    }
  }

  /** Cancels the call. */
  cancel(): void {
    this.nativeStream.cancel();
  }
}

/**
 * Model calls without an agent: `chat`, `stream` and `embed`, with the agent's retries and fallback models.
 *
 * Use it for one-shot calls (classify, summarize, extract JSON) where you do
 * not need tools run for you or a saved conversation. One gateway can call
 * any model; each call names its model, and the provider clients are reused.
 *
 * @example
 * import { FakeModel, Gateway } from "criogen";
 *
 * const gateway = new Gateway({ retry: { maxAttempts: 2 } });
 * const reply = await gateway.chat(new FakeModel(["pong"]), "ping"); // or "openai:gpt-5-mini"
 * console.log(reply.text, reply.model); // "pong" "fake:fake"
 */
export class Gateway {
  /** @internal */
  readonly native: NativeGateway;

  /**
   * @param options Retries and fallback models for every call (see `GatewayOptions`).
   * @throws {ValidationError} An option the engine does not accept (e.g. an unknown `fallbackOn` kind).
   */
  constructor(options: GatewayOptions = {}) {
    const config = {
      fallbacks: (options.fallbacks ?? []).map(modelSpec),
      fallbackOn: options.fallbackOn,
      retry: options.retry,
      maxRetryAfterSecs: options.maxRetryAfterSecs,
    };
    try {
      this.native = new native.NativeGateway(JSON.stringify(config));
    } catch (error) {
      throw translate(error);
    }
  }

  private static payload(model: Model, input: ChatInput, options: ChatOptions): string {
    const { instructions, temperature, maxTokens, output, tools } = options;
    // An older engine would ignore `tools` without a word.
    if (tools?.length) requireFunctions("Gateway tools");
    return JSON.stringify({ model: modelSpec(model), input, instructions, temperature, maxTokens, output, tools });
  }

  /**
   * One answer.
   *
   * @param model The model to call: a "provider:model" string or a model object. The gateway's fallbacks follow it.
   * @param input Text (one user message) or a list of messages.
   * @param options Instructions, temperature, token limit, an `output` schema, tools and an abort `signal`.
   * @throws {ModelError} The call failed after retries and fallbacks.
   * @throws {CriogenError} With `output`: the answer is not JSON matching the schema (code `output_invalid`).
   * @throws {CancelledError} `signal` was aborted.
   * @example
   * import { Gateway } from "criogen";
   *
   * const gateway = new Gateway();
   * const reply = await gateway.chat<{ label: string }>("openai:gpt-5-mini", "The package arrived broken.", {
   *   instructions: "Classify the customer message.",
   *   output: { type: "object", properties: { label: { type: "string", enum: ["complaint", "question", "praise"] } }, required: ["label"] },
   * });
   * console.log(reply.output?.label);
   */
  async chat<Output = unknown>(model: Model, input: ChatInput, options: ChatOptions = {}): Promise<ChatReply<Output>> {
    const payload = Gateway.payload(model, input, options);
    try {
      return JSON.parse(await cancellable(options.signal, (id) => this.native.chat(payload, id)));
    } catch (error) {
      throw translate(error);
    }
  }

  /**
   * The answer as it is written: `for await (const chunk of gw.stream(…))`.
   *
   * On a "restart" chunk, drop the text shown so far: a retry started the
   * answer over.
   *
   * @param model The model to call: a "provider:model" string or a model object.
   * @param input Text (one user message) or a list of messages.
   * @param options As for `chat`; aborting `signal` cancels the stream.
   * @example
   * import { FakeModel, Gateway } from "criogen";
   *
   * const stream = new Gateway().stream(new FakeModel(["Hello there."]), "Say hello.");
   * let shown = "";
   * for await (const chunk of stream) shown = chunk.type === "restart" ? "" : shown + chunk.text;
   * const reply = await stream.reply(); // usage, stopReason, the full message
   */
  stream<Output = unknown>(model: Model, input: ChatInput, options: ChatOptions = {}): ChatStream<Output> {
    const payload = Gateway.payload(model, input, options);
    let nativeStream: NativeChatStream;
    try {
      nativeStream = this.native.stream(payload);
    } catch (error) {
      throw translate(error);
    }
    const stream = new ChatStream<Output>(nativeStream);
    options.signal?.addEventListener("abort", () => stream.cancel(), { once: true });
    if (options.signal?.aborted) stream.cancel();
    return stream;
  }

  /**
   * One vector per text, in order.
   *
   * The gateway's retries and fallbacks do not apply to embeddings.
   *
   * @param embedder Which embedding model to call (`Embedder.openai()`, `Embedder.ollama()`, ...).
   * @param texts The texts to embed.
   * @param options `signal` cancels the call.
   * @throws {ModelError} The provider call failed.
   * @example
   * import { Embedder, Gateway } from "criogen";
   *
   * const vectors = await new Gateway().embed(Embedder.fake(), ["red car", "blue car"]); // or Embedder.openai()
   * console.log(vectors.length, vectors[0].length);
   */
  async embed(embedder: Embedder, texts: string[], options: { signal?: AbortSignal } = {}): Promise<number[][]> {
    const payload = JSON.stringify({ embedder: embedder.spec, texts });
    try {
      return JSON.parse(await cancellable(options.signal, (id) => this.native.embed(payload, id)));
    } catch (error) {
      throw translate(error);
    }
  }
}

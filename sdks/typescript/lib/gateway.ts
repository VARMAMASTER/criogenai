// Calling a model without an agent (design 0018): `Gateway`. Retries and
// fallback models work as for an agent, in the Rust engine.

import { Retry, cancellable } from "./agent";
import { translate } from "./errors";
import { Embedder, Model, modelSpec } from "./models";
import { native, NativeChatStream, NativeGateway } from "./native";

/** Text, or messages (the transcript format of `result.messages`; `content` may be a string). */
export type ChatInput = string | Record<string, unknown>[];

export interface ChatOptions {
  /** The system message. */
  instructions?: string;
  temperature?: number;
  maxTokens?: number;
  /** A JSON Schema the answer must match; `reply.output` is then the parsed JSON. */
  output?: Record<string, unknown>;
  /** Aborting cancels the call (the HTTP request stops). */
  signal?: AbortSignal;
}

/** A model's answer. */
export interface ChatReply<Output = unknown> {
  /** All text of the answer. */
  text: string;
  /** The assistant message. */
  message: Record<string, unknown>;
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
  type: "text" | "restart";
  text: string;
}

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

  /** @internal */
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

  /** Waits for the call; the whole answer. */
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

/** Model calls without an agent: `chat`, `stream` and `embed`, with the agent's retries and fallback models. */
export class Gateway {
  /** @internal */
  readonly native: NativeGateway;

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
    const { instructions, temperature, maxTokens, output } = options;
    return JSON.stringify({ model: modelSpec(model), input, instructions, temperature, maxTokens, output });
  }

  /** One answer. */
  async chat<Output = unknown>(model: Model, input: ChatInput, options: ChatOptions = {}): Promise<ChatReply<Output>> {
    const payload = Gateway.payload(model, input, options);
    try {
      return JSON.parse(await cancellable(options.signal, (id) => this.native.chat(payload, id)));
    } catch (error) {
      throw translate(error);
    }
  }

  /** The answer as it is written: `for await (const chunk of gw.stream(…))`. */
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

  /** One vector per text, in order. */
  async embed(embedder: Embedder, texts: string[], options: { signal?: AbortSignal } = {}): Promise<number[][]> {
    const payload = JSON.stringify({ embedder: embedder.spec, texts });
    try {
      return JSON.parse(await cancellable(options.signal, (id) => this.native.embed(payload, id)));
    } catch (error) {
      throw translate(error);
    }
  }
}

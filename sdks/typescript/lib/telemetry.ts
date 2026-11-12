// Tracing (design 0017 §4): every run of an agent exported as an
// OpenTelemetry trace, to Langfuse or any OTLP/HTTP collector. Message content
// is left out unless `captureContent: true`.

import { translate } from "./errors";
import { native, type NativeTelemetry } from "./native";

/** Settings every exporter takes. */
export interface TelemetryOptions {
  /** Send message text and tool arguments too (default false). */
  captureContent?: boolean;
  /**
   * The `service.name` of the traces.
   * @default "criogenai"
   */
  serviceName?: string;
}

/**
 * An exporter shared by any number of agents: `new Agent({ …, telemetry })`.
 * Exports in the background; call `flush()` before a script ends, and
 * `shutdown()` when done.
 *
 * Each run becomes one trace, with spans for steps, model calls and tool
 * calls. Without `captureContent` the trace holds names, timings, token
 * counts and error codes, not prompts or outputs.
 *
 * @example
 * import { Agent, FakeModel, Telemetry } from "criogenai";
 *
 * // Sends to the local trace viewer: start it first with `npx criogenai trace`.
 * const telemetry = Telemetry.local();
 * const agent = new Agent({ model: new FakeModel(["Hello."]), telemetry });
 * await agent.run("Hi");
 * await telemetry.flush();
 * console.log(telemetry.stats()); // e.g. { exported: 3, failed: 0, dropped: 0, lastError: null }
 */
export class Telemetry {
  /** @internal */
  readonly native: NativeTelemetry;

  /**
   * @internal
   * @param spec The exporter's settings, as the engine reads them.
   */
  private constructor(spec: Record<string, unknown>) {
    try {
      this.native = new native.NativeTelemetry(JSON.stringify(spec));
    } catch (error) {
      throw translate(error);
    }
  }

  /**
   * Langfuse (cloud or self-hosted). Keys default to LANGFUSE_PUBLIC_KEY /
   * LANGFUSE_SECRET_KEY and the address to LANGFUSE_BASE_URL.
   *
   * @param options `publicKey` and `secretKey` (pass both, or neither to read
   * both from the environment), `baseUrl` (when given it is always used; else
   * `LANGFUSE_BASE_URL` or `LANGFUSE_HOST`, else `https://cloud.langfuse.com`),
   * plus `captureContent` and `serviceName`.
   * @throws {ValidationError} Only one key was passed, or none and the environment variables are not set (`invalid_value`).
   *
   * @example
   * import { Agent, Telemetry } from "criogenai";
   *
   * // Reads LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY and LANGFUSE_BASE_URL.
   * const telemetry = Telemetry.langfuse({ serviceName: "support-bot" });
   * const agent = new Agent({ model: "openai:gpt-5-mini", telemetry });
   * await agent.run("Hello");
   * await telemetry.shutdown();
   */
  static langfuse(options: TelemetryOptions & { publicKey?: string; secretKey?: string; baseUrl?: string } = {}): Telemetry {
    return new Telemetry({ kind: "langfuse", ...options, captureContent: options.captureContent ?? false });
  }

  /**
   * Any OTLP/HTTP (JSON) collector, e.g. "http://localhost:4318/v1/traces".
   *
   * @param endpoint The collector's traces URL, including its path (usually `/v1/traces`).
   * @param options `headers` sent with every export (e.g. an API key), plus `captureContent` and `serviceName`.
   *
   * @example
   * import { Telemetry } from "criogenai";
   *
   * const telemetry = Telemetry.otlp("https://otel.example.com/v1/traces", {
   *   headers: { authorization: `Bearer ${process.env.OTEL_TOKEN}` },
   *   serviceName: "support-bot",
   * });
   */
  static otlp(endpoint: string, options: TelemetryOptions & { headers?: Record<string, string> } = {}): Telemetry {
    return new Telemetry({ kind: "otlp", endpoint, ...options, headers: options.headers ?? {}, captureContent: options.captureContent ?? false });
  }

  /**
   * The local trace viewer (`npx criogenai trace`, `serveTraces()`, design 0023).
   * Content only with `captureContent: true`; it stays on this computer either way.
   *
   * @param options `url`: the viewer's address (default `http://127.0.0.1:4318`; `/v1/traces` is added), plus `captureContent` and `serviceName`.
   */
  static local(options: TelemetryOptions & { url?: string } = {}): Telemetry {
    const { url, ...rest } = options;
    return Telemetry.otlp(`${(url ?? "http://127.0.0.1:4318").replace(/\/+$/, "")}/v1/traces`, rest);
  }

  /** Waits until everything sent so far is exported. */
  flush(): Promise<void> {
    return this.native.flush();
  }

  /** Exports what is left and stops. */
  shutdown(): Promise<void> {
    return this.native.shutdown();
  }

  /**
   * Exported, failed and dropped counts, and the last error.
   *
   * @returns `exported`: spans the backend accepted. `failed`: spans lost after failed export attempts. `dropped`: events dropped because the queue was full (a run is never blocked). `lastError`: the last export error, or `null`.
   */
  stats(): { exported: number; failed: number; dropped: number; lastError: string | null } {
    return JSON.parse(this.native.stats());
  }

  /** `{ telemetry: true }`: the exporter's settings (keys, headers) never appear in JSON output. */
  toJSON(): Record<string, unknown> {
    return { telemetry: true };
  }
}

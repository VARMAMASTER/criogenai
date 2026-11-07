// Tracing (design 0017 §4): every run of an agent exported as an
// OpenTelemetry trace, to Langfuse or any OTLP/HTTP collector. Message content
// is left out unless `captureContent: true`.

import { translate } from "./errors";
import { native, type NativeTelemetry } from "./native";

export interface TelemetryOptions {
  /** Send message text and tool arguments too (default false). */
  captureContent?: boolean;
  /** The `service.name` of the traces. */
  serviceName?: string;
}

/**
 * An exporter shared by any number of agents: `new Agent({ …, telemetry })`.
 * Exports in the background; call `flush()` before a script ends, and
 * `shutdown()` when done.
 */
export class Telemetry {
  /** @internal */
  readonly native: NativeTelemetry;

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
   */
  static langfuse(options: TelemetryOptions & { publicKey?: string; secretKey?: string; baseUrl?: string } = {}): Telemetry {
    return new Telemetry({ kind: "langfuse", ...options, captureContent: options.captureContent ?? false });
  }

  /** Any OTLP/HTTP (JSON) collector, e.g. "http://localhost:4318/v1/traces". */
  static otlp(endpoint: string, options: TelemetryOptions & { headers?: Record<string, string> } = {}): Telemetry {
    return new Telemetry({ kind: "otlp", endpoint, ...options, headers: options.headers ?? {}, captureContent: options.captureContent ?? false });
  }

  /**
   * The local trace viewer (`npx criogen trace`, `serveTraces()`, design 0023).
   * Content only with `captureContent: true`; it stays on this computer either way.
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

  /** Exported, failed and dropped counts, and the last error. */
  stats(): { exported: number; failed: number; dropped: number; lastError: string | null } {
    return JSON.parse(this.native.stats());
  }

  toJSON(): Record<string, unknown> {
    return { telemetry: true };
  }
}

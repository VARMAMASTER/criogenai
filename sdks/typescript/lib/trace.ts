// The local trace viewer (design 0023): every run on a page, on your own
// computer. `npx criogen trace`, or from code:
//
//   const viewer = await serveTraces();               // http://127.0.0.1:4318
//   const agent = new Agent({ model, telemetry: Telemetry.local() });
//
// Each run's model calls, tool calls and steps on a time axis, with durations,
// tokens, cost (from your `prices`) and failures. Prompts and outputs appear
// only with `Telemetry.local({ captureContent: true })`; nothing leaves the
// computer. It listens on 127.0.0.1 unless given a `token`. Node only.

import { errorFor, translate } from "./errors";
import { native } from "./native";

export interface TraceViewerOptions {
  /** Default 4318 (OTLP/HTTP's port); 0 picks a free one. */
  port?: number;
  /** Default "127.0.0.1"; anything else needs a `token`. */
  host?: string;
  /** A SQLite file (default ~/.criogen/traces.db) or ":memory:". */
  db?: string;
  keepRuns?: number;
  keepDays?: number;
  /** USD per million tokens by "provider:model" or model name, for the cost column. */
  prices?: Record<string, { input?: number; output?: number }>;
  /** Clients must send `Authorization: Bearer <token>`. */
  token?: string;
}

/** A viewer serving in the background. */
export interface TraceViewer {
  /** The page's address. */
  readonly url: string;
  stop(): Promise<void>;
}

interface TraceAddon {
  traceStart(spec: string): Promise<string>;
  traceStop(handle: number): Promise<void>;
}

/** Starts the local trace viewer in the background. */
export async function serveTraces(options: TraceViewerOptions = {}): Promise<TraceViewer> {
  const addon = native as unknown as Partial<TraceAddon>;
  if (typeof addon.traceStart !== "function") {
    throw errorFor("unsupported", "the trace viewer needs criogen's engine 0.1.5 or newer, on a server (not in the browser)");
  }
  const host = options.host ?? "127.0.0.1";
  const port = options.port ?? 4318;
  const spec = {
    listen: host.includes(":") ? `[${host}]:${port}` : `${host}:${port}`,
    db: options.db === ":memory:" ? "memory" : (options.db ?? "default"),
    keep_runs: options.keepRuns,
    keep_days: options.keepDays,
    prices: options.prices,
    token: options.token,
  };
  let started: { handle: number; url: string };
  try {
    started = JSON.parse(await addon.traceStart(JSON.stringify(spec)));
  } catch (error) {
    throw translate(error);
  }
  let stopped = false;
  return {
    url: started.url,
    async stop() {
      if (stopped) return;
      stopped = true;
      await (addon as TraceAddon).traceStop(started.handle);
    },
  };
}

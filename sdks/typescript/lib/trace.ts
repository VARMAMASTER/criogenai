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

/** Settings for `serveTraces`. */
export interface TraceViewerOptions {
  /** Default 4318 (OTLP/HTTP's port); 0 picks a free one. */
  port?: number;
  /** Default "127.0.0.1"; anything else needs a `token`. */
  host?: string;
  /** A SQLite file (default ~/.criogen/traces.db) or ":memory:". */
  db?: string;
  /**
   * How many of the newest runs are kept (at least 1); older ones are deleted.
   * @default 10000
   */
  keepRuns?: number;
  /**
   * How many days a run is kept (at least 1).
   * @default 14
   */
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
  /** Stops serving. The runs stay in the database file. Calling it twice does nothing. */
  stop(): Promise<void>;
}

interface TraceAddon {
  traceStart(spec: string): Promise<string>;
  traceStop(handle: number): Promise<void>;
}

/**
 * Starts the local trace viewer in the background.
 *
 * It receives traces over OTLP/HTTP (`Telemetry.local()` sends there) and
 * shows each run on a page at `viewer.url`. Node only; the same viewer runs
 * from the command line with `npx criogen trace`.
 *
 * @param options Port, host, database, retention, prices and token.
 * @returns The running viewer: its `url` and `stop()`.
 * @throws {ValidationError} The viewer cannot start (`invalid_value`): the port is taken, the database cannot be opened, or the host is not this computer and there is no `token`.
 * @throws {CriogenError} The engine has no trace viewer (`unsupported`, e.g. in the browser).
 *
 * @example
 * import { Agent, FakeModel, Telemetry, serveTraces } from "criogen";
 *
 * const viewer = await serveTraces({ port: 0, db: ":memory:" });
 * const telemetry = Telemetry.local({ url: viewer.url });
 * const agent = new Agent({ model: new FakeModel(["Hello."]), telemetry });
 * await agent.run("Hi");
 * await telemetry.flush();
 * console.log(`Open ${viewer.url} to see the run.`);
 * await viewer.stop();
 */
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

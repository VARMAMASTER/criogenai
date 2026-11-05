// `ready()`: one call that says whether Criogen can work here, for start-up
// checks and health endpoints. It never throws; it reports. In the browser it
// also loads the engine (WebAssembly, design 0021): call it once, first.

import { Agent } from "./agent";
import { isCriogenError } from "./errors";
import type { Model } from "./models";
import { host, loadNative, prepareNative } from "./native";

export interface ReadyReport {
  /** Everything checked is fine. */
  ok: boolean;
  /** The engine (the native addon, or WebAssembly in a browser): loads here or not. */
  engine: { ok: boolean; platform: string; arch: string; libc: string | null; error?: string; code?: string };
  /** Only when a model was given: its credentials work and it answered. */
  model?: { ok: boolean; model: string; latencyMs: number; error?: string; code?: string };
}

export interface ReadyOptions {
  /** Also call this model once with a tiny prompt (spends a few tokens). */
  model?: Model;
  /** Give up on the model check after this long (default 30 s). */
  timeoutMs?: number;
  /**
   * Browser only: where the WebAssembly engine is, when it is not next to the
   * script (a URL, a `Response`, its bytes, or a compiled `WebAssembly.Module`).
   */
  wasm?: unknown;
}

function describe(error: unknown): { error: string; code?: string } {
  if (isCriogenError(error)) return { error: error.message, code: error.code };
  return { error: error instanceof Error ? error.message : String(error) };
}

/** Checks the engine loads and, with `model`, that the model answers. */
export async function ready(options: ReadyOptions = {}): Promise<ReadyReport> {
  const where = host();
  let engine: ReadyReport["engine"];
  try {
    await prepareNative(options.wasm);
    loadNative();
    engine = { ok: true, ...host() };
  } catch (error) {
    engine = { ok: false, ...where, ...describe(error) };
  }
  const report: ReadyReport = { ok: engine.ok, engine };
  if (options.model === undefined) return report;

  const name = typeof options.model === "string" ? options.model : `${(options.model as { kind?: string }).kind ?? "model"}:${(options.model as { model?: string }).model ?? ""}`;
  const started = Date.now();
  if (!engine.ok) {
    report.model = { ok: false, model: name, latencyMs: 0, error: "the engine is not available" };
    report.ok = false;
    return report;
  }
  try {
    const agent = new Agent({ model: options.model, limits: { modelCalls: 1 } });
    const result = await agent.run("Reply with the single word OK.", { signal: AbortSignal.timeout(options.timeoutMs ?? 30_000) });
    report.model = { ok: result.modelCalls > 0, model: name, latencyMs: Date.now() - started };
  } catch (error) {
    report.model = { ok: false, model: name, latencyMs: Date.now() - started, ...describe(error) };
  }
  report.ok = report.engine.ok && report.model.ok;
  return report;
}

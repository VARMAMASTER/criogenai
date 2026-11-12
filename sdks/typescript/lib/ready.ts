// `ready()`: one call that says whether CriogenAI can work here, for start-up
// checks and health endpoints. It never throws; it reports. In the browser it
// also loads the engine (WebAssembly, design 0021): call it once, first.

import { Agent } from "./agent";
import { isCriogenError } from "./errors";
import type { Model } from "./models";
import { host, loadNative, prepareNative } from "./native";

/** What `ready()` found. */
export interface ReadyReport {
  /** Everything checked is fine. */
  ok: boolean;
  /**
   * The engine (the native addon, or WebAssembly in a browser): loads here or not.
   * `platform`, `arch` and `libc` describe this system; `error` and `code` say why it failed.
   */
  engine: { ok: boolean; platform: string; arch: string; libc: string | null; error?: string; code?: string };
  /**
   * Only when a model was given: its credentials work and it answered.
   * `model` names it, `latencyMs` is how long the call took; `error` and `code` say why it failed.
   */
  model?: { ok: boolean; model: string; latencyMs: number; error?: string; code?: string };
}

/** Settings for `ready()`. */
export interface ReadyOptions {
  /** Also call this model once with a tiny prompt (spends a few tokens). */
  model?: Model;
  /**
   * Give up on the model check after this long, in milliseconds.
   * @default 30000
   */
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

/**
 * Checks the engine loads and, with `model`, that the model answers.
 *
 * Use it at start-up or in a health endpoint. It never throws: every problem
 * is in the report. In the browser (`criogenai/web`) it also loads the
 * WebAssembly engine, so call it once before anything else there.
 *
 * @param options `model` to also call a model once, `timeoutMs` for that call, `wasm` for the browser engine's location.
 * @returns `ok` and what was checked.
 *
 * @example
 * import { ready } from "criogenai";
 *
 * const report = await ready(); // or ready({ model: "openai:gpt-5-mini" })
 * console.log(report.ok, report.engine.platform, report.engine.arch);
 * if (!report.ok) console.error(report.engine.error ?? report.model?.error);
 */
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

  const name = modelName(options.model);
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

/** `"provider:model"` for a string or provider model; else the class name (e.g. "FakeModel"). */
function modelName(model: unknown): string {
  if (typeof model === "string") return model;
  const m = model as { kind?: unknown; model?: unknown; constructor?: { name?: string } } | null | undefined;
  if (typeof m?.kind === "string" && typeof m.model === "string" && m.model) return `${m.kind}:${m.model}`;
  if (typeof m?.model === "string" && m.model) return m.model;
  return m?.constructor?.name || "model";
}

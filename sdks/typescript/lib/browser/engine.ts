// The engine in the browser and other JavaScript hosts (design 0021): the
// Rust engine compiled to WebAssembly, through the same bridge as the Node
// addon (criogenai-js). WebAssembly compiles asynchronously, so `ready()` loads
// it once, before anything else runs.

import init, * as glue from "../../web-dist/engine/criogenai.js";
import { NativeUnavailableError } from "../errors";
import type { Addon, EngineLoader, HostInfo } from "../native";

let loaded = false;
let loading: Promise<void> | undefined;

function host(): HostInfo {
  const inBrowser = typeof (globalThis as { navigator?: unknown }).navigator === "object";
  return { platform: inBrowser ? "browser" : "javascript", arch: "wasm32", libc: null };
}

/** The WebAssembly engine; `prepare(source)` compiles it (once). */
export const webLoader: EngineLoader = {
  load(): Addon {
    if (!loaded) {
      throw new NativeUnavailableError(
        "native_unavailable",
        "the engine is not loaded yet: call `await ready()` once, before using CriogenAI in the browser",
        { ...host() },
      );
    }
    return glue as unknown as Addon;
  },
  async prepare(source?: unknown): Promise<void> {
    if (loaded) return;
    loading ??= init(source === undefined ? undefined : { module_or_path: source as never }).then(
      () => {
        loaded = true;
      },
      (error: unknown) => {
        loading = undefined;
        throw error;
      },
    );
    await loading;
  },
  host,
};

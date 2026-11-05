// In the browser bundle, in place of ../native-node (the Node addon loader):
// the browser entry selects the WebAssembly engine before anything runs.

import { NativeUnavailableError } from "../errors";
import type { EngineLoader, HostInfo } from "../native";

export const PLATFORM_PACKAGES: Record<string, string> = {};

export function nodeHost(): HostInfo {
  return { platform: "browser", arch: "wasm32", libc: null };
}

export const nodeLoader: EngineLoader = {
  load() {
    throw new NativeUnavailableError("native_unavailable", "the Node addon does not run in a browser", nodeHost());
  },
  prepare: async () => {},
  host: nodeHost,
};

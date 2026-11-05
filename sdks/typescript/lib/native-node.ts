// The engine on servers (design 0012 §4): the native addon, built from source
// (native/criogen.node, as scripts/test-typescript.sh makes) or the prebuilt
// package for this platform (criogen-<platform>-<arch>, an optional
// dependency). With CRIOGEN_ENGINE=wasm, the browser's engine instead (design
// 0021): the same test suite then checks the WebAssembly build.

import * as path from "path";
import { NativeUnavailableError } from "./errors";
import type { Addon, EngineLoader, HostInfo } from "./native";

/** Platform packages whose name differs from criogen-<platform>-<arch>: npm's spam
 * filter refused `criogen-win32-x64`, so Windows uses `criogen-windows-x64`. */
export const PLATFORM_PACKAGES: Record<string, string> = { "win32-x64": "criogen-windows-x64" };

/** This system, for error messages and `ready()`: platform, architecture, C library. */
export function nodeHost(): HostInfo {
  let libc: string | null = null;
  if (process.platform === "linux") {
    try {
      const report = process.report?.getReport?.() as { header?: { glibcVersionRuntime?: string } } | undefined;
      const glibc = report?.header?.glibcVersionRuntime;
      libc = glibc ? `glibc ${glibc}` : "musl (or no glibc)";
    } catch {
      libc = "unknown";
    }
  }
  return { platform: process.platform, arch: process.arch, libc };
}

function unavailable(message: string, pkg: string, cause: unknown): NativeUnavailableError {
  const h = nodeHost();
  const where = `${h.platform}-${h.arch}${h.libc ? ` (${h.libc})` : ""}`;
  const reason = cause instanceof Error ? cause.message.split(/\r?\n/)[0] : String(cause);
  return new NativeUnavailableError(
    "native_unavailable",
    `${message} on ${where}: ${reason}. Supported: Linux x64 and ARM64 with glibc 2.28 or newer, ` +
      `macOS (Apple Silicon, Intel), Windows x64; Node 18+, Node runtime (not Edge).`,
    { ...h, package: pkg },
    cause,
  );
}

function load(): Addon {
  if (process.env.CRIOGEN_ENGINE === "wasm") {
    const glue = path.join(__dirname, "..", "web-dist", "node", "criogen.js");
    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      return require(glue);
    } catch (error) {
      throw unavailable("criogen's WebAssembly engine could not be loaded (npm run build:web)", glue, error);
    }
  }
  const local = path.join(__dirname, "..", "native", "criogen.node");
  const target = `${process.platform}-${process.arch}`;
  const prebuilt = PLATFORM_PACKAGES[target] ?? `criogen-${target}`;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require(local);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    // The file is there but cannot be loaded here (built for another system, or
    // a C library it does not match, such as an older glibc or musl).
    if (code === "ERR_DLOPEN_FAILED") throw unavailable("criogen's engine could not be loaded", "native/criogen.node", error);
    if (code !== "MODULE_NOT_FOUND") throw error;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require(prebuilt);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === "ERR_DLOPEN_FAILED") throw unavailable(`criogen's engine (${prebuilt}) could not be loaded`, prebuilt, error);
    if (code !== "MODULE_NOT_FOUND") throw error;
    throw unavailable(`criogen has no engine package installed (${prebuilt})`, prebuilt, error);
  }
}

/** The Node addon: loads synchronously, so `prepare` only checks it loads. */
export const nodeLoader: EngineLoader = {
  load,
  prepare: async () => {
    load();
  },
  host: nodeHost,
};

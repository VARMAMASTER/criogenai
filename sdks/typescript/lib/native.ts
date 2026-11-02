// The native addon (design 0012 §4): loaded on first use, never at import, so a
// system it cannot run on gets a NativeUnavailableError where it is used.

import * as path from "path";
import { NativeUnavailableError } from "./errors";

/** A JavaScript tool as the engine calls it: request JSON in, envelope JSON out. */
export type NativeTool = (request: string) => Promise<string>;

export interface NativeStorage {
  op(op: string, payload: string): Promise<string>;
  close(): Promise<void>;
}

export interface NativeStream {
  next(): Promise<string | null>;
  finish(): Promise<string>;
  cancel(): void;
}

export interface NativeAgent {
  /** The agent's handle, for a workflow node. */
  readonly handle: number;
  run(payload: string, cancelId: number): Promise<string>;
  thread(op: string, payload: string, cancelId: number): Promise<string>;
  stream(op: string, payload: string): NativeStream;
}

export interface NativeGateway {
  chat(payload: string, cancelId: number): Promise<string>;
  stream(payload: string): NativeChatStream;
  embed(payload: string, cancelId: number): Promise<string>;
}

export interface NativeChatStream {
  next(): Promise<string | null>;
  reply(): Promise<string>;
  cancel(): void;
}

export interface NativeTelemetry {
  readonly handle: number;
  flush(): Promise<void>;
  shutdown(): Promise<void>;
  stats(): string;
}

export interface NativeWorkflow {
  run(payload: string, cancelId: number): Promise<string>;
  resume(payload: string, cancelId: number): Promise<string>;
  thread(op: string, payload: string, cancelId: number): Promise<string>;
}

interface Addon {
  NativeStorage: new (spec: string) => NativeStorage;
  NativeAgent: new (config: string, tools: NativeTool[], storage: NativeStorage, hooks: NativeTool[]) => NativeAgent;
  NativeTelemetry: new (spec: string) => NativeTelemetry;
  NativeGateway: new (config: string) => NativeGateway;
  NativeWorkflow: new (config: string, nodes: NativeTool[], routers: NativeTool[], storage: NativeStorage) => NativeWorkflow;
  newCancelId(): number;
  cancelCall(id: number): void;
  releaseCancelId(id: number): void;
}

/**
 * The addon: one built from source (native/criogen.node, as
 * scripts/test-typescript.sh makes), else the prebuilt package for this
 * platform (criogen-<platform>-<arch>, an optional dependency).
 */
/** Platform packages whose name differs from criogen-<platform>-<arch>: npm's spam
 * filter refused `criogen-win32-x64`, so Windows uses `criogen-windows-x64`. */
export const PLATFORM_PACKAGES: Record<string, string> = { "win32-x64": "criogen-windows-x64" };

/** This system, for error messages and `ready()`: platform, architecture, C library. */
export function host(): { platform: string; arch: string; libc: string | null } {
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
  const h = host();
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

let loaded: Addon | undefined;
let failure: unknown;

/** The engine, loaded on first use (never at import); the same error each time it cannot load. */
export function loadNative(): Addon {
  if (loaded) return loaded;
  if (failure !== undefined) throw failure;
  try {
    loaded = load();
    return loaded;
  } catch (error) {
    failure = error;
    throw error;
  }
}

/** The engine. Importing criogen never loads it; the first property access does. */
export const native: Addon = new Proxy({} as Addon, {
  get: (_target, property) => Reflect.get(loadNative() as object, property),
});

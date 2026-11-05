// The engine (design 0012 §4, design 0021): the Node addon on servers, the
// same engine as WebAssembly in the browser. Loaded on first use, never at
// import, so a system it cannot run on gets a NativeUnavailableError where it
// is used. Both engines have this interface (one Rust bridge, criogen-js).

import * as node from "./native-node";

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

/** What both engines export. */
export interface Addon {
  NativeStorage: new (spec: string) => NativeStorage;
  NativeAgent: new (config: string, tools: NativeTool[], storage: NativeStorage, hooks: NativeTool[]) => NativeAgent;
  NativeTelemetry: new (spec: string) => NativeTelemetry;
  NativeGateway: new (config: string) => NativeGateway;
  NativeWorkflow: new (config: string, nodes: NativeTool[], routers: NativeTool[], storage: NativeStorage) => NativeWorkflow;
  newCancelId(): number;
  cancelCall(id: number): void;
  releaseCancelId(id: number): void;
  /** The named providers (design 0006 §4.1) as JSON. */
  presets(): string;
  /** Registers a JavaScript model function (design 0021 §7); its handle names it in model specs. */
  registerFunction(fn: NativeTool): number;
  /** Forgets a model function (models built from it keep working). */
  releaseFunction(handle: number): void;
}

/** Where the engine runs: platform, architecture, C library. */
export interface HostInfo {
  platform: string;
  arch: string;
  libc: string | null;
}

/** How an engine is found: the Node addon (the default) or WebAssembly (the browser entry). */
export interface EngineLoader {
  /** The engine, now; throws a NativeUnavailableError when it cannot be had. */
  load(): Addon;
  /** Gets the engine ready (WebAssembly compiles asynchronously); `source` says where it is. */
  prepare(source?: unknown): Promise<void>;
  /** This system, for error messages and `ready()`. */
  host(): HostInfo;
}

/** Platform packages whose name differs from criogen-<platform>-<arch>. */
export const PLATFORM_PACKAGES: Record<string, string> = node.PLATFORM_PACKAGES;

let loader: EngineLoader = node.nodeLoader;
let loaded: Addon | undefined;
let failure: unknown;

/** @internal Selects the engine (the browser entry chooses WebAssembly). */
export function useEngineLoader(next: EngineLoader): void {
  loader = next;
  loaded = undefined;
  failure = undefined;
}

/** This system, for error messages and `ready()`: platform, architecture, C library. */
export function host(): HostInfo {
  return loader.host();
}

/** The engine, loaded on first use (never at import); the same error each time it cannot load. */
export function loadNative(): Addon {
  if (loaded) return loaded;
  if (failure !== undefined) throw failure;
  try {
    loaded = loader.load();
    return loaded;
  } catch (error) {
    failure = error;
    throw error;
  }
}

/** Gets the engine ready, then forgets an earlier "not loaded yet". */
export async function prepareNative(source?: unknown): Promise<void> {
  await loader.prepare(source);
  if (!loaded) failure = undefined;
}

/** The engine. Importing criogen never loads it; the first property access does. */
export const native: Addon = new Proxy({} as Addon, {
  get: (_target, property) => Reflect.get(loadNative() as object, property),
});

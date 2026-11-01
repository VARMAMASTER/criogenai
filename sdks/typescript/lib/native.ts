// The native addon (design 0012 §4): loaded once from native/criogen.node.

import * as path from "path";

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

function load(): Addon {
  const local = path.join(__dirname, "..", "native", "criogen.node");
  const target = `${process.platform}-${process.arch}`;
  const prebuilt = PLATFORM_PACKAGES[target] ?? `criogen-${target}`;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require(local);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === "ERR_DLOPEN_FAILED") {
      // The file is there but cannot be loaded here (built for another system, or a
      // C library it does not match, such as Alpine/musl).
      throw new Error(
        `criogen's engine could not be loaded on ${process.platform}-${process.arch}. ` +
          `Supported: Linux (glibc) x64 and ARM64, macOS Apple Silicon and Intel, Windows x64. ` +
          `To build it yourself: npm run build:native`,
        { cause: error },
      );
    }
    if (code !== "MODULE_NOT_FOUND") throw error;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require(prebuilt);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "MODULE_NOT_FOUND") throw error;
    throw new Error(
      `criogen has no native addon for ${process.platform}-${process.arch}: ` +
        `install ${prebuilt}, or build from source (npm run build:native)`,
    );
  }
}

export const native: Addon = load();

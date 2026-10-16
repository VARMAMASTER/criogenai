// The native addon (design 0012 §4): loaded once from native/agentbuild.node.

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
  run(payload: string, cancelId: number): Promise<string>;
  thread(op: string, payload: string, cancelId: number): Promise<string>;
  stream(op: string, payload: string): NativeStream;
}

interface Addon {
  NativeStorage: new (spec: string) => NativeStorage;
  NativeAgent: new (config: string, tools: NativeTool[], storage: NativeStorage) => NativeAgent;
  newCancelId(): number;
  cancelCall(id: number): void;
  releaseCancelId(id: number): void;
}

/**
 * The addon: one built from source (native/agentbuild.node, as
 * scripts/test-typescript.sh makes), else the prebuilt package for this
 * platform (@agentbuild/sdk-<platform>-<arch>, an optional dependency).
 */
function load(): Addon {
  const local = path.join(__dirname, "..", "native", "agentbuild.node");
  const prebuilt = `@agentbuild/sdk-${process.platform}-${process.arch}`;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require(local);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "MODULE_NOT_FOUND") throw error;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require(prebuilt);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "MODULE_NOT_FOUND") throw error;
    throw new Error(
      `@agentbuild/sdk has no native addon for ${process.platform}-${process.arch}: ` +
        `install ${prebuilt}, or build from source (npm run build:native)`,
    );
  }
}

export const native: Addon = load();

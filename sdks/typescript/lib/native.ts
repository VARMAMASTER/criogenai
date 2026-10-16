// The native addon (design 0012 §4): loaded once from native/agentbuild.node.

import * as path from "path";

/** A JavaScript tool as the engine calls it: request JSON in, envelope JSON out. */
export type NativeTool = (request: string) => Promise<string>;

export interface NativeStorage {
  op(op: string, payload: string): Promise<string>;
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

// eslint-disable-next-line @typescript-eslint/no-require-imports
export const native: Addon = require(path.join(__dirname, "..", "native", "agentbuild.node"));

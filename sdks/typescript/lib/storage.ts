// Storage and memory (design 0012 §2): where threads, long-term memory and
// the audit trail live. The engine does the work; this holds the settings.

import { native, NativeStorage } from "./native";
import { translate } from "./errors";

export interface RetentionOptions {
  threadTtlSecs?: number;
  keepCheckpoints?: number;
  dropBranches?: boolean;
}

export interface StorageOptions {
  /** `undefined` (process memory), "sqlite://path.db" or "postgres://…". */
  url?: string;
  /** "k2:<64 hex>,k1:<64 hex>" (the first is active): encryption at rest. */
  encryptionKeys?: string;
  /** Record who did what (design 0010). */
  audit?: boolean;
  retention?: RetentionOptions;
  onBusy?: "wait" | "reject" | "interrupt";
  busyTimeoutSecs?: number;
}

/** Threads, memory and the audit trail. Agents built with one share it. */
export class Storage {
  readonly url?: string;
  /** @internal */
  readonly native: NativeStorage;

  constructor(options: StorageOptions | string = {}) {
    const o = typeof options === "string" ? { url: options } : options;
    this.url = o.url;
    try {
      this.native = new native.NativeStorage(JSON.stringify(o));
    } catch (error) {
      throw translate(error);
    }
  }

  private async op<T>(op: string, payload: Record<string, unknown> = {}): Promise<T> {
    try {
      return JSON.parse(await this.native.op(op, JSON.stringify(payload))) as T;
    } catch (error) {
      throw translate(error);
    }
  }

  /** Closes the database's connections (SQLite releases the file). Later calls fail with StorageError. */
  async close(): Promise<void> {
    await this.native.close();
  }

  /** Saves a fact under `namespace` (e.g. "users/42"). */
  async remember(namespace: string, key: string, value: unknown, options: { ttlSecs?: number } = {}): Promise<void> {
    await this.op("memory_put", { namespace, key, value, ttlSecs: options.ttlSecs });
  }

  /** The item, or null. */
  recall(namespace: string, key: string): Promise<Record<string, unknown> | null> {
    return this.op("memory_get", { namespace, key });
  }

  /** Items in `namespace` and below matching `text` (keywords). */
  search(namespace: string, text?: string, limit = 10): Promise<Record<string, unknown>[]> {
    return this.op("memory_search", { namespace, text, limit });
  }

  /** Deletes everything in `namespace` and below (e.g. one patient). */
  forgetAll(namespace: string): Promise<number> {
    return this.op("memory_delete_namespace", { namespace });
  }

  /** Applies the retention rules once. */
  prune(): Promise<Record<string, number>> {
    return this.op("prune");
  }

  /** Audit records, oldest first (needs `audit: true`). */
  auditRecords(query: { threadId?: string; action?: string; afterSeq?: number; limit?: number } = {}): Promise<Record<string, unknown>[]> {
    return this.op("audit_query", query);
  }

  /** Checks the audit trail's hash chain. */
  verifyAudit(): Promise<{ records: number; ok: boolean; first_broken: number | null }> {
    return this.op("audit_verify");
  }
}

export interface MemoryOptions {
  recall?: number;
  tools?: boolean;
  ttlSecs?: number;
}

/**
 * Long-term memory for an agent: recalled before each model call, and
 * remember / forget / search_memory tools for the model.
 */
export class Memory {
  /** @internal */
  readonly spec: Record<string, unknown>;

  private constructor(spec: Record<string, unknown>) {
    this.spec = spec;
  }

  /** One namespace per value of the run's `key` (e.g. per user): `prefix/<value>`. */
  static byState(key: string, prefix: string, options: MemoryOptions = {}): Memory {
    return new Memory({ byState: key, prefix, ...options });
  }

  /** One namespace for every run. */
  static shared(namespace: string, options: MemoryOptions = {}): Memory {
    return new Memory({ shared: namespace, ...options });
  }

  /**
   * Searches by meaning with `embedder`: recall and `search_memory` find facts
   * that say the same thing in other words. If the embedder fails, search
   * falls back to keywords.
   */
  semantic(embedder: { readonly spec: Record<string, unknown> }): Memory {
    return new Memory({ ...this.spec, embedder: embedder.spec });
  }
}

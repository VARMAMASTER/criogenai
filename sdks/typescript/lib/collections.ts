// Collections: documents searched by meaning (design 0025).
//
//   const docs = await Collection.open("handbook", { embedder: Embedder.openai(), storage });
//   await docs.add([{ id: "leave", text: "Annual leave is 25 days.", metadata: { section: "hr" } }]);
//   const hits = await docs.search("How many vacation days?", { k: 5 });
//   const agent = new Agent({ model, tools: [docs.asTool("search_handbook", "Search the staff handbook.")] });
//
// Documents are kept in the storage (encrypted when it is) and indexed in
// memory when the collection opens: exact by default, `index: "hnsw"` for very
// large collections (faster queries, approximate, slower to open). Works in
// both engines (in the browser, over in-memory storage).

import { translate } from "./errors";
import { type Embedder, requireEngine015 } from "./models";
import { Storage } from "./storage";

/** A text to search, with an id and metadata. */
export interface Document {
  id: string;
  text: string;
  metadata?: Record<string, unknown>;
}

/** A search result: the document and its cosine similarity to the query. */
export interface Hit {
  id: string;
  text: string;
  metadata: Record<string, unknown>;
  score: number;
}

export interface CollectionOptions {
  embedder: Embedder;
  /** A Storage or a URL; in memory without one. */
  storage?: Storage | string;
  /** "flat" (exact, the default) or "hnsw". */
  index?: "flat" | "hnsw";
  /** HNSW only: candidates kept per search (default 64; higher is slower and more accurate). */
  efSearch?: number;
}

/** A collection as an agent tool (`collection.asTool(...)`). */
export class CollectionTool {
  constructor(
    readonly collection: Collection,
    readonly name: string,
    readonly description: string,
  ) {}
}

/** Documents searched by meaning. Open one with `Collection.open`. */
export class Collection {
  /** @internal */
  constructor(
    readonly name: string,
    readonly storage: Storage,
    /** @internal */ readonly handle: number,
  ) {}

  private async op<T>(op: string, payload: Record<string, unknown> = {}): Promise<T> {
    try {
      return JSON.parse(await this.storage.native.op(op, JSON.stringify({ handle: this.handle, ...payload }))) as T;
    } catch (error) {
      throw translate(error);
    }
  }

  /** Opens collection `name`, building its index from the stored vectors. */
  static async open(name: string, options: CollectionOptions): Promise<Collection> {
    requireEngine015("Collections");
    const storage = options.storage instanceof Storage ? options.storage : new Storage({ url: options.storage });
    const spec = { name, embedder: options.embedder.spec, index: options.index ?? "flat", efSearch: options.efSearch };
    let opened: { handle: number };
    try {
      opened = JSON.parse(await storage.native.op("collection_open", JSON.stringify(spec)));
    } catch (error) {
      throw translate(error);
    }
    return new Collection(name, storage, opened.handle);
  }

  /** Embeds, stores and indexes documents (an existing id is replaced). Returns how many the collection holds. */
  add(documents: Document[]): Promise<number> {
    return this.op("collection_add", { documents: documents.map((d) => ({ id: d.id, text: d.text, metadata: d.metadata ?? {} })) });
  }

  /** The `k` documents closest in meaning to `query`, best first; `where` keeps only those whose metadata has these values. */
  search(query: string, options: { k?: number; where?: Record<string, unknown> } = {}): Promise<Hit[]> {
    return this.op("collection_search", { query, k: options.k ?? 5, where: options.where ?? {} });
  }

  /** Removes a document; `true` if it was there. */
  remove(id: string): Promise<boolean> {
    return this.op("collection_remove", { id });
  }

  /** Rebuilds the index from storage (another process added documents). */
  reload(): Promise<number> {
    return this.op("collection_reload");
  }

  /** Documents in the index. */
  count(): Promise<number> {
    return this.op("collection_count");
  }

  /** Forgets the in-memory index (the documents stay stored). */
  close(): Promise<boolean> {
    return this.op("collection_close");
  }

  /** This collection as an agent tool: the model gives a `query` (and optionally `k`, up to 20) and gets the hits. */
  asTool(name: string, description: string): CollectionTool {
    return new CollectionTool(this, name, description);
  }
}

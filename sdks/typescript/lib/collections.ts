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
  /** Unique in its collection (at most 256 bytes); adding the same id again replaces the document. */
  id: string;
  /** What is embedded, searched and returned (at most 100,000 characters; split longer texts). */
  text: string;
  /** Kept with the document, returned with hits and usable in `search`'s `where` filter (at most 8 KiB as JSON). */
  metadata?: Record<string, unknown>;
}

/** A search result: the document and its cosine similarity to the query. */
export interface Hit {
  /** The document's id. */
  id: string;
  /** The document's text. */
  text: string;
  /** The document's metadata (`{}` when it has none). */
  metadata: Record<string, unknown>;
  /** Cosine similarity between the query and the document, from -1 to 1; higher is closer. */
  score: number;
}

/** Settings for `Collection.open`. */
export interface CollectionOptions {
  /** The embedding model that turns texts into vectors, e.g. `Embedder.openai()`. Use the same one every time you open the collection. */
  embedder: Embedder;
  /** A Storage or a URL; in memory without one. */
  storage?: Storage | string;
  /**
   * `"flat"` compares the query with every vector (exact, builds at once).
   * `"hnsw"` is approximate: faster queries on very large collections, slower
   * to open (the index is rebuilt each time).
   * @default "flat"
   */
  index?: "flat" | "hnsw";
  /** HNSW only: candidates kept per search (default 64; higher is slower and more accurate). */
  efSearch?: number;
}

/**
 * A collection as an agent tool, made by `collection.asTool(name, description)`.
 * Put it in an agent's `tools`.
 */
export class CollectionTool {
  /**
   * @param collection The collection the tool searches.
   * @param name The tool's name, as the model sees it.
   * @param description What the tool searches, for the model.
   */
  constructor(
    readonly collection: Collection,
    readonly name: string,
    readonly description: string,
  ) {}
}

/**
 * Documents searched by meaning: a small knowledge base for retrieval.
 *
 * Open one with `Collection.open`. Documents and their vectors are kept in
 * the storage (encrypted when the storage is) and indexed in memory when the
 * collection opens. Give it to an agent with `asTool`.
 *
 * @example
 * import { Collection, Embedder } from "criogen";
 *
 * const docs = await Collection.open("handbook", { embedder: Embedder.fake() });
 * await docs.add([
 *   { id: "leave", text: "Annual leave is 25 days a year.", metadata: { section: "hr" } },
 *   { id: "parking", text: "Staff parking is behind building B." },
 * ]);
 * const hits = await docs.search("how many days of annual leave", { k: 1 });
 * console.log(hits[0].id); // "leave"
 */
export class Collection {
  /**
   * @internal
   * @param name The collection's name.
   * @param storage Where its documents are stored.
   * @param handle The engine's handle for the open index.
   */
  constructor(
    /** The collection's name. */
    readonly name: string,
    /** The storage that holds its documents. */
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

  /**
   * Opens collection `name`, building its index from the stored vectors.
   *
   * A new name starts an empty collection. Opening the same name again (in
   * this or another process, on the same storage) finds the documents added
   * before.
   *
   * @param name The collection's name.
   * @param options The embedder, the storage and the index kind.
   * @throws {StorageError} The storage failed.
   */
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

  /**
   * Embeds, stores and indexes documents (an existing id is replaced). Returns how many the collection holds.
   *
   * @param documents The documents to add.
   * @returns How many documents the collection holds afterwards.
   * @throws {ValidationError} A document's id, text or metadata is too long (`invalid_value`).
   * @throws {ModelError} The embedding model failed.
   */
  add(documents: Document[]): Promise<number> {
    return this.op("collection_add", { documents: documents.map((d) => ({ id: d.id, text: d.text, metadata: d.metadata ?? {} })) });
  }

  /**
   * The `k` documents closest in meaning to `query`, best first; `where` keeps only those whose metadata has these values.
   *
   * @param query The question or text to match.
   * @param options `k`: how many hits (default 5). `where`: metadata fields and the values they must equal, e.g. `{ section: "hr" }`.
   * @throws {ModelError} The embedding model failed.
   */
  search(query: string, options: { k?: number; where?: Record<string, unknown> } = {}): Promise<Hit[]> {
    return this.op("collection_search", { query, k: options.k ?? 5, where: options.where ?? {} });
  }

  /**
   * Removes a document; `true` if it was there.
   *
   * @param id The document's id.
   */
  remove(id: string): Promise<boolean> {
    return this.op("collection_remove", { id });
  }

  /** Rebuilds the index from storage (another process added documents). Returns how many documents it holds. */
  reload(): Promise<number> {
    return this.op("collection_reload");
  }

  /** Documents in the index. */
  count(): Promise<number> {
    return this.op("collection_count");
  }

  /** Forgets the in-memory index (the documents stay stored). Later calls on this object fail; open it again to use it. */
  close(): Promise<boolean> {
    return this.op("collection_close");
  }

  /**
   * This collection as an agent tool: the model gives a `query` (and optionally `k`, up to 20) and gets the hits.
   *
   * The tool is read-only; each hit's text is cut to 2,000 characters.
   *
   * @param name The tool's name, e.g. `"search_handbook"`.
   * @param description What it searches, so the model knows when to call it.
   *
   * @example
   * import { Agent, Collection, Embedder, FakeModel } from "criogen";
   *
   * const docs = await Collection.open("handbook", { embedder: Embedder.fake() });
   * await docs.add([{ id: "leave", text: "Annual leave is 25 days a year." }]);
   * const agent = new Agent({
   *   model: new FakeModel(["Annual leave is 25 days."]),
   *   tools: [docs.asTool("search_handbook", "Search the staff handbook.")],
   * });
   * console.log((await agent.run("How much leave do I get?")).text);
   */
  asTool(name: string, description: string): CollectionTool {
    return new CollectionTool(this, name, description);
  }
}

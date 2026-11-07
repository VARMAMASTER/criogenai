# 0025: Vector indexes and retrieval

Status: approved by the user's request (2026-11-07, roadmap three, task 20).
Extends 0008 §16 (semantic memory search). The base of the RAG kit (task 24).

## What the benchmark showed

`cargo run -p criogen-bench --release --bin vectors` (one machine,
2026-11-07; random 768-dimension vectors, top 5, median of 15 queries):

| Items in a namespace | In memory | SQLite file |
|---|---|---|
| 1 000 | 3.3 ms | 17.8 ms |
| 10 000 | 28.9 ms | 206 ms |
| 50 000 | 163 ms | 1 072 ms |

Brute force grows linearly. For one user's or patient's memory (hundreds to
a few thousand items) it is fine, and memory search stays as it is. For a
document collection (10 000 to 1 000 000 chunks, the RAG case) it is not.

## Decision: our own in-memory index, vectors stored encrypted as today

The roadmap named `sqlite-vec` and `pgvector`. Neither is taken now:

- **Encryption at rest (0008 §15).** Both keep vectors in the database in a
  form the database can compare, so unencrypted. Embeddings can be inverted
  into text, and Criogen encrypts them today. A hospital deployment (Axon
  Health) must not lose that.
- **Packaging.** `sqlite-vec` is a C extension: every wheel and npm engine
  package (five platforms) would compile and ship it. `pgvector` must be
  installed on the server, and our portable PostgreSQL for tests has no
  build of it.
- **The sizes we target** (up to about a million chunks per process) are
  served in memory in milliseconds.

So vectors stay where they are (the memory store, encrypted when the store
is), and a **collection** keeps an index of them in process memory, built
when it opens and updated as documents are added. `pgvector` remains an
option for unencrypted Postgres deployments with many millions of vectors,
when someone needs it.

## The index (`criogen_storage::vector`)

```rust
trait VectorIndex: Send + Sync {
    fn add(&mut self, id: u64, vector: &[f32]) -> Result<(), StorageError>;
    fn remove(&mut self, id: u64) -> bool;
    fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)>; // cosine, best first
    fn len(&self) -> usize;
}
```

- `FlatIndex`: exact. Vectors normalised once and kept contiguous, so a
  query is one pass of dot products. Same ranking as memory search.
- `HnswIndex`: approximate (Hierarchical Navigable Small World graphs, from
  the published algorithm, our own code). `m` 16, `ef_construction` 100,
  `ef_search` 64 by default; a fixed random seed, so builds are
  reproducible. Removal marks a node deleted (searches skip it); past 30 %
  deleted the graph is rebuilt.
- Bounds: 1 to 8 192 dimensions (as embeddings), every value finite, at most
  `max_vectors` (default 2 000 000) per index, `k` at most 1 000.
- Recall and speed are measured by the benchmark (recall@10 against the
  flat index) and written in `docs/benchmarks.md`; the docs quote only
  those numbers.

### As measured (`--bin vectors -- --indexes [--clustered]`, 768 dimensions)

| Clustered vectors | Flat query | HNSW query | HNSW recall@10 | HNSW build |
|---|---|---|---|---|
| 10 000 | 2.3 ms | 0.78 ms | 1.00 | 7 s |
| 50 000 | 15.5 ms | 0.24 ms | 0.98 | 34 s |
| 100 000 | 16.6 ms | 0.18 ms | 0.88 | 54 s |

On uniform random vectors (no structure, the worst case for a graph index)
HNSW recall falls to 0.34 at 10 000 and 0.10 at 50 000. So:

- **The default is the flat index**, always: exact, built in a fraction of a
  second, 17 ms a query at 100 000 documents, and the same ranking as memory
  search. (The roadmap's "HNSW above 20 000" is dropped: rebuilding the
  graph at every open costs too much for what it saves.)
- **HNSW is opt-in** (`IndexKind::Hnsw`, `index="hnsw"`): about 100 times
  faster queries on large, clustered collections, approximate, and slow to
  build. Raise `ef_search` for recall. Persisting the graph (so it need not
  be rebuilt) is the next step if it is used.
- The memory store's own similarity search stays brute force (18 ms at
  1 000 items in SQLite, the per-user case).

Two changes made the index fast: the dot product works in blocks of eight
values without index checks (the compiler turns it into vector
instructions), and HNSW keeps visited nodes in a plain array.

## Collections and retrievers (`criogen_agent::retrieval`)

```rust
let docs = Collection::open(storage.memory(), "handbook", embedder).await?; // loads, builds the index
docs.add(vec![Document::new("leave-1", "Annual leave is 25 days.").meta("section", "hr")]).await?;
let hits = docs.search("How many vacation days?", 5).await?;        // Vec<Hit {id, text, metadata, score}>
let agent = Agent::builder(model).tool(docs.as_tool("search_handbook", "Search the staff handbook.")).build()?;
```

- A collection is a namespace of the memory store (`criogen` /
  `collections` / name): each document is an item with its text, metadata
  and vector, so it is saved, encrypted and retained like any memory.
- `Collection::open` reads every vector once (`MemoryStore::embedded`, a
  new method every backend implements, decrypting as any read does) and
  builds the index (flat unless HNSW is chosen). `add` embeds in batches
  (64 texts), stores, then indexes; `remove` deletes from both; `reload`
  rebuilds from storage (another process added documents). A query is
  embedded with the collection's embedder; vectors of another model are
  refused at `add` and skipped at `open`.
- Filters: `search_where(query, k, {"section": "hr"})` keeps hits whose
  metadata has those values (applied to a widened candidate set, `k × 4`,
  then cut to `k`).
- `trait Retriever { async fn retrieve(&self, query, k) -> Vec<Hit> }`:
  collections implement it, and so can anything else (a web search, your
  database). `retriever_tool(retriever, name, description)` makes any
  retriever an agent tool (`{query, k?}` → hits as JSON, text cut to 2 000
  characters each).
- Limits: a document's text up to 100 000 characters (split longer ones;
  the RAG kit will), metadata up to 8 KB, ids up to 256 bytes.

Python `criogen.Collection` and TypeScript `Collection` wrap the same Rust
collection (`open`, `add`, `remove`, `search`, `reload`, `count`,
`as_tool`). In TypeScript the operations go through `NativeStorage.op`
(`collection_*`), so both engines have them; in the browser the collection
works over the in-memory store.

## Not in this slice

`pgvector` and `sqlite-vec`, document loaders and splitters (task 24),
hybrid keyword + vector ranking, re-ranking models, persisting the HNSW graph
itself (it is rebuilt at open; the benchmark gives the build time).

## Tests

Index: flat ranking equals the brute-force function; HNSW recall@10 ≥ 0.95
on 5 000 random vectors and finds exact duplicates; removal, rebuild,
bounds, reproducible builds. Collections: add / search / remove / reload
across a reopen (SQLite), filters, another model refused, embedder failure,
encrypted storage keeps vectors encrypted and still searches, the tool in an
agent with `FakeModel` and `FakeEmbedder`. SDKs: the same through Python
and TypeScript.

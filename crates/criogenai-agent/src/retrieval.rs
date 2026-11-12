//! Collections of documents searched by meaning, and retrievers (design
//! 0025).
//!
//! ```no_run
//! use std::sync::Arc;
//! use criogenai_agent::retrieval::{Collection, Document};
//! # async fn run(store: Arc<dyn criogenai_storage::MemoryStore>) -> Result<(), Box<dyn std::error::Error>> {
//! let embedder = Arc::new(criogenai_providers::FakeEmbedder::new());
//! let handbook = Collection::open(store, "handbook", embedder).await?;
//! handbook.add(vec![Document::new("leave", "Annual leave is 25 days.").meta("section", "hr")]).await?;
//! let hits = handbook.search("How many vacation days?", 5).await?;
//! # Ok(()) }
//! ```
//!
//! A collection's documents are items of the memory store (namespace
//! `criogen/collections/<name>`), with their text, metadata and vector, so
//! they are saved, encrypted and retained like any memory. The index lives in
//! process memory: built when the collection opens, kept up to date by `add`
//! and `remove`.

use std::collections::HashMap;
use std::sync::Arc;

use criogenai_providers::{BoxFuture, Embedder, ModelError, ToolAnnotations, ToolSpec};
use criogenai_storage::vector::{FlatIndex, HnswConfig, HnswIndex, VectorIndex};
use criogenai_storage::{Embedding, MemoryStore, Namespace, StorageError};
use serde_json::{Map, Value, json};

use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};

/// Texts embedded per call when adding.
pub const EMBED_BATCH: usize = 64;
/// The longest document text, in characters (split longer ones).
pub const MAX_TEXT_CHARS: usize = 100_000;
/// The largest metadata, in bytes of JSON.
pub const MAX_METADATA_BYTES: usize = 8 * 1024;
/// The longest document id, in bytes.
pub const MAX_ID_BYTES: usize = 256;
/// Hits a retriever tool returns at most.
pub const MAX_TOOL_HITS: usize = 20;
/// A hit's text is cut to this many characters in a tool result.
pub const TOOL_TEXT_CHARS: usize = 2_000;

/// Why a collection could not do what was asked.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RetrievalError {
    /// The memory store failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The embedding model failed.
    #[error("the embedding model failed: {}", .0.message)]
    Embedding(Box<ModelError>),
    /// A document or an argument is not usable.
    #[error("{0}")]
    Invalid(String),
}

impl RetrievalError {
    /// The stable code: the storage error's, `model_failed` or
    /// `invalid_value`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Storage(e) => e.code(),
            Self::Embedding(_) => "model_failed",
            Self::Invalid(_) => "invalid_value",
        }
    }
}

fn invalid(message: impl Into<String>) -> RetrievalError {
    RetrievalError::Invalid(message.into())
}

/// A text to search, with an id and metadata.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Document {
    /// Unique in its collection; adding the same id replaces it.
    pub id: String,
    /// What is embedded and returned.
    pub text: String,
    /// Kept with it, returned with hits, usable as filters.
    pub metadata: Map<String, Value>,
}

impl Document {
    /// A document.
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            metadata: Map::new(),
        }
    }

    /// Adds a metadata field.
    pub fn meta(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// A search result.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Hit {
    /// The document's id.
    pub id: String,
    /// Its text.
    pub text: String,
    /// Its metadata.
    pub metadata: Map<String, Value>,
    /// Cosine similarity with the query (−1 to 1).
    pub score: f64,
}

impl Hit {
    /// A hit (for retrievers of your own).
    pub fn new(id: impl Into<String>, text: impl Into<String>, score: f64) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            metadata: Map::new(),
            score,
        }
    }

    fn to_json(&self, max_chars: usize) -> Value {
        let text = match self.text.char_indices().nth(max_chars) {
            Some((at, _)) => format!("{}…", &self.text[..at]),
            None => self.text.clone(),
        };
        json!({"id": self.id, "text": text, "metadata": self.metadata, "score": self.score})
    }
}

/// Anything that finds texts for a query: a collection, a web search, your
/// database.
pub trait Retriever: Send + Sync + 'static {
    /// The `k` best hits for `query`, best first.
    fn retrieve<'a>(
        &'a self,
        query: &'a str,
        k: usize,
    ) -> BoxFuture<'a, Result<Vec<Hit>, RetrievalError>>;
}

/// How a collection indexes its vectors.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum IndexKind {
    /// Exact: every vector compared with the query. Builds instantly; fast
    /// enough for most collections (see `docs/benchmarks.md`). The default.
    #[default]
    Flat,
    /// Approximate and faster to query on large collections, slower to build
    /// (it is rebuilt each time the collection opens).
    Hnsw(HnswConfig),
}

#[derive(Debug)]
struct State {
    index: Box<dyn VectorIndex>,
    by_key: HashMap<String, u64>,
    keys: HashMap<u64, String>,
    next: u64,
}

impl State {
    fn new(kind: IndexKind) -> Self {
        Self {
            index: match kind {
                IndexKind::Flat => Box::new(FlatIndex::new()),
                IndexKind::Hnsw(config) => Box::new(HnswIndex::new(config)),
            },
            by_key: HashMap::new(),
            keys: HashMap::new(),
            next: 1,
        }
    }

    fn slot(&mut self, key: &str) -> u64 {
        if let Some(&n) = self.by_key.get(key) {
            return n;
        }
        let n = self.next;
        self.next += 1;
        self.by_key.insert(key.to_string(), n);
        self.keys.insert(n, key.to_string());
        n
    }
}

struct Inner {
    name: String,
    namespace: Namespace,
    store: Arc<dyn MemoryStore>,
    embedder: Arc<dyn Embedder>,
    model: String,
    kind: IndexKind,
    state: tokio::sync::RwLock<State>,
}

/// Documents searched by meaning. Cheap to clone.
#[derive(Clone)]
pub struct Collection {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("name", &self.inner.name)
            .field("model", &self.inner.model)
            .finish_non_exhaustive()
    }
}

impl Collection {
    /// Opens collection `name` of `store` (exact index), reading its
    /// documents' vectors once to build the index.
    pub async fn open(
        store: Arc<dyn MemoryStore>,
        name: &str,
        embedder: Arc<dyn Embedder>,
    ) -> Result<Self, RetrievalError> {
        Self::open_with(store, name, embedder, IndexKind::Flat).await
    }

    /// [`open`](Self::open) with a chosen index.
    pub async fn open_with(
        store: Arc<dyn MemoryStore>,
        name: &str,
        embedder: Arc<dyn Embedder>,
        kind: IndexKind,
    ) -> Result<Self, RetrievalError> {
        let namespace = Namespace::new(["criogenai", "collections", name])?;
        let model = format!("{}/{}", embedder.provider(), embedder.model());
        let collection = Self {
            inner: Arc::new(Inner {
                name: name.to_string(),
                namespace,
                store,
                embedder,
                model,
                kind,
                state: tokio::sync::RwLock::new(State::new(kind)),
            }),
        };
        collection.reload().await?;
        Ok(collection)
    }

    /// Rebuilds the index from storage (another process added documents).
    pub async fn reload(&self) -> Result<(), RetrievalError> {
        let inner = &self.inner;
        let items = inner.store.embedded(&inner.namespace).await?;
        let mut state = State::new(inner.kind);
        for (item, embedding) in items {
            // Only this namespace (not deeper ones), and only this model.
            if item.namespace != inner.namespace || embedding.model != inner.model {
                continue;
            }
            let n = state.slot(&item.key);
            state.index.add(n, &embedding.vector)?;
        }
        *inner.state.write().await = state;
        Ok(())
    }

    /// The collection's name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Documents in the index.
    pub async fn len(&self) -> usize {
        self.inner.state.read().await.index.len()
    }

    /// Whether it holds no document.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RetrievalError> {
        let vectors = self
            .inner
            .embedder
            .embed(texts)
            .await
            .map_err(|e| RetrievalError::Embedding(Box::new(e)))?;
        if vectors.len() != texts.len() {
            return Err(invalid(format!(
                "the embedding model returned {} vectors for {} texts",
                vectors.len(),
                texts.len()
            )));
        }
        Ok(vectors)
    }

    /// Embeds, stores and indexes `documents` (an existing id is replaced).
    pub async fn add(&self, documents: Vec<Document>) -> Result<(), RetrievalError> {
        for d in &documents {
            if d.id.is_empty() || d.id.len() > MAX_ID_BYTES {
                return Err(invalid(format!(
                    "a document id has 1 to {MAX_ID_BYTES} bytes"
                )));
            }
            if d.text.chars().count() > MAX_TEXT_CHARS {
                return Err(invalid(format!(
                    "document `{}` is longer than {MAX_TEXT_CHARS} characters: split it",
                    d.id
                )));
            }
            if serde_json::to_vec(&d.metadata).map_or(usize::MAX, |b| b.len()) > MAX_METADATA_BYTES
            {
                return Err(invalid(format!(
                    "document `{}` has more than {MAX_METADATA_BYTES} bytes of metadata",
                    d.id
                )));
            }
        }
        for batch in documents.chunks(EMBED_BATCH) {
            let texts: Vec<String> = batch.iter().map(|d| d.text.clone()).collect();
            let vectors = self.embed(&texts).await?;
            for (d, vector) in batch.iter().zip(vectors) {
                self.inner
                    .store
                    .put_embedded(
                        &self.inner.namespace,
                        &d.id,
                        json!({"text": d.text, "metadata": d.metadata}),
                        None,
                        Embedding::new(self.inner.model.clone(), vector.clone()),
                    )
                    .await?;
                let mut state = self.inner.state.write().await;
                let n = state.slot(&d.id);
                state.index.add(n, &vector)?;
            }
        }
        Ok(())
    }

    /// Removes a document; `true` if it was there.
    pub async fn remove(&self, id: &str) -> Result<bool, RetrievalError> {
        let stored = self.inner.store.delete(&self.inner.namespace, id).await?;
        let mut state = self.inner.state.write().await;
        if let Some(n) = state.by_key.remove(id) {
            state.keys.remove(&n);
            state.index.remove(n);
        }
        Ok(stored)
    }

    /// The `k` documents closest in meaning to `query`, best first.
    pub async fn search(&self, query: &str, k: usize) -> Result<Vec<Hit>, RetrievalError> {
        self.search_where(query, k, &Map::new()).await
    }

    /// [`search`](Self::search), keeping only documents whose metadata has
    /// every field of `filter` with that value.
    pub async fn search_where(
        &self,
        query: &str,
        k: usize,
        filter: &Map<String, Value>,
    ) -> Result<Vec<Hit>, RetrievalError> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let vector = self
            .embed(&[query.to_string()])
            .await?
            .pop()
            .unwrap_or_default();
        let wanted = if filter.is_empty() {
            k
        } else {
            k.saturating_mul(4)
        };
        let found: Vec<(String, f32)> = {
            let state = self.inner.state.read().await;
            state
                .index
                .search(&vector, wanted)
                .into_iter()
                .filter_map(|(n, score)| Some((state.keys.get(&n)?.clone(), score)))
                .collect()
        };
        let mut hits = Vec::with_capacity(k);
        for (key, score) in found {
            let Some(item) = self.inner.store.get(&self.inner.namespace, &key).await? else {
                continue;
            };
            let metadata = item.value["metadata"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            if filter.iter().any(|(f, v)| metadata.get(f) != Some(v)) {
                continue;
            }
            hits.push(Hit {
                id: key,
                text: item.value["text"].as_str().unwrap_or_default().to_string(),
                metadata,
                score: f64::from(score),
            });
            if hits.len() == k {
                break;
            }
        }
        Ok(hits)
    }

    /// This collection as an agent tool (see [`retriever_tool`]).
    pub fn as_tool(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> RetrieverTool {
        retriever_tool(Arc::new(self.clone()), name, description)
    }
}

impl Retriever for Collection {
    fn retrieve<'a>(
        &'a self,
        query: &'a str,
        k: usize,
    ) -> BoxFuture<'a, Result<Vec<Hit>, RetrievalError>> {
        Box::pin(self.search(query, k))
    }
}

/// A retriever as an agent tool: `{query, k?}` in, hits as JSON out.
pub struct RetrieverTool {
    spec: ToolSpec,
    retriever: Arc<dyn Retriever>,
}

impl std::fmt::Debug for RetrieverTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetrieverTool")
            .field("name", &self.spec.name)
            .finish_non_exhaustive()
    }
}

/// Any retriever as an agent tool named `name`. The model gives a `query`
/// and optionally `k` (1 to 20, default 5); it gets the hits (text cut to
/// 2 000 characters each). Read-only.
pub fn retriever_tool(
    retriever: Arc<dyn Retriever>,
    name: impl Into<String>,
    description: impl Into<String>,
) -> RetrieverTool {
    let schema = json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "What to look for, in words."},
            "k": {"type": "integer", "minimum": 1, "maximum": MAX_TOOL_HITS, "description": "How many results (default 5)."}
        },
        "required": ["query"]
    });
    RetrieverTool {
        spec: ToolSpec::new(name, description, schema)
            .with_annotations(ToolAnnotations::read_only()),
        retriever,
    }
}

impl Tool for RetrieverTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call(
        &self,
        args: Map<String, Value>,
        _ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let k = args
                .get("k")
                .and_then(Value::as_u64)
                .map_or(5, |k| (k as usize).clamp(1, MAX_TOOL_HITS));
            let hits =
                self.retriever.retrieve(query, k).await.map_err(|e| {
                    ToolError::new(format!("search failed ({})", e.code())).retryable()
                })?;
            Ok(ToolOutput::Json(Value::Array(
                hits.iter().map(|h| h.to_json(TOOL_TEXT_CHARS)).collect(),
            )))
        })
    }
}

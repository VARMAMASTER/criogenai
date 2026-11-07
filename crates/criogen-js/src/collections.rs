//! Collections (design 0025): opened over a storage, named to TypeScript by
//! a handle; agents take them as search tools by handle
//! (`config.collections`). Reached through `NativeStorage.op`, so both
//! engines have them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use criogen_agent::AgentBuilder;
use criogen_agent::retrieval::{Collection, Document, IndexKind, RetrievalError};
use criogen_storage::Storage;
use criogen_storage::vector::HnswConfig;
use serde_json::{Map, Value, json};

use crate::config::embedder;
use crate::{Failure, Result, bad, failure, lock, text};

fn collections() -> &'static Mutex<HashMap<u32, Collection>> {
    static COLLECTIONS: OnceLock<Mutex<HashMap<u32, Collection>>> = OnceLock::new();
    COLLECTIONS.get_or_init(Mutex::default)
}

fn collection(payload: &Value) -> Result<Collection> {
    let handle = payload["handle"].as_u64().unwrap_or_default() as u32;
    lock(collections())
        .get(&handle)
        .cloned()
        .ok_or_else(|| bad("the collection is closed"))
}

fn from_retrieval(error: RetrievalError) -> Failure {
    failure(error.code(), error.to_string(), None)
}

/// `collection_open` (`{name, embedder, index, efSearch}` → `{handle,
/// count}`), `collection_add` (`{handle, documents}`), `collection_search`
/// (`{handle, query, k, where}` → hits), `collection_remove` (`{handle,
/// id}`), `collection_reload`, `collection_count`, `collection_close`.
pub(crate) async fn operation(storage: Storage, op: &str, payload: Value) -> Result<Value> {
    match op {
        "collection_open" => {
            let kind = match text(&payload, "index") {
                Some("hnsw") => {
                    let mut config = HnswConfig::default();
                    if let Some(ef) = payload["efSearch"].as_u64() {
                        config = config.ef_search(usize::try_from(ef).unwrap_or(usize::MAX));
                    }
                    IndexKind::Hnsw(config)
                }
                _ => IndexKind::Flat,
            };
            let opened = Collection::open_with(
                storage.memory().clone(),
                text(&payload, "name").unwrap_or_default(),
                embedder(&payload["embedder"])?,
                kind,
            )
            .await
            .map_err(from_retrieval)?;
            static NEXT: AtomicU32 = AtomicU32::new(1);
            let handle = NEXT.fetch_add(1, Ordering::Relaxed);
            let count = opened.len().await;
            lock(collections()).insert(handle, opened);
            Ok(json!({ "handle": handle, "count": count }))
        }
        "collection_add" => {
            let c = collection(&payload)?;
            let mut docs = Vec::new();
            for d in payload["documents"].as_array().cloned().unwrap_or_default() {
                let mut doc = Document::new(
                    text(&d, "id").unwrap_or_default(),
                    text(&d, "text").unwrap_or_default(),
                );
                for (k, v) in d["metadata"].as_object().cloned().unwrap_or_default() {
                    doc = doc.meta(k, v);
                }
                docs.push(doc);
            }
            c.add(docs).await.map_err(from_retrieval)?;
            Ok(json!(c.len().await))
        }
        "collection_search" => {
            let c = collection(&payload)?;
            let filter: Map<String, Value> =
                payload["where"].as_object().cloned().unwrap_or_default();
            let k = payload["k"]
                .as_u64()
                .map_or(5, |k| usize::try_from(k).unwrap_or(5));
            let hits = c
                .search_where(text(&payload, "query").unwrap_or_default(), k, &filter)
                .await
                .map_err(from_retrieval)?;
            Ok(Value::Array(
                hits.iter()
                    .map(|h| json!({"id": h.id, "text": h.text, "metadata": h.metadata, "score": h.score}))
                    .collect(),
            ))
        }
        "collection_remove" => {
            let c = collection(&payload)?;
            Ok(json!(
                c.remove(text(&payload, "id").unwrap_or_default())
                    .await
                    .map_err(from_retrieval)?
            ))
        }
        "collection_reload" => {
            let c = collection(&payload)?;
            c.reload().await.map_err(from_retrieval)?;
            Ok(json!(c.len().await))
        }
        "collection_count" => Ok(json!(collection(&payload)?.len().await)),
        "collection_close" => {
            let handle = payload["handle"].as_u64().unwrap_or_default() as u32;
            Ok(json!(lock(collections()).remove(&handle).is_some()))
        }
        other => Err(bad(format!("unknown storage operation `{other}`"))),
    }
}

/// Adds the search tools `config.collections` names: `[{handle, name,
/// description}]`.
pub(crate) fn add_collection_tools(
    mut builder: AgentBuilder,
    config: &Value,
) -> Result<AgentBuilder> {
    for spec in config["collections"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let c = collection(&spec)?;
        builder = builder.tool(c.as_tool(
            text(&spec, "name").unwrap_or_default(),
            text(&spec, "description").unwrap_or_default(),
        ));
    }
    Ok(builder)
}

//! What the SQL backends (SQLite, Postgres) share: how a checkpoint is
//! encoded (delta or keyframe, snapshot without a second copy of the
//! state), how it is decoded, and the per-thread state cache. The SQL
//! itself lives with each backend; the stored format is the same.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use agentbuild_core::{PendingWrite, Snapshot, State};
use serde_json::Value;

use crate::checkpoint::{Checkpoint, merge_pending};
use crate::delta::{self, DeltaOp};
use crate::error::{StorageError, StorageErrorKind};
use crate::store::{MemoryItem, Namespace};

/// Default keyframe interval: the full state at least every 25 checkpoints.
pub(crate) const KEYFRAME_INTERVAL: u32 = 25;

/// Remembered states (for computing deltas) before the cache is cleared.
const CACHE_LIMIT: usize = 256;

/// The last state written or read per thread, to compute the next delta
/// without reading it back. Keyed by checkpoint id, which never changes
/// content, so it stays right when other processes write the same thread.
#[derive(Default)]
pub(crate) struct StateCache(Mutex<HashMap<String, Cached>>);

struct Cached {
    id: String,
    state: State,
    depth: u32,
}

impl StateCache {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Cached>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The state of checkpoint `id` of `thread` and its distance from its
    /// keyframe, if it is the one remembered.
    pub(crate) fn get(&self, thread: &str, id: &str) -> Option<(State, u32)> {
        self.map()
            .get(thread)
            .filter(|cached| cached.id == id)
            .map(|cached| (cached.state.clone(), cached.depth))
    }

    pub(crate) fn remember(&self, thread: &str, id: String, state: State, depth: u32) {
        let mut map = self.map();
        if map.len() >= CACHE_LIMIT && !map.contains_key(thread) {
            map.clear();
        }
        map.insert(thread.to_string(), Cached { id, state, depth });
    }

    pub(crate) fn forget(&self, thread: &str) {
        self.map().remove(thread);
    }
}

/// A checkpoint as stored in a row.
pub(crate) struct Encoded {
    /// `delta` or `keyframe`.
    pub kind: &'static str,
    /// Checkpoints since the keyframe (0 for a keyframe).
    pub depth: u32,
    /// Delta ops or the full state, as JSON.
    pub body: String,
    /// The snapshot as JSON, without its state when it equals the
    /// checkpoint's (stored once).
    pub snapshot: Option<String>,
}

/// Encodes `checkpoint` against its parent's state (`parent`: the state and
/// its depth): a delta if the parent is close to a keyframe and the delta is
/// smaller than the state, a keyframe otherwise.
pub(crate) fn encode(
    checkpoint: &Checkpoint,
    parent: Option<&(State, u32)>,
    keyframe_interval: u32,
) -> Result<Encoded, StorageError> {
    let full = serde_json::to_string(&checkpoint.state).map_err(encode_error)?;
    let (kind, depth, body) = match parent {
        Some((before, depth)) if depth + 1 < keyframe_interval => {
            let ops = delta::diff(before, &checkpoint.state);
            let encoded = serde_json::to_string(&ops).map_err(encode_error)?;
            if encoded.len() < full.len() {
                ("delta", depth + 1, encoded)
            } else {
                ("keyframe", 0, full)
            }
        }
        _ => ("keyframe", 0, full),
    };
    let snapshot = match &checkpoint.snapshot {
        Some(snapshot) => {
            let mut value = serde_json::to_value(snapshot).map_err(encode_error)?;
            if snapshot.state == checkpoint.state
                && let Value::Object(map) = &mut value
            {
                map.remove("state");
            }
            Some(value.to_string())
        }
        None => None,
    };
    Ok(Encoded {
        kind,
        depth,
        body,
        snapshot,
    })
}

/// Rebuilds the state of checkpoint `id` from its chain: `(kind, body)` from
/// the keyframe to the checkpoint itself.
pub(crate) fn rebuild(id: &str, chain: &[(String, String)]) -> Result<State, StorageError> {
    let Some(((kind, body), deltas)) = chain.split_first() else {
        return Err(corrupt(format!("checkpoint `{id}` has no rows")));
    };
    if kind != "keyframe" {
        return Err(corrupt(format!(
            "checkpoint `{id}` has no keyframe among its ancestors"
        )));
    }
    let mut state: State = parse(body, "keyframe")?;
    for (_, body) in deltas {
        let ops: Vec<DeltaOp> = parse(body, "delta")?;
        state = delta::apply(state, &ops)?;
    }
    Ok(state)
}

/// A stored snapshot, with the checkpoint's state put back if it was left
/// out, and the task writes added after it in order.
pub(crate) fn decode_snapshot(
    text: &str,
    state: &State,
    writes: Vec<(String, PendingWrite)>,
) -> Result<Snapshot, StorageError> {
    let mut value: Value = parse(text, "snapshot")?;
    if let Value::Object(map) = &mut value
        && !map.contains_key("state")
    {
        map.insert("state".into(), state.clone().into_value());
    }
    let mut snapshot: Snapshot = serde_json::from_value(value)
        .map_err(|err| corrupt(format!("unreadable snapshot: {err}")))?;
    for (node, write) in writes {
        merge_pending(&mut snapshot, &node, write);
    }
    Ok(snapshot)
}

/// A memory item from its columns.
pub(crate) fn memory_item(
    path: &str,
    key: String,
    value: &str,
    created_at_ms: i64,
    updated_at_ms: i64,
    expires_at_ms: Option<i64>,
) -> Result<MemoryItem, StorageError> {
    let namespace = Namespace::parse(path)
        .map_err(|err| corrupt(format!("unreadable memory namespace `{path}`: {err}")))?;
    Ok(MemoryItem {
        namespace,
        key,
        value: parse(value, "memory value")?,
        created_at_ms: created_at_ms as u64,
        updated_at_ms: updated_at_ms as u64,
        expires_at_ms: expires_at_ms.map(|at| at as u64),
        score: None,
    })
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(
    text: &str,
    what: &str,
) -> Result<T, StorageError> {
    serde_json::from_str(text).map_err(|err| corrupt(format!("unreadable {what}: {err}")))
}

/// A `Source` or `CheckpointStatus` as stored (its JSON name).
pub(crate) fn enum_text<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    match serde_json::to_value(value).map_err(encode_error)? {
        Value::String(text) => Ok(text),
        other => Err(corrupt(format!("unexpected enum value {other}"))),
    }
}

pub(crate) fn parse_enum<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StorageError> {
    serde_json::from_value(Value::String(text.to_string()))
        .map_err(|err| corrupt(format!("unknown value `{text}`: {err}")))
}

pub(crate) fn corrupt(message: String) -> StorageError {
    StorageError::new(StorageErrorKind::Corrupt, message)
}

pub(crate) fn conflict(message: String) -> StorageError {
    StorageError::new(StorageErrorKind::Conflict, message)
}

pub(crate) fn encode_error(err: serde_json::Error) -> StorageError {
    corrupt(format!("cannot encode as JSON: {err}"))
}

//! Delta checkpoints (design 0008 §4.1): a checkpoint stores only what
//! changed since its parent, as a list of [`DeltaOp`]s.

use agentbuild_core::State;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{StorageError, StorageErrorKind};

/// One change between two states.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeltaOp {
    /// `key` now holds `value`.
    Set {
        /// The state key.
        key: String,
        /// Its new value.
        value: Value,
    },
    /// `items` were added to the end of the list under `key` (e.g. new
    /// messages). The list before is unchanged.
    Append {
        /// The state key.
        key: String,
        /// The new elements, in order.
        items: Vec<Value>,
    },
    /// `key` was removed.
    Remove {
        /// The state key.
        key: String,
    },
}

/// The changes that turn `old` into `new`. A list that only grew becomes
/// an [`Append`](DeltaOp::Append) of the new elements, so a long chat
/// stores each message once. Keys are visited in sorted order, so equal
/// inputs give equal deltas.
pub fn diff(old: &State, new: &State) -> Vec<DeltaOp> {
    let (old, new) = (old.as_map(), new.as_map());
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut ops = Vec::new();
    for key in keys {
        match (old.get(key), new.get(key)) {
            (Some(before), Some(after)) if before == after => {}
            (Some(Value::Array(before)), Some(Value::Array(after)))
                if after.len() > before.len() && after[..before.len()] == before[..] =>
            {
                ops.push(DeltaOp::Append {
                    key: key.clone(),
                    items: after[before.len()..].to_vec(),
                });
            }
            (_, Some(after)) => ops.push(DeltaOp::Set {
                key: key.clone(),
                value: after.clone(),
            }),
            (Some(_), None) => ops.push(DeltaOp::Remove { key: key.clone() }),
            (None, None) => unreachable!("the key came from one of the maps"),
        }
    }
    ops
}

/// Applies `ops` (from [`diff`]) to `state`. Fails with `storage_corrupt`
/// if an append targets something that is not a list.
pub fn apply(state: State, ops: &[DeltaOp]) -> Result<State, StorageError> {
    let mut map: Map<String, Value> = state.into_map();
    for op in ops {
        match op {
            DeltaOp::Set { key, value } => {
                map.insert(key.clone(), value.clone());
            }
            DeltaOp::Append { key, items } => match map.get_mut(key) {
                Some(Value::Array(list)) => list.extend(items.iter().cloned()),
                _ => {
                    return Err(StorageError::new(
                        StorageErrorKind::Corrupt,
                        format!("delta appends to `{key}`, which is not a list"),
                    ));
                }
            },
            DeltaOp::Remove { key } => {
                map.remove(key);
            }
        }
    }
    Ok(State::from(map))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn state(value: Value) -> State {
        State::try_from(value).unwrap()
    }

    #[test]
    fn appends_sets_and_removes_round_trip() {
        let old = state(json!({"messages": [1, 2], "a": 1, "gone": true, "same": "x"}));
        let new = state(json!({"messages": [1, 2, 3, 4], "a": 2, "same": "x", "new": [5]}));
        let ops = diff(&old, &new);
        assert_eq!(
            serde_json::to_value(&ops).unwrap(),
            json!([
                {"op": "set", "key": "a", "value": 2},
                {"op": "remove", "key": "gone"},
                {"op": "append", "key": "messages", "items": [3, 4]},
                {"op": "set", "key": "new", "value": [5]},
            ])
        );
        assert_eq!(apply(old.clone(), &ops).unwrap(), new);
        assert!(diff(&new, &new).is_empty());
    }

    #[test]
    fn a_rewritten_list_is_set_whole() {
        let old = state(json!({"l": [1, 2, 3]}));
        for after in [json!([1, 9, 3, 4]), json!([1]), json!([])] {
            let new = state(json!({"l": after.clone()}));
            assert_eq!(
                diff(&old, &new),
                [DeltaOp::Set {
                    key: "l".into(),
                    value: after
                }]
            );
            assert_eq!(apply(old.clone(), &diff(&old, &new)).unwrap(), new);
        }
    }

    #[test]
    fn appending_to_a_non_list_is_corrupt() {
        let ops = [DeltaOp::Append {
            key: "a".into(),
            items: vec![json!(1)],
        }];
        let err = apply(state(json!({"a": 1})), &ops).unwrap_err();
        assert_eq!(err.code(), "storage_corrupt");
        assert!(apply(State::new(), &ops).is_err());
    }
}

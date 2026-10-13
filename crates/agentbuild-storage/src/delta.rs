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

    /// A small deterministic generator (xorshift), so failures reproduce.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn value(&mut self, depth: u32) -> Value {
            match self.below(if depth > 2 { 4 } else { 6 }) {
                0 => Value::Null,
                1 => json!(self.below(1000)),
                2 => json!(format!("s{}", self.below(50))),
                3 => json!(self.below(2) == 1),
                4 => Value::Array((0..self.below(4)).map(|_| self.value(depth + 1)).collect()),
                _ => {
                    let mut map = serde_json::Map::new();
                    for _ in 0..self.below(4) {
                        map.insert(format!("k{}", self.below(5)), self.value(depth + 1));
                    }
                    Value::Object(map)
                }
            }
        }
    }

    /// Changes a state the way runs do: append to lists (most often), set,
    /// remove, rewrite or shrink a list.
    fn mutate(rng: &mut Rng, state: &State) -> State {
        let mut map = state.clone().into_map();
        for _ in 0..1 + rng.below(3) {
            let key = format!("key{}", rng.below(6));
            match rng.below(6) {
                0..=2 => {
                    let mut list = match map.remove(&key) {
                        Some(Value::Array(list)) => list,
                        _ => Vec::new(),
                    };
                    for _ in 0..1 + rng.below(3) {
                        list.push(rng.value(0));
                    }
                    map.insert(key, Value::Array(list));
                }
                3 => {
                    map.insert(key, rng.value(0));
                }
                4 => {
                    map.remove(&key);
                }
                _ => {
                    if let Some(Value::Array(list)) = map.get_mut(&key) {
                        let keep = rng.below(list.len() as u64 + 1) as usize;
                        list.truncate(keep);
                        if rng.below(2) == 1 {
                            list.push(rng.value(0));
                        }
                    }
                }
            }
        }
        State::from(map)
    }

    #[test]
    fn random_histories_replay_exactly() {
        for seed in 1..=200u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut keyframe = State::new();
            let mut replayed = State::new();
            let mut state = State::new();
            for _ in 0..40 {
                let next = mutate(&mut rng, &state);
                let ops = diff(&state, &next);
                // Ops survive storage as JSON.
                let stored: Vec<DeltaOp> =
                    serde_json::from_str(&serde_json::to_string(&ops).unwrap()).unwrap();
                assert_eq!(stored, ops, "seed {seed}");
                replayed = apply(replayed, &stored).unwrap();
                assert_eq!(replayed, next, "seed {seed}");
                // Deltas between equal states are empty; applying one delta
                // to its own base gives the new state.
                assert!(diff(&next, &next).is_empty());
                assert_eq!(apply(state.clone(), &ops).unwrap(), next, "seed {seed}");
                state = next;
            }
            // A chain from an empty keyframe ends at the same state.
            keyframe = apply(keyframe, &diff(&State::new(), &state)).unwrap();
            assert_eq!(keyframe, state, "seed {seed}");
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

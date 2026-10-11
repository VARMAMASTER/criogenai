use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::RunErrorKind;
use crate::state::{State, Update};

/// How writes to a state key are combined. Declared per key with
/// [`WorkflowBuilder::reducer`](crate::WorkflowBuilder::reducer); undeclared
/// keys use [`Reducer::Replace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reducer {
    /// A write replaces the value. Two writes in one step are an
    /// `update_conflict`.
    Replace,
    /// The value is a list; each write is a list appended to it, in node
    /// declaration order.
    Append,
    /// The value is a list of objects with a string `id`. A write with a new id
    /// appends, an existing id replaces in place, and `{"id": …, "$remove": true}`
    /// removes. Two writes to the same id in one step are an `update_conflict`.
    UpsertById,
}

/// Wrapping a value as `{"$replace": [...]}` replaces the whole list of an
/// `append` or `upsert_by_id` key.
pub(crate) const REPLACE_MARKER: &str = "$replace";
const REMOVE_MARKER: &str = "$remove";

/// Merges the updates of one step into `state`, in the order given (node
/// declaration order). On error `state` may be partially modified, so callers
/// merge into a copy.
pub(crate) fn merge(
    state: &mut State,
    reducers: &HashMap<String, Reducer>,
    updates: Vec<(String, Update)>,
) -> Result<(), RunErrorKind> {
    // Group writes by key, keeping writer order. BTreeMap keeps error reporting
    // deterministic when several keys conflict.
    let mut by_key: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();
    for (node, update) in updates {
        for (key, value) in update.into_map() {
            by_key.entry(key).or_default().push((node.clone(), value));
        }
    }

    for (key, writes) in by_key {
        let reducer = reducers.get(&key).copied().unwrap_or(Reducer::Replace);
        if reducer == Reducer::Replace || writes.iter().any(|(_, v)| is_replace(v)) {
            if writes.len() > 1 {
                return Err(RunErrorKind::UpdateConflict {
                    key,
                    first: writes[0].0.clone(),
                    second: writes[1].0.clone(),
                });
            }
            let (node, value) = writes.into_iter().next().expect("one write");
            let value = match (reducer, value) {
                (Reducer::Replace, v) => v,
                (_, v) => unwrap_replace(&node, &key, v)?,
            };
            state.insert(key, value);
            continue;
        }

        let current = match state.as_map().get(&key) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => {
                return Err(invalid(&writes[0].0, &key, "stored value is not a list"));
            }
        };
        let merged = match reducer {
            Reducer::Append => append(&key, current, writes)?,
            Reducer::UpsertById => upsert(&key, current, writes)?,
            Reducer::Replace => unreachable!("handled above"),
        };
        state.insert(key, Value::Array(merged));
    }
    Ok(())
}

fn append(
    key: &str,
    mut current: Vec<Value>,
    writes: Vec<(String, Value)>,
) -> Result<Vec<Value>, RunErrorKind> {
    for (node, value) in writes {
        match value {
            Value::Array(items) => current.extend(items),
            _ => return Err(invalid(&node, key, "append expects a list")),
        }
    }
    Ok(current)
}

fn upsert(
    key: &str,
    current: Vec<Value>,
    writes: Vec<(String, Value)>,
) -> Result<Vec<Value>, RunErrorKind> {
    let mut items: Vec<Option<Value>> = current.into_iter().map(Some).collect();
    let mut position: HashMap<String, usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(id) = item.as_ref().and_then(item_id) {
            position.insert(id.to_string(), i);
        }
    }
    // id -> node that touched it in this step
    let mut touched: HashMap<String, String> = HashMap::new();

    for (node, value) in writes {
        let Value::Array(entries) = value else {
            return Err(invalid(&node, key, "upsert_by_id expects a list"));
        };
        for entry in entries {
            let Some(id) = item_id(&entry).map(str::to_string) else {
                return Err(invalid(
                    &node,
                    key,
                    "upsert_by_id items must be objects with a string `id`",
                ));
            };
            if let Some(first) = touched.insert(id.clone(), node.clone()) {
                return Err(RunErrorKind::UpdateConflict {
                    key: format!("{key}[id={id}]"),
                    first,
                    second: node,
                });
            }
            let remove = entry.get(REMOVE_MARKER) == Some(&Value::Bool(true));
            match (position.get(&id).copied(), remove) {
                (Some(i), true) => items[i] = None,
                (Some(i), false) => items[i] = Some(entry),
                (None, true) => {} // removing an absent id is a no-op
                (None, false) => {
                    position.insert(id, items.len());
                    items.push(Some(entry));
                }
            }
        }
    }
    Ok(items.into_iter().flatten().collect())
}

fn item_id(item: &Value) -> Option<&str> {
    item.as_object()?.get("id")?.as_str()
}

fn is_replace(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.len() == 1 && map.contains_key(REPLACE_MARKER))
}

fn unwrap_replace(node: &str, key: &str, value: Value) -> Result<Value, RunErrorKind> {
    let Value::Object(mut map): Value = value else {
        unreachable!("checked by is_replace")
    };
    match map.remove(REPLACE_MARKER) {
        Some(list @ Value::Array(_)) => Ok(list),
        _ => Err(invalid(node, key, "`$replace` expects a list")),
    }
}

fn invalid(node: &str, key: &str, message: &str) -> RunErrorKind {
    RunErrorKind::InvalidValue {
        node: node.to_string(),
        key: key.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(
        initial: Value,
        reducers: &[(&str, Reducer)],
        updates: Vec<(&str, Value)>,
    ) -> Result<Value, RunErrorKind> {
        let mut state = State::try_from(initial).unwrap();
        let reducers = reducers.iter().map(|(k, r)| (k.to_string(), *r)).collect();
        let updates = updates
            .into_iter()
            .map(|(n, u)| (n.to_string(), Update::try_from(u).unwrap()))
            .collect();
        merge(&mut state, &reducers, updates)?;
        Ok(state.into_value())
    }

    #[test]
    fn append_concatenates_in_writer_order() {
        let out = run(
            json!({"log": [0]}),
            &[("log", Reducer::Append)],
            vec![("a", json!({"log": [1, 2]})), ("b", json!({"log": [3]}))],
        )
        .unwrap();
        assert_eq!(out, json!({"log": [0, 1, 2, 3]}));
    }

    #[test]
    fn append_starts_from_missing_key() {
        let out = run(
            json!({}),
            &[("log", Reducer::Append)],
            vec![("a", json!({"log": ["x"]}))],
        )
        .unwrap();
        assert_eq!(out, json!({"log": ["x"]}));
    }

    #[test]
    fn append_rejects_non_list_write() {
        let err = run(
            json!({}),
            &[("log", Reducer::Append)],
            vec![("a", json!({"log": 1}))],
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid_value");
    }

    #[test]
    fn upsert_appends_replaces_and_removes() {
        let out = run(
            json!({"m": [{"id": "1", "t": "a"}, {"id": "2", "t": "b"}, {"id": "3", "t": "c"}]}),
            &[("m", Reducer::UpsertById)],
            vec![(
                "n",
                json!({"m": [{"id": "2", "t": "B"}, {"id": "3", "$remove": true}, {"id": "4", "t": "d"}]}),
            )],
        )
        .unwrap();
        assert_eq!(
            out,
            json!({"m": [{"id": "1", "t": "a"}, {"id": "2", "t": "B"}, {"id": "4", "t": "d"}]})
        );
    }

    #[test]
    fn upsert_same_id_twice_in_a_step_conflicts() {
        let err = run(
            json!({}),
            &[("m", Reducer::UpsertById)],
            vec![
                ("a", json!({"m": [{"id": "x"}]})),
                ("b", json!({"m": [{"id": "x"}]})),
            ],
        )
        .unwrap_err();
        assert_eq!(
            err,
            RunErrorKind::UpdateConflict {
                key: "m[id=x]".into(),
                first: "a".into(),
                second: "b".into()
            }
        );
    }

    #[test]
    fn upsert_different_ids_from_parallel_nodes_merge() {
        let out = run(
            json!({}),
            &[("m", Reducer::UpsertById)],
            vec![
                ("a", json!({"m": [{"id": "1"}]})),
                ("b", json!({"m": [{"id": "2"}]})),
            ],
        )
        .unwrap();
        assert_eq!(out, json!({"m": [{"id": "1"}, {"id": "2"}]}));
    }

    #[test]
    fn upsert_requires_ids() {
        let err = run(
            json!({}),
            &[("m", Reducer::UpsertById)],
            vec![("a", json!({"m": [{"t": 1}]}))],
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid_value");
    }

    #[test]
    fn replace_marker_overwrites_list() {
        let out = run(
            json!({"m": [{"id": "1"}, {"id": "2"}]}),
            &[("m", Reducer::UpsertById)],
            vec![("summarize", json!({"m": {"$replace": [{"id": "s"}]}}))],
        )
        .unwrap();
        assert_eq!(out, json!({"m": [{"id": "s"}]}));
    }

    #[test]
    fn replace_marker_with_another_writer_conflicts() {
        let err = run(
            json!({}),
            &[("m", Reducer::Append)],
            vec![
                ("a", json!({"m": {"$replace": []}})),
                ("b", json!({"m": [1]})),
            ],
        )
        .unwrap_err();
        assert_eq!(err.code(), "update_conflict");
    }

    #[test]
    fn plain_keys_keep_replace_semantics() {
        let err = run(
            json!({}),
            &[],
            vec![("a", json!({"k": 1})), ("b", json!({"k": 2}))],
        )
        .unwrap_err();
        assert_eq!(err.code(), "update_conflict");
        let out = run(json!({"k": 0}), &[], vec![("a", json!({"k": 1}))]).unwrap();
        assert_eq!(out, json!({"k": 1}));
    }
}

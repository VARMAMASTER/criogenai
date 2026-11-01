use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::NotAnObject;

/// The data shared by all nodes in a run: a JSON object.
///
/// State is JSON so that it can cross into Python and JavaScript without a
/// per-language schema. Nodes never mutate it directly; they return an
/// [`Update`] that the runtime merges between steps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct State(Map<String, Value>);

impl State {
    /// Creates an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert, convenient for constructing inputs.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.insert(key, value);
        self
    }

    /// Returns the value stored under `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Sets `key`, returning the previous value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        self.0.insert(key.into(), value.into())
    }

    /// Borrows the underlying JSON object.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Converts into the underlying JSON object.
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }

    /// Converts into a `serde_json::Value::Object`.
    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }

    #[cfg(test)]
    pub(crate) fn apply(&mut self, update: Update) {
        self.0.extend(update.0);
    }
}

impl From<Map<String, Value>> for State {
    fn from(map: Map<String, Value>) -> Self {
        Self(map)
    }
}

impl TryFrom<Value> for State {
    type Error = NotAnObject;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(map) => Ok(Self(map)),
            other => Err(NotAnObject::of(&other)),
        }
    }
}

/// The keys a node wants to change. Each key replaces the stored value.
///
/// Writing `null` stores `null`; it does not delete the key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Update(Map<String, Value>);

impl Update {
    /// Creates an empty update (a node that changes nothing).
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style set.
    pub fn set(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.insert(key, value);
        self
    }

    /// Sets `key`, returning the previous value in this update.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        self.0.insert(key.into(), value.into())
    }

    /// Returns the value this update sets for `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Iterates over the keys this update writes.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    /// Returns `true` if the update writes no keys.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Converts into the underlying JSON object.
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }
}

impl From<Map<String, Value>> for Update {
    fn from(map: Map<String, Value>) -> Self {
        Self(map)
    }
}

impl TryFrom<Value> for Update {
    type Error = NotAnObject;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(map) => Ok(Self(map)),
            other => Err(NotAnObject::of(&other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn apply_overwrites_only_written_keys() {
        let mut state = State::new().with("a", 1).with("b", 2);
        state.apply(Update::new().set("b", 20).set("c", json!([3])));
        assert_eq!(state.into_value(), json!({"a": 1, "b": 20, "c": [3]}));
    }

    #[test]
    fn null_is_stored_not_deleted() {
        let mut state = State::new().with("a", 1);
        state.apply(Update::new().set("a", Value::Null));
        assert_eq!(state.get("a"), Some(&Value::Null));
    }

    #[test]
    fn try_from_rejects_non_objects() {
        assert!(State::try_from(json!({"x": 1})).is_ok());
        let err = State::try_from(json!([1, 2])).unwrap_err();
        assert_eq!(err.to_string(), "expected a JSON object, got an array");
        assert!(Update::try_from(json!("s")).is_err());
    }

    #[test]
    fn serializes_as_plain_object() {
        let state = State::new().with("k", "v");
        assert_eq!(serde_json::to_string(&state).unwrap(), r#"{"k":"v"}"#);
    }
}

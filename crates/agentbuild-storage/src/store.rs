//! Long-term memory (design 0008 §2.3, §13): facts that outlive a thread,
//! kept under namespaces such as `users/42`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentbuild_core::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::checkpoint::now_ms;
use crate::error::{StorageError, StorageErrorKind};

/// The largest value [`MemoryStore::put`] accepts, as JSON text (1 MiB).
/// Long content belongs in files or a database, not in memory.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// The longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 256;

/// Where memory items live: a path of segments such as `users/42`.
///
/// Segments are non-empty and contain no `/` or control characters. The
/// empty namespace ([`Namespace::root`]) holds no items itself; it is the
/// prefix of every namespace, for [`MemoryStore::search`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct Namespace(Vec<String>);

impl Namespace {
    /// A namespace from its segments, e.g. `["users", "42"]`.
    pub fn new<I, S>(segments: I) -> Result<Self, StorageError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let segments: Vec<String> = segments.into_iter().map(Into::into).collect();
        for segment in &segments {
            if segment.is_empty() || segment.contains('/') || segment.chars().any(char::is_control)
            {
                return Err(invalid(format!(
                    "invalid namespace segment {segment:?}: segments are non-empty, without `/` \
                     or control characters"
                )));
            }
        }
        Ok(Self(segments))
    }

    /// A namespace from a path such as `users/42`. `""` is the root.
    pub fn parse(path: &str) -> Result<Self, StorageError> {
        if path.is_empty() {
            return Ok(Self::root());
        }
        Self::new(path.split('/'))
    }

    /// The empty namespace: the prefix of every namespace.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// This namespace with one more segment.
    pub fn child(&self, segment: impl Into<String>) -> Result<Self, StorageError> {
        let mut segments = self.0.clone();
        segments.push(segment.into());
        Self::new(segments)
    }

    /// The segments.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// `true` for the root.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The segments joined with `/`.
    pub fn path(&self) -> String {
        self.0.join("/")
    }

    /// `true` if `other` is this namespace or inside it.
    pub fn contains(&self, other: &Namespace) -> bool {
        other.0.starts_with(&self.0)
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path())
    }
}

impl TryFrom<Vec<String>> for Namespace {
    type Error = StorageError;

    fn try_from(segments: Vec<String>) -> Result<Self, Self::Error> {
        Self::new(segments)
    }
}

impl From<Namespace> for Vec<String> {
    fn from(namespace: Namespace) -> Self {
        namespace.0
    }
}

/// One stored fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryItem {
    /// Where it lives.
    pub namespace: Namespace,
    /// Its key in the namespace.
    pub key: String,
    /// Its value (any JSON).
    pub value: Value,
    /// When it was first written (Unix milliseconds).
    pub created_at_ms: u64,
    /// When it was last written.
    pub updated_at_ms: u64,
    /// When it expires, if it has a time to live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    /// How well it matched a search's text, from 0 to 1 (search results
    /// with text only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// What [`MemoryStore::search`] looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchQuery {
    /// Words to match (keyword search). `None`: every item, newest first.
    pub text: Option<String>,
    /// The most items to return. Default 10.
    pub limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: None,
            limit: 10,
        }
    }
}

impl SearchQuery {
    /// Every item, newest first, at most 10.
    pub fn new() -> Self {
        Self::default()
    }

    /// Items matching the words of `text`, best first.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// At most `limit` items.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// Where long-term memory is kept (design 0008 §7). Expired items are
/// invisible at once and removed by [`prune_expired`](Self::prune_expired).
///
/// **Search** is by keywords: an item matches when its key or the text in
/// its value contains the query's words (common words such as "the" or
/// "my" are ignored; a word also matches longer words it starts, from
/// three letters). Items are ranked by the share of words matched, then by
/// last update. Every backend ranks the same way.
pub trait MemoryStore: Send + Sync + 'static {
    /// Saves `value` under `key` in `namespace`, replacing what was there.
    /// `ttl`: removed that long from now; `None` keeps it until deleted.
    /// Fails with `invalid_value` for the root namespace, an empty or long
    /// key, or a value over [`MAX_VALUE_BYTES`].
    fn put<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
        value: Value,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<(), StorageError>>;

    /// The item, unless missing or expired.
    fn get<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<MemoryItem>, StorageError>>;

    /// Items in `prefix` and the namespaces inside it, ranked (see the
    /// trait docs).
    fn search<'a>(
        &'a self,
        prefix: &'a Namespace,
        query: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<MemoryItem>, StorageError>>;

    /// Deletes the item. `true` if it existed and had not expired.
    fn delete<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
    ) -> BoxFuture<'a, Result<bool, StorageError>>;

    /// Deletes every item in `prefix` and the namespaces inside it (for
    /// example everything about one user). Returns how many rows went.
    /// The root is refused (`invalid_value`).
    fn delete_namespace<'a>(
        &'a self,
        prefix: &'a Namespace,
    ) -> BoxFuture<'a, Result<u64, StorageError>>;

    /// Removes expired items. Returns how many.
    fn prune_expired(&self) -> BoxFuture<'_, Result<u64, StorageError>>;
}

/// A memory store limited to one namespace: what an agent's tools see.
/// Cheap to clone.
#[derive(Clone)]
pub struct ScopedMemory {
    store: Arc<dyn MemoryStore>,
    namespace: Namespace,
}

impl fmt::Debug for ScopedMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedMemory")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl ScopedMemory {
    /// `namespace` of `store`.
    pub fn new(store: Arc<dyn MemoryStore>, namespace: Namespace) -> Self {
        Self { store, namespace }
    }

    /// The namespace.
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// The whole store.
    pub fn store(&self) -> &Arc<dyn MemoryStore> {
        &self.store
    }

    /// [`MemoryStore::put`] in this namespace.
    pub async fn put(
        &self,
        key: &str,
        value: Value,
        ttl: Option<Duration>,
    ) -> Result<(), StorageError> {
        self.store.put(&self.namespace, key, value, ttl).await
    }

    /// [`MemoryStore::get`] in this namespace.
    pub async fn get(&self, key: &str) -> Result<Option<MemoryItem>, StorageError> {
        self.store.get(&self.namespace, key).await
    }

    /// [`MemoryStore::search`] in this namespace (and those inside it).
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryItem>, StorageError> {
        self.store.search(&self.namespace, query).await
    }

    /// [`MemoryStore::delete`] in this namespace.
    pub async fn delete(&self, key: &str) -> Result<bool, StorageError> {
        self.store.delete(&self.namespace, key).await
    }

    /// Deletes everything in this namespace.
    pub async fn clear(&self) -> Result<u64, StorageError> {
        self.store.delete_namespace(&self.namespace).await
    }
}

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::new(StorageErrorKind::InvalidValue, message)
}

/// Checks a `put` and returns the value as JSON text.
pub(crate) fn check_put(
    namespace: &Namespace,
    key: &str,
    value: &Value,
) -> Result<String, StorageError> {
    if namespace.is_root() {
        return Err(invalid("memory items need a namespace (not the root)"));
    }
    check_key(key)?;
    let text = serde_json::to_string(value)
        .map_err(|err| invalid(format!("the value cannot be stored: {err}")))?;
    if text.len() > MAX_VALUE_BYTES {
        return Err(invalid(format!(
            "the value of `{key}` is {} bytes; memory items hold at most {MAX_VALUE_BYTES}",
            text.len()
        )));
    }
    Ok(text)
}

pub(crate) fn check_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(invalid(format!(
            "memory keys are 1 to {MAX_KEY_BYTES} bytes, got {}",
            key.len()
        )));
    }
    Ok(())
}

/// `delete_namespace` refuses the root.
pub(crate) fn check_prefix(prefix: &Namespace) -> Result<(), StorageError> {
    if prefix.is_root() {
        return Err(invalid(
            "refusing to delete the root namespace (every item); name a namespace",
        ));
    }
    Ok(())
}

/// When an item written now with `ttl` expires.
pub(crate) fn expiry(now: u64, ttl: Option<Duration>) -> Option<u64> {
    ttl.map(|ttl| now.saturating_add(ttl.as_millis().min(u128::from(u64::MAX)) as u64))
}

pub(crate) fn is_live(expires_at_ms: Option<u64>, now: u64) -> bool {
    expires_at_ms.is_none_or(|at| at > now)
}

/// Words ignored in queries: they match nearly everything.
const STOP_WORDS: &[&str] = &[
    "a", "about", "am", "an", "and", "any", "are", "as", "at", "be", "can", "could", "did", "do",
    "does", "for", "from", "have", "how", "i", "if", "in", "is", "it", "its", "me", "my", "of",
    "on", "or", "our", "please", "should", "so", "that", "the", "their", "them", "there", "they",
    "this", "to", "us", "was", "we", "were", "what", "when", "where", "which", "who", "why",
    "will", "with", "would", "you", "your",
];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// The searchable words of an item: its key and the strings and numbers in
/// its value, each once, as ` word word … ` (spaces around every word, so
/// ` term` finds words that start with the term).
pub(crate) fn search_text(key: &str, value: &Value) -> String {
    fn collect(value: &Value, into: &mut BTreeSet<String>) {
        match value {
            Value::String(text) => into.extend(words(text)),
            Value::Number(n) => into.extend(words(&n.to_string())),
            Value::Array(list) => list.iter().for_each(|v| collect(v, into)),
            Value::Object(map) => map.values().for_each(|v| collect(v, into)),
            Value::Bool(_) | Value::Null => {}
        }
    }
    let mut all: BTreeSet<String> = words(key).collect();
    collect(value, &mut all);
    let mut text = String::from(" ");
    for word in all {
        text.push_str(&word);
        text.push(' ');
    }
    text
}

/// The words of a query that count (no stop words, each once, at most 32).
pub(crate) fn query_terms(query: &SearchQuery) -> Vec<String> {
    let mut seen = BTreeSet::new();
    query
        .text
        .as_deref()
        .map(|text| {
            words(text)
                .filter(|w| !STOP_WORDS.contains(&w.as_str()))
                .filter(|w| seen.insert(w.clone()))
                .take(32)
                .collect()
        })
        .unwrap_or_default()
}

/// How well `search_text` matches `terms`: each term scores 1 for a whole
/// word, 0.5 for the start of a longer word (terms of 3+ letters); the
/// score is the mean.
pub(crate) fn score(terms: &[String], search_text: &str) -> f64 {
    let total: f64 = terms
        .iter()
        .map(|term| {
            if search_text.contains(&format!(" {term} ")) {
                1.0
            } else if term.chars().count() >= 3 && search_text.contains(&format!(" {term}")) {
                0.5
            } else {
                0.0
            }
        })
        .sum();
    total / terms.len() as f64
}

/// Orders and cuts search candidates. With terms: matching items only,
/// best first; without: newest first. Ties: newest, then namespace path
/// and key (byte order, as SQL sorts them).
pub(crate) fn rank(
    candidates: Vec<(MemoryItem, String)>,
    terms: &[String],
    limit: usize,
) -> Vec<MemoryItem> {
    let mut items: Vec<MemoryItem> = candidates
        .into_iter()
        .filter_map(|(mut item, text)| {
            if terms.is_empty() {
                return Some(item);
            }
            let score = score(terms, &text);
            (score > 0.0).then(|| {
                item.score = Some(score);
                item
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .total_cmp(&a.score.unwrap_or(0.0))
            .then(b.updated_at_ms.cmp(&a.updated_at_ms))
            .then_with(|| a.namespace.path().cmp(&b.namespace.path()))
            .then_with(|| a.key.cmp(&b.key))
    });
    items.truncate(limit);
    items
}

/// Keeps memory in process memory. Lost when the process exits: for tests,
/// scripts and the default when no storage is configured.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    items: Mutex<BTreeMap<(Namespace, String), Stored>>,
}

#[derive(Debug, Clone)]
struct Stored {
    item: MemoryItem,
    search_text: String,
}

impl InMemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn items(&self) -> MutexGuard<'_, BTreeMap<(Namespace, String), Stored>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl MemoryStore for InMemoryStore {
    fn put<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
        value: Value,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        let result = check_put(namespace, key, &value).map(|_| {
            let now = now_ms();
            let mut items = self.items();
            let entry = (namespace.clone(), key.to_string());
            // Overwriting an expired item starts a new one.
            let created_at_ms = items
                .get(&entry)
                .filter(|s| is_live(s.item.expires_at_ms, now))
                .map_or(now, |s| s.item.created_at_ms);
            let search_text = search_text(key, &value);
            items.insert(
                entry,
                Stored {
                    item: MemoryItem {
                        namespace: namespace.clone(),
                        key: key.to_string(),
                        value,
                        created_at_ms,
                        updated_at_ms: now,
                        expires_at_ms: expiry(now, ttl),
                        score: None,
                    },
                    search_text,
                },
            );
        });
        Box::pin(async move { result })
    }

    fn get<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<MemoryItem>, StorageError>> {
        let now = now_ms();
        let found = self
            .items()
            .get(&(namespace.clone(), key.to_string()))
            .filter(|s| is_live(s.item.expires_at_ms, now))
            .map(|s| s.item.clone());
        Box::pin(async move { Ok(found) })
    }

    fn search<'a>(
        &'a self,
        prefix: &'a Namespace,
        query: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<MemoryItem>, StorageError>> {
        let now = now_ms();
        let candidates: Vec<(MemoryItem, String)> = self
            .items()
            .values()
            .filter(|s| prefix.contains(&s.item.namespace) && is_live(s.item.expires_at_ms, now))
            .map(|s| (s.item.clone(), s.search_text.clone()))
            .collect();
        let found = rank(candidates, &query_terms(query), query.limit);
        Box::pin(async move { Ok(found) })
    }

    fn delete<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
    ) -> BoxFuture<'a, Result<bool, StorageError>> {
        let now = now_ms();
        let removed = self
            .items()
            .remove(&(namespace.clone(), key.to_string()))
            .is_some_and(|s| is_live(s.item.expires_at_ms, now));
        Box::pin(async move { Ok(removed) })
    }

    fn delete_namespace<'a>(
        &'a self,
        prefix: &'a Namespace,
    ) -> BoxFuture<'a, Result<u64, StorageError>> {
        let result = check_prefix(prefix).map(|()| {
            let mut items = self.items();
            let before = items.len();
            items.retain(|(namespace, _), _| !prefix.contains(namespace));
            (before - items.len()) as u64
        });
        Box::pin(async move { result })
    }

    fn prune_expired(&self) -> BoxFuture<'_, Result<u64, StorageError>> {
        let now = now_ms();
        let mut items = self.items();
        let before = items.len();
        items.retain(|_, s| is_live(s.item.expires_at_ms, now));
        let removed = (before - items.len()) as u64;
        Box::pin(async move { Ok(removed) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ns(path: &str) -> Namespace {
        Namespace::parse(path).unwrap()
    }

    #[test]
    fn namespaces_are_checked_and_nest() {
        assert_eq!(ns("users/42").segments(), ["users", "42"]);
        assert_eq!(ns("users/42").to_string(), "users/42");
        assert!(ns("").is_root());
        for bad in ["users//42", "/users", "users/", "a/\nb"] {
            let err = Namespace::parse(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_value", "{bad:?}");
        }
        assert!(Namespace::new(["a/b"]).is_err());
        assert!(ns("users").contains(&ns("users/42")));
        assert!(ns("users/42").contains(&ns("users/42")));
        assert!(!ns("users/4").contains(&ns("users/42")));
        assert!(Namespace::root().contains(&ns("x")));
        assert_eq!(ns("users").child("7").unwrap(), ns("users/7"));
        // JSON: a list of segments, checked when read.
        assert_eq!(serde_json::to_value(ns("a/b")).unwrap(), json!(["a", "b"]));
        assert!(serde_json::from_value::<Namespace>(json!(["a", ""])).is_err());
    }

    #[test]
    fn search_text_holds_the_words_of_key_and_value() {
        let text = search_text(
            "diet_preference",
            &json!({"value": "Vegetarian, no nuts!", "since": 2024, "ok": true, "tags": ["Food"]}),
        );
        assert_eq!(text, " 2024 diet food no nuts preference vegetarian ");
    }

    #[test]
    fn scores_count_whole_words_and_word_starts() {
        let text = search_text("diet", &json!("vegetarian, allergic to peanuts"));
        let terms = |q: &str| query_terms(&SearchQuery::new().text(q));
        assert_eq!(terms("What is my DIET?"), ["diet"]);
        assert_eq!(score(&terms("diet"), &text), 1.0);
        assert_eq!(score(&terms("vegetarian peanut"), &text), 0.75);
        // Two letters are too short to match a word start.
        assert_eq!(score(&terms("ve"), &text), 0.0);
        assert_eq!(score(&terms("pizza"), &text), 0.0);
        assert!(terms("what is it").is_empty());
    }

    #[tokio::test]
    async fn in_memory_store_follows_the_contract() {
        let store = InMemoryStore::new();
        let user = ns("users/42");
        store
            .put(&user, "diet", json!("vegetarian"), None)
            .await
            .unwrap();
        let item = store.get(&user, "diet").await.unwrap().unwrap();
        assert_eq!(item.value, json!("vegetarian"));
        assert_eq!(item.created_at_ms, item.updated_at_ms);
        assert!(store.get(&ns("users/4"), "diet").await.unwrap().is_none());

        // Expired at once: invisible, then pruned.
        store
            .put(&user, "mood", json!("tired"), Some(Duration::ZERO))
            .await
            .unwrap();
        assert!(store.get(&user, "mood").await.unwrap().is_none());
        assert!(!store.delete(&user, "mood").await.unwrap());
        store
            .put(&user, "mood", json!("tired"), Some(Duration::ZERO))
            .await
            .unwrap();
        assert_eq!(store.prune_expired().await.unwrap(), 1);

        assert!(store.delete(&user, "diet").await.unwrap());
        assert!(!store.delete(&user, "diet").await.unwrap());

        let root = Namespace::root();
        assert_eq!(
            store
                .put(&root, "k", json!(1), None)
                .await
                .unwrap_err()
                .code(),
            "invalid_value"
        );
        assert_eq!(
            store
                .put(&user, "", json!(1), None)
                .await
                .unwrap_err()
                .code(),
            "invalid_value"
        );
        let big = json!("x".repeat(MAX_VALUE_BYTES));
        assert_eq!(
            store.put(&user, "big", big, None).await.unwrap_err().code(),
            "invalid_value"
        );
        assert_eq!(
            store.delete_namespace(&root).await.unwrap_err().code(),
            "invalid_value"
        );
    }
}

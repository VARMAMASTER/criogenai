//! The audit trail (design 0010): who did what, when, without content, in
//! a hash chain that shows edits, deletions and reordering.

use std::fmt;
use std::sync::{Mutex, MutexGuard};

use agentbuild_core::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::checkpoint::now_ms;
use crate::error::{StorageError, StorageErrorKind};

/// The head of the chain, signed with a key that is not in the database
/// (design 0010 §5): a copy to keep elsewhere. With it,
/// [`AuditLog::verify_anchor`] detects what the chain alone cannot: a tail
/// cut off, or a whole chain recomputed after edits (the SHA-256 chain has
/// no secret, so someone who can write to the table can make any edit
/// consistent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditAnchor {
    /// The record's number.
    pub seq: u64,
    /// The record's hash.
    pub hash: String,
    /// HMAC-SHA256 (hex) of `seq` and `hash` under the signing key.
    pub mac: String,
}

impl AuditAnchor {
    /// Signs `(seq, hash)` with `key`.
    pub fn sign(seq: u64, hash: impl Into<String>, key: &[u8]) -> Self {
        let hash = hash.into();
        let mac = hex(&hmac_sha256(key, anchor_message(seq, &hash).as_bytes()));
        Self { seq, hash, mac }
    }

    /// `true` if `key` signed this anchor (compared in constant time).
    pub fn is_authentic(&self, key: &[u8]) -> bool {
        let message = anchor_message(self.seq, &self.hash);
        let expected = hex(&hmac_sha256(key, message.as_bytes()));
        constant_time_eq(expected.as_bytes(), self.mac.as_bytes())
    }
}

fn anchor_message(seq: u64, hash: &str) -> String {
    format!("agentbuild-audit-anchor:v1:{seq}:{hash}")
}

/// HMAC-SHA256 (RFC 2104).
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(padded.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(padded.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The `prev_hash` of the first record.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Something that happened, before it is recorded (design 0010 §1). No
/// content: ids, names, codes, counts and hashes only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditEvent {
    /// What happened, e.g. `approval.decided` (design 0010 §2).
    pub action: String,
    /// Who did it (set by the app); `None` for the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The thread, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// The run, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// What it acted on: a tool, a model, a checkpoint id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// How it ended: `completed`, an error code, a decision kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Small JSON with ids and numbers.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub details: Map<String, Value>,
}

impl AuditEvent {
    /// An event of `action`.
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            ..Self::default()
        }
    }

    /// Sets [`actor`](Self::actor).
    pub fn actor(mut self, actor: Option<impl Into<String>>) -> Self {
        self.actor = actor.map(Into::into);
        self
    }

    /// Sets [`thread_id`](Self::thread_id).
    pub fn thread(mut self, thread: Option<impl Into<String>>) -> Self {
        self.thread_id = thread.map(Into::into);
        self
    }

    /// Sets [`run_id`](Self::run_id).
    pub fn run(mut self, run: Option<impl Into<String>>) -> Self {
        self.run_id = run.map(Into::into);
        self
    }

    /// Sets [`subject`](Self::subject).
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Sets [`outcome`](Self::outcome).
    pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Adds a detail.
    pub fn detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }
}

/// A recorded [`AuditEvent`]: numbered, timed and chained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditRecord {
    /// 1, 2, 3… without gaps.
    pub seq: u64,
    /// When it was appended (Unix milliseconds).
    pub at_ms: u64,
    /// The event.
    #[serde(flatten)]
    pub event: AuditEvent,
    /// The previous record's hash ([`GENESIS_HASH`] for the first).
    pub prev_hash: String,
    /// SHA-256 (hex) of this record's fields and `prev_hash`.
    pub hash: String,
}

impl AuditRecord {
    /// The record of `event` after the record `(prev_seq, prev_hash)`.
    pub(crate) fn chain(event: AuditEvent, prev: Option<(u64, String)>, at_ms: u64) -> Self {
        let (prev_seq, prev_hash) = prev.unwrap_or((0, GENESIS_HASH.to_string()));
        let mut record = Self {
            seq: prev_seq + 1,
            at_ms,
            event,
            prev_hash,
            hash: String::new(),
        };
        record.hash = record.compute_hash();
        record
    }

    /// The hash its fields give (design 0010 §5).
    pub fn compute_hash(&self) -> String {
        let e = &self.event;
        let fields = Value::Array(vec![
            self.seq.into(),
            self.at_ms.into(),
            e.action.clone().into(),
            e.actor.clone().into(),
            e.thread_id.clone().into(),
            e.run_id.clone().into(),
            e.subject.clone().into(),
            e.outcome.clone().into(),
            Value::Object(e.details.clone()),
            self.prev_hash.clone().into(),
        ]);
        let mut text = String::new();
        canonical(&fields, &mut text);
        hex(&Sha256::digest(text.as_bytes()))
    }
}

/// JSON with object keys sorted and no whitespace, whatever map order
/// serde_json was built with.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(list) => {
            out.push('[');
            for (i, item) in list.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of `text`, hex: for recording that a value existed without
/// recording it (e.g. a memory key).
pub fn sha256_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// Which records [`AuditLog::query`] returns, in `seq` order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditQuery {
    /// Only this thread's records.
    pub thread_id: Option<String>,
    /// Only this actor's records.
    pub actor: Option<String>,
    /// Only this action.
    pub action: Option<String>,
    /// Only records after this `seq` (paging).
    pub after_seq: Option<u64>,
    /// Only records at or after this time (Unix ms).
    pub since_ms: Option<u64>,
    /// Only records before this time.
    pub until_ms: Option<u64>,
    /// The most records to return. Default 100.
    pub limit: usize,
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self {
            thread_id: None,
            actor: None,
            action: None,
            after_seq: None,
            since_ms: None,
            until_ms: None,
            limit: 100,
        }
    }
}

impl AuditQuery {
    /// Every record, 100 at a time.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only `thread`'s records.
    pub fn thread(mut self, thread: impl Into<String>) -> Self {
        self.thread_id = Some(thread.into());
        self
    }

    /// Only `actor`'s records.
    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// Only `action`.
    pub fn action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    /// Records after `seq`.
    pub fn after_seq(mut self, seq: u64) -> Self {
        self.after_seq = Some(seq);
        self
    }

    /// Records at or after `ms`.
    pub fn since_ms(mut self, ms: u64) -> Self {
        self.since_ms = Some(ms);
        self
    }

    /// Records before `ms`.
    pub fn until_ms(mut self, ms: u64) -> Self {
        self.until_ms = Some(ms);
        self
    }

    /// At most `limit` records.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub(crate) fn matches(&self, record: &AuditRecord) -> bool {
        let e = &record.event;
        self.thread_id
            .as_ref()
            .is_none_or(|t| e.thread_id.as_ref() == Some(t))
            && self
                .actor
                .as_ref()
                .is_none_or(|a| e.actor.as_ref() == Some(a))
            && self.action.as_ref().is_none_or(|a| &e.action == a)
            && self.after_seq.is_none_or(|s| record.seq > s)
            && self.since_ms.is_none_or(|t| record.at_ms >= t)
            && self.until_ms.is_none_or(|t| record.at_ms < t)
    }
}

/// The result of [`AuditLog::verify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditVerification {
    /// Records checked.
    pub records: u64,
    /// `true` if every hash and link is right and `seq` has no gaps.
    pub ok: bool,
    /// The first record that is wrong (changed, missing before it, or out
    /// of order).
    pub first_broken: Option<u64>,
    /// The last record's `(seq, hash)`.
    pub head: Option<(u64, String)>,
}

/// Checks records given one by one in `seq` order (design 0010 §5), so a
/// long trail can be read in pages.
#[derive(Debug, Default)]
pub(crate) struct ChainCheck {
    records: u64,
    prev: Option<(u64, String)>,
    first_broken: Option<u64>,
}

impl ChainCheck {
    pub(crate) fn push(&mut self, record: &AuditRecord) {
        self.records += 1;
        let (prev_seq, prev_hash) = self.prev.clone().unwrap_or((0, GENESIS_HASH.to_string()));
        if self.first_broken.is_none()
            && (record.seq != prev_seq + 1
                || record.prev_hash != prev_hash
                || record.hash != record.compute_hash())
        {
            self.first_broken = Some(record.seq);
        }
        self.prev = Some((record.seq, record.hash.clone()));
    }

    pub(crate) fn finish(self) -> AuditVerification {
        AuditVerification {
            records: self.records,
            ok: self.first_broken.is_none(),
            first_broken: self.first_broken,
            head: self.prev,
        }
    }
}

/// Where the audit trail is kept (design 0010 §3). Append-only: there is
/// no update or delete.
pub trait AuditLog: Send + Sync + 'static {
    /// Records `event` after the current last record.
    fn append(&self, event: AuditEvent) -> BoxFuture<'_, Result<AuditRecord, StorageError>>;

    /// Records matching `query`, in `seq` order.
    fn query<'a>(
        &'a self,
        query: &'a AuditQuery,
    ) -> BoxFuture<'a, Result<Vec<AuditRecord>, StorageError>>;

    /// Checks the whole chain.
    fn verify(&self) -> BoxFuture<'_, Result<AuditVerification, StorageError>>;

    /// The last record's `(seq, hash)`: copy it elsewhere now and then, to
    /// detect a rewritten chain.
    fn head(&self) -> BoxFuture<'_, Result<Option<(u64, String)>, StorageError>>;

    /// The head, signed with `key` (keep the key outside the database, and
    /// the anchor where the database's writers cannot reach). `None` for an
    /// empty trail.
    fn anchor<'a>(
        &'a self,
        key: &'a [u8],
    ) -> BoxFuture<'a, Result<Option<AuditAnchor>, StorageError>> {
        Box::pin(async move {
            Ok(self
                .head()
                .await?
                .map(|(seq, hash)| AuditAnchor::sign(seq, hash, key)))
        })
    }

    /// Checks the whole chain **and** that the record the anchor names is
    /// still there with the same hash. `ok` is false (`first_broken` the
    /// anchor's `seq`) if the chain was cut short or recomputed; an anchor
    /// that `key` did not sign is an `invalid_value` error.
    fn verify_anchor<'a>(
        &'a self,
        anchor: &'a AuditAnchor,
        key: &'a [u8],
    ) -> BoxFuture<'a, Result<AuditVerification, StorageError>> {
        Box::pin(async move {
            if !anchor.is_authentic(key) {
                return Err(StorageError::new(
                    StorageErrorKind::InvalidValue,
                    "the audit anchor was not signed with this key",
                ));
            }
            let mut report = self.verify().await?;
            if !report.ok {
                return Ok(report);
            }
            let query = AuditQuery::new()
                .after_seq(anchor.seq.saturating_sub(1))
                .limit(1);
            let found = self.query(&query).await?;
            let matches = found
                .first()
                .is_some_and(|r| r.seq == anchor.seq && r.hash == anchor.hash);
            if !matches {
                report.ok = false;
                report.first_broken = Some(anchor.seq);
            }
            Ok(report)
        })
    }
}

/// The audit trail in process memory: for tests and in-memory storage.
#[derive(Debug, Default)]
pub struct InMemoryAuditLog {
    records: Mutex<Vec<AuditRecord>>,
}

impl InMemoryAuditLog {
    /// An empty trail.
    pub fn new() -> Self {
        Self::default()
    }

    fn records(&self) -> MutexGuard<'_, Vec<AuditRecord>> {
        self.records.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces record `seq`'s event, without fixing its hash: what an
    /// attacker editing the store would do (for tests of
    /// [`AuditLog::verify`]).
    #[doc(hidden)]
    pub fn tamper(&self, seq: u64, event: AuditEvent) {
        if let Some(record) = self.records().iter_mut().find(|r| r.seq == seq) {
            record.event = event;
        }
    }
}

impl AuditLog for InMemoryAuditLog {
    fn append(&self, event: AuditEvent) -> BoxFuture<'_, Result<AuditRecord, StorageError>> {
        let result = check_event(&event).map(|()| {
            let mut records = self.records();
            let prev = records.last().map(|r| (r.seq, r.hash.clone()));
            let record = AuditRecord::chain(event, prev, now_ms());
            records.push(record.clone());
            record
        });
        Box::pin(async move { result })
    }

    fn query<'a>(
        &'a self,
        query: &'a AuditQuery,
    ) -> BoxFuture<'a, Result<Vec<AuditRecord>, StorageError>> {
        let found = self
            .records()
            .iter()
            .filter(|r| query.matches(r))
            .take(query.limit)
            .cloned()
            .collect();
        Box::pin(async move { Ok(found) })
    }

    fn verify(&self) -> BoxFuture<'_, Result<AuditVerification, StorageError>> {
        let mut check = ChainCheck::default();
        self.records().iter().for_each(|r| check.push(r));
        let report = check.finish();
        Box::pin(async move { Ok(report) })
    }

    fn head(&self) -> BoxFuture<'_, Result<Option<(u64, String)>, StorageError>> {
        let head = self.records().last().map(|r| (r.seq, r.hash.clone()));
        Box::pin(async move { Ok(head) })
    }
}

/// An action is required; everything else is optional.
pub(crate) fn check_event(event: &AuditEvent) -> Result<(), StorageError> {
    if event.action.is_empty() {
        return Err(StorageError::new(
            StorageErrorKind::InvalidValue,
            "an audit event needs an action",
        ));
    }
    Ok(())
}

impl fmt::Display for AuditRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {}", self.seq, self.event.action)?;
        if let Some(actor) = &self.event.actor {
            write!(f, " by {actor}")?;
        }
        if let Some(thread) = &self.event.thread_id {
            write!(f, " on {thread}")?;
        }
        if let Some(outcome) = &self.event.outcome {
            write!(f, ": {outcome}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn records_chain_and_verify() {
        let log = InMemoryAuditLog::new();
        let a = log
            .append(
                AuditEvent::new("run.finished")
                    .actor(Some("dr-rao"))
                    .outcome("completed"),
            )
            .await
            .unwrap();
        assert_eq!(a.seq, 1);
        assert_eq!(a.prev_hash, GENESIS_HASH);
        assert_eq!(a.hash.len(), 64);
        let b = log
            .append(
                AuditEvent::new("tool.executed")
                    .subject("lookup")
                    .detail("ms", 5),
            )
            .await
            .unwrap();
        assert_eq!(b.prev_hash, a.hash);
        let report = log.verify().await.unwrap();
        assert!(report.ok);
        assert_eq!(report.records, 2);
        assert_eq!(report.head, Some((2, b.hash.clone())));

        log.tamper(
            1,
            AuditEvent::new("run.finished").actor(Some("someone-else")),
        );
        let report = log.verify().await.unwrap();
        assert!(!report.ok);
        assert_eq!(report.first_broken, Some(1));
        assert!(log.append(AuditEvent::new("")).await.is_err());
    }

    #[test]
    fn hashes_ignore_map_order_and_change_with_any_field() {
        let mut one = Map::new();
        one.insert("b".into(), json!(1));
        one.insert("a".into(), json!({"y": 2, "x": [1, {"d": 1, "c": 2}]}));
        let mut text = String::new();
        canonical(&Value::Object(one), &mut text);
        assert_eq!(text, r#"{"a":{"x":[1,{"c":2,"d":1}],"y":2},"b":1}"#);

        let base = AuditRecord::chain(AuditEvent::new("x").detail("n", 1), None, 7);
        for changed in [
            AuditRecord::chain(AuditEvent::new("y").detail("n", 1), None, 7),
            AuditRecord::chain(AuditEvent::new("x").detail("n", 2), None, 7),
            AuditRecord::chain(AuditEvent::new("x").detail("n", 1), None, 8),
            AuditRecord::chain(
                AuditEvent::new("x").detail("n", 1),
                Some((0, "1".repeat(64))),
                7,
            ),
        ] {
            assert_ne!(changed.hash, base.hash);
        }
        assert_eq!(sha256_hex("abc").len(), 64);
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

#[cfg(test)]
mod hmac_tests {
    use super::*;

    #[test]
    fn hmac_sha256_matches_the_rfc_4231_vectors() {
        let mac = hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?"));
        assert_eq!(
            mac,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block size is hashed first (case 6).
        let mac = hex(&hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        ));
        assert_eq!(
            mac,
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn anchors_are_authentic_only_under_their_key() {
        let anchor = AuditAnchor::sign(7, "abc", b"key-one");
        assert!(anchor.is_authentic(b"key-one"));
        assert!(!anchor.is_authentic(b"key-two"));
        let mut edited = anchor.clone();
        edited.seq = 8;
        assert!(!edited.is_authentic(b"key-one"));
    }
}

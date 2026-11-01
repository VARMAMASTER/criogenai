//! Signed snapshots (design 0005 §5, security audit October 2026).
//!
//! A [`Snapshot`] is plain JSON, so an app that keeps one in a place its
//! users can reach (a cookie, a client-side store, a shared bucket) hands
//! them the means to edit it: skip the approval step, forge a decision,
//! change whose run it is. [`seal_snapshot`] signs it with a key only the
//! server holds, and [`open_snapshot`] refuses anything that was changed,
//! signed with another key, or sealed for someone else.
//!
//! The signature does not stop a user from handing back an **old** valid
//! snapshot, so resuming it twice would run its gated tool twice. For that,
//! keep snapshots on the server (a [`Thread`](https://docs.rs/criogen-agent)
//! does: it resumes only the head, by compare-and-set) or record each
//! sealed snapshot's id once it is used.

use criogen_core::Snapshot;

use crate::audit::{constant_time_eq, hex, hmac_sha256};
use crate::error::{StorageError, StorageErrorKind};

/// A sealed snapshot starts with this.
const PREFIX: &str = "abs1.";

/// The shortest key accepted.
pub const MIN_SEAL_KEY_BYTES: usize = 16;

fn mac(key: &[u8], context: &str, json: &str) -> String {
    // The context is length-prefixed: no two (context, json) pairs give the
    // same message.
    let message = format!("criogen-snapshot:v1:{}:{context}:{json}", context.len());
    hex(&hmac_sha256(key, message.as_bytes()))
}

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::new(StorageErrorKind::InvalidValue, message)
}

/// `snapshot` as text, signed with `key` (at least
/// [`MIN_SEAL_KEY_BYTES`] bytes, kept on the server). `context` says whose it
/// is, for example the user id and thread id: [`open_snapshot`] refuses it
/// under any other context, so one user cannot resume another's run.
pub fn seal_snapshot(
    snapshot: &Snapshot,
    key: &[u8],
    context: &str,
) -> Result<String, StorageError> {
    if key.len() < MIN_SEAL_KEY_BYTES {
        return Err(invalid(format!(
            "the signing key is shorter than {MIN_SEAL_KEY_BYTES} bytes"
        )));
    }
    let json = serde_json::to_string(snapshot)
        .map_err(|err| invalid(format!("the snapshot cannot be written: {err}")))?;
    Ok(format!("{PREFIX}{}.{json}", mac(key, context, &json)))
}

/// The snapshot in `sealed`, if `key` signed exactly this text for
/// `context`. Any other text, key or context is an `invalid_value` error
/// that says only that the snapshot is not trusted.
pub fn open_snapshot(sealed: &str, key: &[u8], context: &str) -> Result<Snapshot, StorageError> {
    let untrusted = || invalid("the snapshot is not one this server sealed for this context");
    if key.len() < MIN_SEAL_KEY_BYTES {
        return Err(invalid(format!(
            "the signing key is shorter than {MIN_SEAL_KEY_BYTES} bytes"
        )));
    }
    let rest = sealed.strip_prefix(PREFIX).ok_or_else(untrusted)?;
    let (signature, json) = rest.split_once('.').ok_or_else(untrusted)?;
    if !constant_time_eq(mac(key, context, json).as_bytes(), signature.as_bytes()) {
        return Err(untrusted());
    }
    serde_json::from_str(json).map_err(|_| untrusted())
}

#[cfg(test)]
mod tests {
    use super::*;
    use criogen_core::Snapshot;

    fn snapshot() -> Snapshot {
        // A paused run, built the way the engine does it (a serialized form
        // is enough: the fields are the engine's).
        serde_json::from_value(serde_json::json!({
            "format": "criogen.snapshot/1",
            "workflow_fingerprint": "abc",
            "run_id": "r-1",
            "step": 2,
            "state": {"user_id": "ada"},
            "frontier": ["review"],
            "join_progress": {},
            "suspended": [],
        }))
        .expect("the test snapshot matches the engine's format")
    }

    #[test]
    fn a_sealed_snapshot_opens_only_unchanged_with_its_key_and_context() {
        let key = b"a-server-side-key-0123456789";
        let sealed = seal_snapshot(&snapshot(), key, "user:ada/thread:7").unwrap();
        assert_eq!(
            open_snapshot(&sealed, key, "user:ada/thread:7").unwrap(),
            snapshot()
        );
        // Edited (the approval step skipped), re-signed with a guess, or
        // another user's context: all the same refusal.
        let edited = sealed.replace("\"review\"", "\"tools\"");
        assert_ne!(edited, sealed);
        for bad in [
            edited.as_str(),
            "abs1.deadbeef.{}",
            "plain json",
            "",
            sealed.trim_start_matches("abs1."),
        ] {
            let err = open_snapshot(bad, key, "user:ada/thread:7").unwrap_err();
            assert_eq!(err.code(), "invalid_value", "{bad}");
        }
        assert!(open_snapshot(&sealed, b"another-key-0123456789", "user:ada/thread:7").is_err());
        assert!(open_snapshot(&sealed, key, "user:bob/thread:7").is_err());
        // A context cannot be shifted into the JSON or the other way round.
        assert!(open_snapshot(&sealed, key, "user:ada/thread:7 ").is_err());
        // Short keys are refused, both ways.
        assert!(seal_snapshot(&snapshot(), b"short", "c").is_err());
        assert!(open_snapshot(&sealed, b"short", "user:ada/thread:7").is_err());
    }
}

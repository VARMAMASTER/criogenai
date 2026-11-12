use std::fmt;

/// What kind of storage failure happened (design 0008 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StorageErrorKind {
    /// Another run holds the thread (`on_busy` is `Reject`, or
    /// `busy_timeout` elapsed while waiting).
    ThreadBusy,
    /// The backend could not be reached or timed out. Transient.
    Unavailable,
    /// A compare-and-set lost: the thread changed since it was read.
    Conflict,
    /// The checkpoint id is unknown for this thread.
    CheckpointNotFound,
    /// Stored data cannot be read back.
    Corrupt,
    /// The database was written by a newer, incompatible version.
    Incompatible,
    /// A value, key or namespace that storage does not accept (for example
    /// a memory item over the size limit).
    InvalidValue,
    /// Encrypted data, but no key for it: an encrypted database opened
    /// without keys, or a row encrypted with a key id not in the keyring.
    EncryptionKeyMissing,
    /// A key whose id the database knows does not match it.
    EncryptionKeyInvalid,
    /// An audit record could not be written, so the action it describes
    /// failed (design 0010 §4). Transient.
    AuditFailed,
}

impl StorageErrorKind {
    /// The stable snake_case code.
    pub fn code(self) -> &'static str {
        match self {
            Self::ThreadBusy => "thread_busy",
            Self::Unavailable => "storage_unavailable",
            Self::Conflict => "storage_conflict",
            Self::CheckpointNotFound => "checkpoint_not_found",
            Self::Corrupt => "storage_corrupt",
            Self::Incompatible => "storage_incompatible",
            Self::InvalidValue => "invalid_value",
            Self::EncryptionKeyMissing => "encryption_key_missing",
            Self::EncryptionKeyInvalid => "encryption_key_invalid",
            Self::AuditFailed => "audit_failed",
        }
    }
}

/// A storage operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub struct StorageError {
    /// What kind of failure.
    pub kind: StorageErrorKind,
    /// What went wrong.
    pub message: String,
}

impl StorageError {
    /// A new error.
    pub fn new(kind: StorageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The stable snake_case code (see [`StorageErrorKind::code`]).
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}

/// Stored data that cannot be read back (`storage_corrupt`).
#[cfg_attr(
    not(any(feature = "sqlite", feature = "postgres", feature = "encryption")),
    allow(dead_code)
)]
pub(crate) fn corrupt(message: impl Into<String>) -> StorageError {
    StorageError::new(StorageErrorKind::Corrupt, message)
}

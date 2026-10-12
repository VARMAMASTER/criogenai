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

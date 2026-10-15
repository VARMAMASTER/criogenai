//! Threads, checkpoints and storage for AgentBuild (design 0008).
//!
//! A **thread** is one conversation or work item: a sequence of runs that
//! share state. Each run ends in a [`Checkpoint`] (plain JSON) written
//! through a [`Checkpointer`] with compare-and-set, so two writers cannot
//! silently fork a thread. [`Storage`] bundles a checkpointer with the
//! rules for busy threads ([`OnBusy`]), and a [`MemoryStore`] for
//! long-term memory: facts that outlive a thread, kept under a
//! [`Namespace`] such as `users/42`.
//!
//! Agents use this through `agentbuild_agent::Agent::thread`.
//!
//! Runs are saved as they go ([`Durability::Step`], via [`ThreadWriter`]),
//! so a run interrupted by a crash continues where it stopped.
//!
//! **Status:** steps 1–4 of design 0008: the contract, the in-memory
//! backends; with the `sqlite` feature, `SqliteCheckpointer` (delta
//! checkpoints with keyframes, migrations) and `SqliteMemoryStore`
//! (keyword search); with the `postgres` feature, the same on Postgres
//! plus [`ThreadLocks`] across processes. Vector search, retention and
//! encryption at rest are planned.
//!
//! ```
//! use agentbuild_core::State;
//! use agentbuild_storage::{Checkpoint, Source, Storage};
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let storage = Storage::in_memory();
//! let store = storage.checkpointer();
//! let first = Checkpoint::new("chat-1", 1, Source::Step, State::new().with("n", 1));
//! store.commit(first.clone(), 0).await.unwrap();
//!
//! // A writer that read version 0 loses.
//! let stale = Checkpoint::new("chat-1", 1, Source::Step, State::new());
//! assert_eq!(store.commit(stale, 0).await.unwrap_err().code(), "storage_conflict");
//!
//! let head = store.load("chat-1", None).await.unwrap().unwrap();
//! assert_eq!(head.id, first.id);
//! # });
//! ```

mod checkpoint;
pub mod delta;
mod error;
mod locks;
mod memory;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;
#[cfg(feature = "sqlite")]
mod sqlite;
mod storage;
mod store;
mod writer;

pub use checkpoint::{
    Checkpoint, CheckpointMeta, CheckpointStatus, Checkpointer, Source, merge_pending,
};
pub use error::{StorageError, StorageErrorKind};
pub use locks::{HeldLock, InterruptHandler, ThreadLocks};
pub use memory::InMemoryCheckpointer;
#[cfg(feature = "postgres")]
pub use postgres::{
    PostgresCheckpointer, PostgresMemoryStore, PostgresOptions, PostgresThreadLocks,
};
#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteCheckpointer, SqliteMemoryStore};
pub use storage::{Durability, OnBusy, Storage, ThreadLease};
pub use store::{
    InMemoryStore, MAX_KEY_BYTES, MAX_VALUE_BYTES, MemoryItem, MemoryStore, Namespace,
    ScopedMemory, SearchQuery,
};
pub use writer::ThreadWriter;

// Compiles and runs the README's example with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

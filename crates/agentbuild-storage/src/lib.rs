//! Threads, checkpoints and storage for AgentBuild (design 0008).
//!
//! A **thread** is one conversation or work item: a sequence of runs that
//! share state. Each run ends in a [`Checkpoint`] (plain JSON) written
//! through a [`Checkpointer`] with compare-and-set, so two writers cannot
//! silently fork a thread. [`Storage`] bundles a checkpointer with the
//! rules for busy threads ([`OnBusy`]).
//!
//! Agents use this through `agentbuild_agent::Agent::thread`.
//!
//! Runs are saved as they go ([`Durability::Step`], via [`ThreadWriter`]),
//! so a run interrupted by a crash continues where it stopped.
//!
//! **Status:** steps 1–2 of design 0008: the contract, the in-memory
//! backend and, with the `sqlite` feature, `SqliteCheckpointer` (delta
//! checkpoints with keyframes, migrations). Postgres, the memory store and
//! retention are planned.
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
mod memory;
#[cfg(feature = "sqlite")]
mod sqlite;
mod storage;
mod writer;

pub use checkpoint::{
    Checkpoint, CheckpointMeta, CheckpointStatus, Checkpointer, Source, merge_pending,
};
pub use error::{StorageError, StorageErrorKind};
pub use memory::InMemoryCheckpointer;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteCheckpointer;
pub use storage::{Durability, OnBusy, Storage, ThreadLease};
pub use writer::ThreadWriter;

// Compiles and runs the README's example with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

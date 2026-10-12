# agentbuild-storage

Threads, checkpoints and storage backends for
[AgentBuild](https://github.com/VARMAMASTER/agentbuild) (design 0008).

- a **thread** is one conversation or work item; each run on it ends in a
  **checkpoint**: plain JSON with the state and, for a suspended or failed
  run, the snapshot to continue from
- commits are **compare-and-set** on the thread's version, so two writers
  cannot silently fork a thread
- busy threads: a second operation waits its turn (default), fails fast, or
  interrupts the active run
- history follows the current branch; rewinding keeps the old branch
- runs are saved before every step and after every task (durability
  `step`), so an interrupted run continues without repeating finished work
- backends: in memory, and SQLite (feature `sqlite`), which stores only
  what changed per checkpoint plus a full copy every 25

**Status:** the contract, the in-memory and SQLite backends, per-step
saving. Postgres, the long-term memory store and retention are planned.

Most code uses this through agents (`agent.thread("user-42")`, see
`agentbuild-agent`). The contract underneath:

```rust
use agentbuild_core::State;
use agentbuild_storage::{Checkpoint, Source, Storage};

# tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
let storage = Storage::in_memory();
let store = storage.checkpointer();
store
    .commit(Checkpoint::new("chat-1", 1, Source::Step, State::new().with("n", 1)), 0)
    .await
    .unwrap();

// A writer that still thinks the thread is new loses.
let stale = Checkpoint::new("chat-1", 1, Source::Step, State::new());
assert_eq!(store.commit(stale, 0).await.unwrap_err().code(), "storage_conflict");
assert_eq!(store.history("chat-1", 10).await.unwrap().len(), 1);
# });
```

With the `sqlite` feature, `Storage::connect("sqlite://agent.db")` opens
(or creates) a database file; tables are created on first use.

A custom backend implements the five methods of `Checkpointer`.

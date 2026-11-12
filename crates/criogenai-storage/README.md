# criogenai-storage

Threads, checkpoints and storage backends for
[CriogenAI](https://github.com/VARMAMASTER/criogen) (design 0008).

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
- backends: in memory; SQLite (feature `sqlite`), which stores only what
  changed per checkpoint plus a full copy every 25; and Postgres (feature
  `postgres`), with the same format, TLS, and busy-thread rules that hold
  across processes (advisory locks)
- long-term memory: a `MemoryStore` of JSON items under namespaces such as
  `users/42`, with keyword search, time to live, and deleting everything
  in a namespace; in memory or in the same SQLite or Postgres database
- retention: delete threads idle for a time, keep only the last N
  checkpoints of each thread, remove expired memory (`Storage::prune`, or
  a background pruner)
- encryption at rest (feature `encryption`): conversations, snapshots,
  task results and memory are stored encrypted (AES-256-GCM, a key per
  row), with key rotation; a database that is encrypted refuses to open
  without its keys

- audit trail (design 0010): who did what and when, without content, in
  a hash chain that `verify` checks; in the same database
  (`Storage::with_audit`)
- semantic search: items saved with a vector (`put_embedded`) are found
  by meaning (`SearchQuery::similar_to`), ranked by cosine similarity on
  every backend

**Status:** the contract, the in-memory, SQLite and Postgres backends,
per-step saving, the memory store with keyword and semantic search,
retention, encryption at rest. Vector indexes are planned.

Most code uses this through agents (`agent.thread("user-42")`, see
`criogenai-agent`). The contract underneath:

```rust
use criogenai_core::State;
use criogenai_storage::{Checkpoint, Source, Storage};

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

Encrypted, with retention (features `sqlite` and `encryption`):

```rust,ignore
// CRIOGEN_ENCRYPTION_KEYS="k1:<64 hex digits>"  (openssl rand -hex 32)
let keys = Keyring::from_env("CRIOGEN_ENCRYPTION_KEYS")?;
let storage = Storage::connect_encrypted("sqlite://agent.db", keys)
    .await?
    .retention(Retention::new().thread_ttl(Duration::from_secs(30 * 86_400)));
let _pruner = storage.spawn_pruner(Duration::from_secs(3600));
```

Thread ids, namespaces and timestamps stay readable: use opaque ids, not
names or record numbers.

A custom backend implements the seven methods of `Checkpointer`.

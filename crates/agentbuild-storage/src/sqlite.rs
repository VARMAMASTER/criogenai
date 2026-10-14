//! SQLite storage (design 0008 §4, §12, §13): delta checkpoints with
//! keyframes, compare-and-set commits, automatic migrations, and the memory
//! store.

mod memory;

pub use memory::SqliteMemoryStore;

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use agentbuild_core::{BoxFuture, PendingWrite, Snapshot, State};
use serde_json::Value;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Row, Sqlite, Transaction};

use crate::checkpoint::{Checkpoint, CheckpointMeta, Checkpointer, merge_pending, now_ms};
use crate::delta::{self, DeltaOp};
use crate::error::{StorageError, StorageErrorKind};

/// Schema changes, in order. Each runs once, in a transaction, and is
/// recorded in `agentbuild_schema_migrations`.
const MIGRATIONS: &[&str] = &[
    // 1: threads, checkpoints and task writes.
    "CREATE TABLE agentbuild_threads (
        thread_id TEXT PRIMARY KEY,
        head_id TEXT NOT NULL,
        head_version INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );
    CREATE TABLE agentbuild_checkpoints (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        parent_id TEXT,
        version INTEGER NOT NULL,
        source TEXT NOT NULL,
        status TEXT NOT NULL,
        run_id TEXT NOT NULL,
        step INTEGER NOT NULL,
        kind TEXT NOT NULL,
        depth INTEGER NOT NULL,
        body TEXT NOT NULL,
        snapshot TEXT,
        created_at_ms INTEGER NOT NULL
    );
    CREATE INDEX agentbuild_checkpoints_thread
        ON agentbuild_checkpoints (thread_id, version);
    CREATE TABLE agentbuild_pending_writes (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        checkpoint_id TEXT NOT NULL,
        node TEXT NOT NULL,
        write TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    );
    CREATE INDEX agentbuild_pending_writes_checkpoint
        ON agentbuild_pending_writes (checkpoint_id, seq);",
    // 2: long-term memory. `search_text` holds the item's words as
    // ` word word … ` for keyword search.
    "CREATE TABLE agentbuild_memory (
        namespace TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        search_text TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER,
        PRIMARY KEY (namespace, key)
    );
    CREATE INDEX agentbuild_memory_expires
        ON agentbuild_memory (expires_at_ms) WHERE expires_at_ms IS NOT NULL",
];

/// Default for [`SqliteCheckpointer::keyframe_interval`].
const KEYFRAME_INTERVAL: u32 = 25;

/// Remembered states (for computing deltas) before the cache is cleared.
const CACHE_LIMIT: usize = 256;

/// Checkpoints in a SQLite database file. Survives restarts; for local apps
/// and single-machine services. Several processes may share the file:
/// commits are compare-and-set, but leases ([`OnBusy`](crate::OnBusy)) are
/// per process.
///
/// Each checkpoint stores only what changed since its parent (appended
/// messages, changed keys); every [`keyframe_interval`](Self::keyframe_interval)
/// checkpoints, or when a delta would not be smaller, the full state. So a
/// long chat stores each message once, and loading reads one keyframe plus
/// at most `interval - 1` deltas.
///
/// Tables (prefixed `agentbuild_`) are created on first use.
///
/// Needs Tokio's real clock: under a paused test clock (`start_paused`) the
/// connection pool's timeout fires at once. Tests on paused time should use
/// [`InMemoryCheckpointer`](crate::InMemoryCheckpointer).
pub struct SqliteCheckpointer {
    pool: SqlitePool,
    keyframe_interval: u32,
    /// The last state written or read per thread, to compute the next delta
    /// without reading it back.
    cache: Mutex<HashMap<String, Cached>>,
}

#[derive(Clone)]
struct Cached {
    id: String,
    state: State,
    depth: u32,
}

impl std::fmt::Debug for SqliteCheckpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteCheckpointer")
            .field("keyframe_interval", &self.keyframe_interval)
            .finish_non_exhaustive()
    }
}

impl SqliteCheckpointer {
    /// Opens (or creates) the database at `url`: `sqlite://path/to/file.db`,
    /// `sqlite:file.db`, or `sqlite::memory:` (one private in-memory
    /// database, lost when this value is dropped). Applies pending
    /// migrations.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let in_memory = url.contains(":memory:") || url.contains("mode=memory");
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|err| {
                StorageError::new(
                    StorageErrorKind::Unavailable,
                    format!("invalid SQLite URL `{url}`: {err}"),
                )
            })?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = if in_memory {
            // Every connection to `:memory:` is a separate database: keep
            // exactly one, forever.
            SqlitePoolOptions::new()
                .max_connections(1)
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
        } else {
            SqlitePoolOptions::new().max_connections(8)
        }
        .connect_with(options)
        .await
        .map_err(sql_error)?;
        let checkpointer = Self {
            pool,
            keyframe_interval: KEYFRAME_INTERVAL,
            cache: Mutex::default(),
        };
        checkpointer.migrate().await?;
        Ok(checkpointer)
    }

    /// Opens (or creates) the database file at `path`.
    pub async fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        Self::connect(&format!("sqlite://{}", path.as_ref().display())).await
    }

    /// Writes the full state at least every `interval` checkpoints of a
    /// branch (default 25, minimum 1 = no deltas).
    pub fn keyframe_interval(mut self, interval: u32) -> Self {
        self.keyframe_interval = interval.max(1);
        self
    }

    /// The memory store in the same database (same connections).
    pub fn memory_store(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::new(self.pool.clone())
    }

    /// The schema version the database is at.
    pub async fn schema_version(&self) -> Result<u32, StorageError> {
        let version: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM agentbuild_schema_migrations",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(sql_error)?;
        Ok(version as u32)
    }

    /// Closes the database connections. Later calls fail with
    /// `storage_unavailable`.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    async fn migrate(&self) -> Result<(), StorageError> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agentbuild_schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at_ms INTEGER NOT NULL
            )",
        )
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        let current: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM agentbuild_schema_migrations",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(sql_error)?;
        let known = MIGRATIONS.len() as i64;
        if current > known {
            return Err(StorageError::new(
                StorageErrorKind::Incompatible,
                format!(
                    "the database is at schema version {current}, newer than this version of \
                     AgentBuild supports ({known}); upgrade AgentBuild"
                ),
            ));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            // One statement at a time: `raw_sql`'s future is not `Send` for
            // every lifetime, which would stop callers from opening storage
            // inside `tokio::spawn`. Migrations have no `;` inside literals.
            for statement in migration.split(';').map(str::trim) {
                if !statement.is_empty() {
                    sqlx::query(statement)
                        .execute(&mut *tx)
                        .await
                        .map_err(sql_error)?;
                }
            }
            sqlx::query(
                "INSERT INTO agentbuild_schema_migrations (version, applied_at_ms) VALUES (?, ?)",
            )
            .bind(index as i64 + 1)
            .bind(now_ms() as i64)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        }
        tx.commit().await.map_err(sql_error)
    }

    /// A write transaction that holds the write lock from the start, so it
    /// waits (busy timeout) instead of failing on a concurrent writer.
    async fn begin(&self) -> Result<Transaction<'static, Sqlite>, StorageError> {
        self.pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_error)
    }

    fn cache(&self) -> MutexGuard<'_, HashMap<String, Cached>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn remember(&self, thread: &str, cached: Cached) {
        let mut cache = self.cache();
        if cache.len() >= CACHE_LIMIT && !cache.contains_key(thread) {
            cache.clear();
        }
        cache.insert(thread.to_string(), cached);
    }

    /// The state of checkpoint `id` and its distance from its keyframe.
    async fn materialize(
        &self,
        thread: &str,
        id: &str,
    ) -> Result<Option<(State, u32)>, StorageError> {
        if let Some(cached) = self.cache().get(thread).filter(|c| c.id == id) {
            return Ok(Some((cached.state.clone(), cached.depth)));
        }
        // The checkpoint and its ancestors up to the nearest keyframe.
        let rows = sqlx::query(
            "WITH RECURSIVE chain (id, parent_id, kind, depth, body, n) AS (
                SELECT id, parent_id, kind, depth, body, 0
                    FROM agentbuild_checkpoints WHERE id = ?1 AND thread_id = ?2
                UNION ALL
                SELECT c.id, c.parent_id, c.kind, c.depth, c.body, chain.n + 1
                    FROM agentbuild_checkpoints c JOIN chain ON c.id = chain.parent_id
                    WHERE chain.kind = 'delta'
            )
            SELECT id, kind, depth, body FROM chain ORDER BY n DESC",
        )
        .bind(id)
        .bind(thread)
        .fetch_all(&self.pool)
        .await
        .map_err(sql_error)?;
        let Some(first) = rows.first() else {
            return Ok(None);
        };
        if get::<String>(first, "kind")? != "keyframe" {
            return Err(corrupt(format!(
                "checkpoint `{id}` has no keyframe among its ancestors"
            )));
        }
        let mut state: State = parse(&get::<String>(first, "body")?, "keyframe")?;
        for row in &rows[1..] {
            let ops: Vec<DeltaOp> = parse(&get::<String>(row, "body")?, "delta")?;
            state = delta::apply(state, &ops)?;
        }
        let depth = get::<i64>(rows.last().expect("not empty"), "depth")? as u32;
        Ok(Some((state, depth)))
    }

    async fn pending_writes(
        &self,
        checkpoint: &str,
    ) -> Result<Vec<(String, PendingWrite)>, StorageError> {
        let rows = sqlx::query(
            "SELECT node, write FROM agentbuild_pending_writes
                WHERE checkpoint_id = ? ORDER BY seq",
        )
        .bind(checkpoint)
        .fetch_all(&self.pool)
        .await
        .map_err(sql_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    get(row, "node")?,
                    parse(&get::<String>(row, "write")?, "task write")?,
                ))
            })
            .collect()
    }

    async fn load_checkpoint(
        &self,
        thread: &str,
        at: Option<&str>,
    ) -> Result<Option<Checkpoint>, StorageError> {
        let id = match at {
            Some(id) => id.to_string(),
            None => {
                let head: Option<String> = sqlx::query_scalar(
                    "SELECT head_id FROM agentbuild_threads WHERE thread_id = ?",
                )
                .bind(thread)
                .fetch_optional(&self.pool)
                .await
                .map_err(sql_error)?;
                match head {
                    Some(id) => id,
                    None => return Ok(None),
                }
            }
        };
        let row = sqlx::query(
            "SELECT id, parent_id, version, source, run_id, step, snapshot, created_at_ms
                FROM agentbuild_checkpoints WHERE id = ? AND thread_id = ?",
        )
        .bind(&id)
        .bind(thread)
        .fetch_optional(&self.pool)
        .await
        .map_err(sql_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let Some((state, depth)) = self.materialize(thread, &id).await? else {
            return Ok(None);
        };
        let snapshot = match get::<Option<String>>(&row, "snapshot")? {
            Some(text) => {
                let mut value: Value = parse(&text, "snapshot")?;
                if let Value::Object(map) = &mut value
                    && !map.contains_key("state")
                {
                    map.insert("state".into(), state.clone().into_value());
                }
                let mut snapshot: Snapshot = serde_json::from_value(value)
                    .map_err(|err| corrupt(format!("unreadable snapshot: {err}")))?;
                for (node, write) in self.pending_writes(&id).await? {
                    merge_pending(&mut snapshot, &node, write);
                }
                Some(snapshot)
            }
            None => None,
        };
        let checkpoint = Checkpoint {
            id: id.clone(),
            thread_id: thread.to_string(),
            parent_id: get(&row, "parent_id")?,
            version: get::<i64>(&row, "version")? as u64,
            source: parse_enum(&get::<String>(&row, "source")?)?,
            run_id: get(&row, "run_id")?,
            step: get::<i64>(&row, "step")? as usize,
            state: state.clone(),
            snapshot,
            created_at_ms: get::<i64>(&row, "created_at_ms")? as u64,
        };
        self.remember(thread, Cached { id, state, depth });
        Ok(Some(checkpoint))
    }

    async fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected: u64,
    ) -> Result<(), StorageError> {
        let thread = checkpoint.thread_id.clone();
        if checkpoint.version != expected + 1 {
            return Err(conflict(format!(
                "checkpoint version {} does not follow {expected}",
                checkpoint.version
            )));
        }
        // Encode against the parent: a delta if it is close to a keyframe
        // and smaller than the state.
        let parent = match &checkpoint.parent_id {
            Some(parent) => self.materialize(&thread, parent).await?,
            None => None,
        };
        let full = serde_json::to_string(&checkpoint.state).map_err(encode_error)?;
        let (kind, depth, body) = match &parent {
            Some((before, depth)) if depth + 1 < self.keyframe_interval => {
                let ops = delta::diff(before, &checkpoint.state);
                let encoded = serde_json::to_string(&ops).map_err(encode_error)?;
                if encoded.len() < full.len() {
                    ("delta", depth + 1, encoded)
                } else {
                    ("keyframe", 0, full)
                }
            }
            _ => ("keyframe", 0, full),
        };
        let snapshot = match &checkpoint.snapshot {
            Some(snapshot) => {
                let mut value = serde_json::to_value(snapshot).map_err(encode_error)?;
                // Stored once: the snapshot's state is the checkpoint's.
                if snapshot.state == checkpoint.state
                    && let Value::Object(map) = &mut value
                {
                    map.remove("state");
                }
                Some(value.to_string())
            }
            None => None,
        };

        let now = now_ms() as i64;
        let mut tx = self.begin().await?;
        let moved = if expected == 0 {
            sqlx::query(
                "INSERT INTO agentbuild_threads
                    (thread_id, head_id, head_version, created_at_ms, updated_at_ms)
                    VALUES (?1, ?2, ?3, ?4, ?4)
                    ON CONFLICT (thread_id) DO NOTHING",
            )
            .bind(&thread)
            .bind(&checkpoint.id)
            .bind(checkpoint.version as i64)
            .bind(now)
            .execute(&mut *tx)
            .await
        } else {
            sqlx::query(
                "UPDATE agentbuild_threads SET head_id = ?1, head_version = ?2, updated_at_ms = ?3
                    WHERE thread_id = ?4 AND head_version = ?5",
            )
            .bind(&checkpoint.id)
            .bind(checkpoint.version as i64)
            .bind(now)
            .bind(&thread)
            .bind(expected as i64)
            .execute(&mut *tx)
            .await
        }
        .map_err(sql_error)?
        .rows_affected();
        if moved != 1 {
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT head_version FROM agentbuild_threads WHERE thread_id = ?",
            )
            .bind(&thread)
            .fetch_optional(&mut *tx)
            .await
            .map_err(sql_error)?;
            return Err(conflict(format!(
                "thread `{thread}` is at version {}, not {expected}",
                current.unwrap_or(0)
            )));
        }
        sqlx::query(
            "INSERT INTO agentbuild_checkpoints
                (id, thread_id, parent_id, version, source, status, run_id, step, kind, depth,
                 body, snapshot, created_at_ms)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&checkpoint.id)
        .bind(&thread)
        .bind(&checkpoint.parent_id)
        .bind(checkpoint.version as i64)
        .bind(enum_text(&checkpoint.source)?)
        .bind(enum_text(&checkpoint.status())?)
        .bind(&checkpoint.run_id)
        .bind(checkpoint.step as i64)
        .bind(kind)
        .bind(depth as i64)
        .bind(body)
        .bind(snapshot)
        .bind(checkpoint.created_at_ms as i64)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        tx.commit().await.map_err(sql_error)?;
        self.remember(
            &thread,
            Cached {
                id: checkpoint.id,
                state: checkpoint.state,
                depth,
            },
        );
        Ok(())
    }

    async fn branch(
        &self,
        thread: &str,
        limit: usize,
    ) -> Result<Vec<CheckpointMeta>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "WITH RECURSIVE branch (id, n) AS (
                SELECT head_id, 0 FROM agentbuild_threads WHERE thread_id = ?1
                UNION ALL
                SELECT c.parent_id, branch.n + 1
                    FROM agentbuild_checkpoints c JOIN branch ON c.id = branch.id
                    WHERE c.parent_id IS NOT NULL AND branch.n + 1 < ?2
            )
            SELECT c.id, c.parent_id, c.version, c.source, c.status, c.run_id, c.step,
                    c.created_at_ms
                FROM branch JOIN agentbuild_checkpoints c ON c.id = branch.id
                WHERE c.thread_id = ?1
                ORDER BY branch.n",
        )
        .bind(thread)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(sql_error)?;
        rows.iter()
            .map(|row| {
                Ok(CheckpointMeta {
                    id: get(row, "id")?,
                    parent_id: get(row, "parent_id")?,
                    version: get::<i64>(row, "version")? as u64,
                    source: parse_enum(&get::<String>(row, "source")?)?,
                    status: parse_enum(&get::<String>(row, "status")?)?,
                    run_id: get(row, "run_id")?,
                    step: get::<i64>(row, "step")? as usize,
                    created_at_ms: get::<i64>(row, "created_at_ms")? as u64,
                })
            })
            .collect()
    }

    async fn delete(&self, thread: &str) -> Result<bool, StorageError> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "DELETE FROM agentbuild_pending_writes WHERE checkpoint_id IN
                (SELECT id FROM agentbuild_checkpoints WHERE thread_id = ?)",
        )
        .bind(thread)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        sqlx::query("DELETE FROM agentbuild_checkpoints WHERE thread_id = ?")
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        let existed = sqlx::query("DELETE FROM agentbuild_threads WHERE thread_id = ?")
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?
            .rows_affected()
            > 0;
        tx.commit().await.map_err(sql_error)?;
        self.cache().remove(thread);
        Ok(existed)
    }

    async fn add_pending(
        &self,
        thread: &str,
        checkpoint: &str,
        node: &str,
        write: PendingWrite,
    ) -> Result<(), StorageError> {
        let write = serde_json::to_string(&write).map_err(encode_error)?;
        let added = sqlx::query(
            "INSERT INTO agentbuild_pending_writes (checkpoint_id, node, write, created_at_ms)
                SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (
                    SELECT 1 FROM agentbuild_checkpoints
                        WHERE id = ?1 AND thread_id = ?5 AND snapshot IS NOT NULL
                )",
        )
        .bind(checkpoint)
        .bind(node)
        .bind(write)
        .bind(now_ms() as i64)
        .bind(thread)
        .execute(&self.pool)
        .await
        .map_err(sql_error)?
        .rows_affected();
        if added == 0 {
            return Err(StorageError::new(
                StorageErrorKind::CheckpointNotFound,
                format!("thread `{thread}` has no checkpoint `{checkpoint}` with a run to add to"),
            ));
        }
        Ok(())
    }
}

impl Checkpointer for SqliteCheckpointer {
    fn load<'a>(
        &'a self,
        thread: &'a str,
        at: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Checkpoint>, StorageError>> {
        Box::pin(self.load_checkpoint(thread, at))
    }

    fn commit(
        &self,
        checkpoint: Checkpoint,
        expected: u64,
    ) -> BoxFuture<'_, Result<(), StorageError>> {
        Box::pin(self.commit_checkpoint(checkpoint, expected))
    }

    fn history<'a>(
        &'a self,
        thread: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<CheckpointMeta>, StorageError>> {
        Box::pin(self.branch(thread, limit))
    }

    fn delete_thread<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(self.delete(thread))
    }

    fn put_pending<'a>(
        &'a self,
        thread: &'a str,
        checkpoint: &'a str,
        node: &'a str,
        write: PendingWrite,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(self.add_pending(thread, checkpoint, node, write))
    }
}

fn get<'r, T>(row: &'r SqliteRow, column: &str) -> Result<T, StorageError>
where
    T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite>,
{
    row.try_get(column).map_err(sql_error)
}

fn parse<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Result<T, StorageError> {
    serde_json::from_str(text).map_err(|err| corrupt(format!("unreadable {what}: {err}")))
}

/// A `Source` or `CheckpointStatus` as stored (its JSON name).
fn enum_text<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    match serde_json::to_value(value).map_err(encode_error)? {
        Value::String(text) => Ok(text),
        other => Err(corrupt(format!("unexpected enum value {other}"))),
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StorageError> {
    serde_json::from_value(Value::String(text.to_string()))
        .map_err(|err| corrupt(format!("unknown value `{text}`: {err}")))
}

fn corrupt(message: String) -> StorageError {
    StorageError::new(StorageErrorKind::Corrupt, message)
}

fn conflict(message: String) -> StorageError {
    StorageError::new(StorageErrorKind::Conflict, message)
}

fn encode_error(err: serde_json::Error) -> StorageError {
    corrupt(format!("cannot encode as JSON: {err}"))
}

fn sql_error(err: sqlx::Error) -> StorageError {
    let kind = match &err {
        sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_)
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::TypeNotFound { .. } => StorageErrorKind::Corrupt,
        _ => StorageErrorKind::Unavailable,
    };
    StorageError::new(kind, format!("SQLite: {err}"))
}

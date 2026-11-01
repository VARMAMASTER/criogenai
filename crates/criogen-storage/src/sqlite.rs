//! SQLite storage (design 0008 §4, §12, §13, §15): delta checkpoints with
//! keyframes, compare-and-set commits, automatic migrations, the memory
//! store, retention and encryption at rest.

mod audit;
mod memory;

pub use audit::SqliteAuditLog;
pub use memory::SqliteMemoryStore;

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use criogen_core::{BoxFuture, PendingWrite, State};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Row, Sqlite, Transaction};

use crate::checkpoint::{
    Checkpoint, CheckpointMeta, Checkpointer, ThreadInfo, ThreadQuery, now_ms, prune_plan,
};
use crate::crypto::Sealer;
use crate::error::{StorageError, StorageErrorKind, corrupt};
use crate::sql::{
    KEYFRAME_INTERVAL, StateCache, body_aad, conflict, decode_snapshot, encode, encode_error,
    enum_text, open_chain, parse, parse_enum, rebuild, snapshot_aad, write_aad,
};

/// Schema changes, in order. Each runs once, in a transaction, and is
/// recorded in `criogen_schema_migrations`.
const MIGRATIONS: &[&str] = &[
    // 1: threads, checkpoints and task writes.
    "CREATE TABLE criogen_threads (
        thread_id TEXT PRIMARY KEY,
        head_id TEXT NOT NULL,
        head_version INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );
    CREATE TABLE criogen_checkpoints (
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
    CREATE INDEX criogen_checkpoints_thread
        ON criogen_checkpoints (thread_id, version);
    CREATE TABLE criogen_pending_writes (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        checkpoint_id TEXT NOT NULL,
        node TEXT NOT NULL,
        write TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    );
    CREATE INDEX criogen_pending_writes_checkpoint
        ON criogen_pending_writes (checkpoint_id, seq);",
    // 2: long-term memory. `search_text` holds the item's words as
    // ` word word … ` for keyword search.
    "CREATE TABLE criogen_memory (
        namespace TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        search_text TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER,
        PRIMARY KEY (namespace, key)
    );
    CREATE INDEX criogen_memory_expires
        ON criogen_memory (expires_at_ms) WHERE expires_at_ms IS NOT NULL",
    // 3: settings (whether the database is encrypted, key checks), and
    // finding idle threads for retention.
    "CREATE TABLE criogen_settings (
        name TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE INDEX criogen_threads_updated ON criogen_threads (updated_at_ms)",
    // 4: memory vectors for semantic search (design 0008 §16).
    "ALTER TABLE criogen_memory ADD COLUMN embedding TEXT",
    // 5: the audit trail (design 0010).
    "CREATE TABLE criogen_audit (
        seq INTEGER PRIMARY KEY,
        at_ms INTEGER NOT NULL,
        action TEXT NOT NULL,
        actor TEXT,
        thread_id TEXT,
        run_id TEXT,
        subject TEXT,
        outcome TEXT,
        details TEXT NOT NULL,
        prev_hash TEXT NOT NULL,
        hash TEXT NOT NULL
    );
    CREATE INDEX criogen_audit_thread ON criogen_audit (thread_id, seq);
    CREATE INDEX criogen_audit_time ON criogen_audit (at_ms)",
    // 6: the audit trail is append-only in the database too (design 0010
    // §1.4): plain UPDATE and DELETE statements fail. Dropping a trigger
    // takes schema rights; `verify_anchor` still catches what follows.
    "CREATE TRIGGER criogen_audit_no_update BEFORE UPDATE ON criogen_audit
    BEGIN SELECT RAISE(ABORT, 'criogen_audit is append-only'); END
    --@@
    CREATE TRIGGER criogen_audit_no_delete BEFORE DELETE ON criogen_audit
    BEGIN SELECT RAISE(ABORT, 'criogen_audit is append-only'); END",
];

/// Checkpoint `?1` of thread `?2` and its ancestors up to the nearest
/// keyframe, keyframe first.
const CHAIN: &str = "WITH RECURSIVE chain (id, parent_id, kind, depth, body, n) AS (
        SELECT id, parent_id, kind, depth, body, 0
            FROM criogen_checkpoints WHERE id = ?1 AND thread_id = ?2
        UNION ALL
        SELECT c.id, c.parent_id, c.kind, c.depth, c.body, chain.n + 1
            FROM criogen_checkpoints c JOIN chain ON c.id = chain.parent_id
            WHERE chain.kind = 'delta' AND chain.n < 100000
    )
    SELECT id, kind, depth, body FROM chain ORDER BY n DESC";

/// The database file and its journal readable by their owner only (Unix;
/// elsewhere the directory's permissions apply). They hold conversations.
fn restrict_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Rows deleted or rewritten per statement or batch.
const BATCH: usize = 200;

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
/// Tables (prefixed `criogen_`) are created on first use. Deleted rows
/// are overwritten in the file (`secure_delete`), not left in free pages.
/// With [`connect_encrypted`](Self::connect_encrypted) (feature
/// `encryption`) the columns holding content are encrypted (design 0008
/// §15.2).
///
/// Needs Tokio's real clock: under a paused test clock (`start_paused`) the
/// connection pool's timeout fires at once. Tests on paused time should use
/// [`InMemoryCheckpointer`](crate::InMemoryCheckpointer).
pub struct SqliteCheckpointer {
    pool: SqlitePool,
    keyframe_interval: u32,
    cache: StateCache,
    sealer: Sealer,
}

impl std::fmt::Debug for SqliteCheckpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteCheckpointer")
            .field("keyframe_interval", &self.keyframe_interval)
            .field("encrypted", &self.sealer.is_on())
            .finish_non_exhaustive()
    }
}

impl SqliteCheckpointer {
    /// Opens (or creates) the database at `url`: `sqlite://path/to/file.db`,
    /// `sqlite:file.db`, or `sqlite::memory:` (one private in-memory
    /// database, lost when this value is dropped). Applies pending
    /// migrations. Fails with `encryption_key_missing` if the database is
    /// encrypted.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Self::open_with(url, Sealer::default()).await
    }

    /// [`connect`](Self::connect) with encryption at rest: new content is
    /// encrypted with the active key of `keys`; rows written with its older
    /// keys (or before encryption was on) still read. Fails with
    /// `encryption_key_invalid` if the database knows a key's id with other
    /// bytes.
    #[cfg(feature = "encryption")]
    pub async fn connect_encrypted(url: &str, keys: crate::Keyring) -> Result<Self, StorageError> {
        Self::open_with(url, Sealer::new(Some(keys))).await
    }

    async fn open_with(url: &str, sealer: Sealer) -> Result<Self, StorageError> {
        let in_memory = url.contains(":memory:") || url.contains("mode=memory");
        let options = SqliteConnectOptions::from_str(url).map_err(|err| {
            StorageError::new(
                StorageErrorKind::Unavailable,
                format!("invalid SQLite URL `{url}`: {err}"),
            )
        })?;
        Self::open_options(options, in_memory, sealer).await
    }

    async fn open_options(
        options: SqliteConnectOptions,
        in_memory: bool,
        sealer: Sealer,
    ) -> Result<Self, StorageError> {
        let path = options.get_filename().to_path_buf();
        let options = options
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true)
            // Deleted content is overwritten, not left in free pages.
            .pragma("secure_delete", "ON");
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
            cache: StateCache::default(),
            sealer,
        };
        checkpointer.migrate().await?;
        checkpointer.check_settings().await?;
        if !in_memory {
            restrict_permissions(&path);
        }
        Ok(checkpointer)
    }

    /// Opens (or creates) the database file at `path`. The path is used as
    /// it is: `?` and `#` in it are part of the file name, not URL options.
    pub async fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new().filename(path.as_ref());
        Self::open_options(options, false, Sealer::default()).await
    }

    /// Writes the full state at least every `interval` checkpoints of a
    /// branch (default 25, minimum 1 = no deltas).
    pub fn keyframe_interval(mut self, interval: u32) -> Self {
        self.keyframe_interval = interval.max(1);
        self
    }

    /// The memory store in the same database (same connections and keys).
    pub fn memory_store(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::new(self.pool.clone(), self.sealer.clone())
    }

    /// The audit trail in the same database (design 0010).
    pub fn audit_log(&self) -> SqliteAuditLog {
        SqliteAuditLog::new(self.pool.clone())
    }

    /// The schema version the database is at.
    pub async fn schema_version(&self) -> Result<u32, StorageError> {
        let version: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM criogen_schema_migrations")
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
            "CREATE TABLE IF NOT EXISTS criogen_schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at_ms INTEGER NOT NULL
            )",
        )
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        let current: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM criogen_schema_migrations")
                .fetch_one(&mut *tx)
                .await
                .map_err(sql_error)?;
        let known = MIGRATIONS.len() as i64;
        if current > known {
            return Err(StorageError::new(
                StorageErrorKind::Incompatible,
                format!(
                    "the database is at schema version {current}, newer than this version of \
                     Criogen supports ({known}); upgrade Criogen"
                ),
            ));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            // One statement at a time: `raw_sql`'s future is not `Send` for
            // every lifetime, which would stop callers from opening storage
            // inside `tokio::spawn`. Migrations have no `;` inside literals.
            for statement in crate::sql::statements(migration) {
                sqlx::query(statement)
                    .execute(&mut *tx)
                    .await
                    .map_err(sql_error)?;
            }
            sqlx::query(
                "INSERT INTO criogen_schema_migrations (version, applied_at_ms) VALUES (?, ?)",
            )
            .bind(index as i64 + 1)
            .bind(now_ms() as i64)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        }
        tx.commit().await.map_err(sql_error)
    }

    /// Compares the database's encryption settings with the keys, and
    /// records new key ids (design 0008 §15.2).
    async fn check_settings(&self) -> Result<(), StorageError> {
        let add = self.sealer.check_settings(&self.settings().await?)?;
        if add.is_empty() {
            return Ok(());
        }
        for (name, value) in add {
            sqlx::query(
                "INSERT INTO criogen_settings (name, value) VALUES (?, ?)
                    ON CONFLICT (name) DO NOTHING",
            )
            .bind(name)
            .bind(value)
            .execute(&self.pool)
            .await
            .map_err(sql_error)?;
        }
        // Another process may have recorded the same key id first.
        self.sealer
            .check_settings(&self.settings().await?)
            .map(drop)
    }

    async fn settings(&self) -> Result<HashMap<String, String>, StorageError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name, value FROM criogen_settings")
                .fetch_all(&self.pool)
                .await
                .map_err(sql_error)?;
        Ok(rows.into_iter().collect())
    }

    /// A write transaction that holds the write lock from the start, so it
    /// waits (busy timeout) instead of failing on a concurrent writer.
    async fn begin(&self) -> Result<Transaction<'static, Sqlite>, StorageError> {
        self.pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_error)
    }

    /// The state of checkpoint `id` and its distance from its keyframe.
    async fn materialize(
        &self,
        thread: &str,
        id: &str,
    ) -> Result<Option<(State, u32)>, StorageError> {
        if let Some(cached) = self.cache.get(thread, id) {
            return Ok(Some(cached));
        }
        let rows = sqlx::query(CHAIN)
            .bind(id)
            .bind(thread)
            .fetch_all(&self.pool)
            .await
            .map_err(sql_error)?;
        self.rebuild_rows(thread, id, &rows)
    }

    /// The state and depth of checkpoint `id` from its [`CHAIN`] rows.
    fn rebuild_rows(
        &self,
        thread: &str,
        id: &str,
        rows: &[SqliteRow],
    ) -> Result<Option<(State, u32)>, StorageError> {
        let Some(last) = rows.last() else {
            return Ok(None);
        };
        let depth = get::<i64>(last, "depth")? as u32;
        let rows = rows
            .iter()
            .map(|row| Ok((get(row, "id")?, get(row, "kind")?, get(row, "body")?)))
            .collect::<Result<Vec<_>, StorageError>>()?;
        let chain = open_chain(&self.sealer, thread, rows)?;
        Ok(Some((rebuild(id, &chain)?, depth)))
    }

    async fn pending_writes(
        &self,
        thread: &str,
        checkpoint: &str,
    ) -> Result<Vec<(String, PendingWrite)>, StorageError> {
        let rows = sqlx::query(
            "SELECT node, write FROM criogen_pending_writes
                WHERE checkpoint_id = ? ORDER BY seq",
        )
        .bind(checkpoint)
        .fetch_all(&self.pool)
        .await
        .map_err(sql_error)?;
        rows.iter()
            .map(|row| {
                let node: String = get(row, "node")?;
                let write = self.sealer.open(
                    &write_aad(thread, checkpoint, &node),
                    get(row, "write")?,
                    "task write",
                )?;
                Ok((node, parse(&write, "task write")?))
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
                let head: Option<String> =
                    sqlx::query_scalar("SELECT head_id FROM criogen_threads WHERE thread_id = ?")
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
                FROM criogen_checkpoints WHERE id = ? AND thread_id = ?",
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
            Some(stored) => {
                let text = self
                    .sealer
                    .open(&snapshot_aad(thread, &id), stored, "snapshot")?;
                Some(decode_snapshot(
                    &text,
                    &state,
                    self.pending_writes(thread, &id).await?,
                )?)
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
        self.cache.remember(thread, id, state, depth);
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
        let encoded = encode(&checkpoint, parent.as_ref(), self.keyframe_interval)?;
        let body = self
            .sealer
            .seal(&body_aad(&thread, &checkpoint.id), encoded.body)?;
        let snapshot = self
            .sealer
            .seal_opt(&snapshot_aad(&thread, &checkpoint.id), encoded.snapshot)?;

        let now = now_ms() as i64;
        let mut tx = self.begin().await?;
        let moved = if expected == 0 {
            sqlx::query(
                "INSERT INTO criogen_threads
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
                "UPDATE criogen_threads SET head_id = ?1, head_version = ?2, updated_at_ms = ?3
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
            let current: Option<i64> =
                sqlx::query_scalar("SELECT head_version FROM criogen_threads WHERE thread_id = ?")
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
            "INSERT INTO criogen_checkpoints
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
        .bind(encoded.kind)
        .bind(encoded.depth as i64)
        .bind(body)
        .bind(snapshot)
        .bind(checkpoint.created_at_ms as i64)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        tx.commit().await.map_err(sql_error)?;
        self.cache
            .remember(&thread, checkpoint.id, checkpoint.state, encoded.depth);
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
                SELECT head_id, 0 FROM criogen_threads WHERE thread_id = ?1
                UNION ALL
                SELECT c.parent_id, branch.n + 1
                    FROM criogen_checkpoints c JOIN branch ON c.id = branch.id
                    WHERE c.parent_id IS NOT NULL AND branch.n + 1 < ?2
            )
            SELECT c.id, c.parent_id, c.version, c.source, c.status, c.run_id, c.step,
                    c.created_at_ms
                FROM branch JOIN criogen_checkpoints c ON c.id = branch.id
                WHERE c.thread_id = ?1
                ORDER BY branch.n",
        )
        .bind(thread)
        .bind(limit.min(i64::MAX as usize) as i64)
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
            "DELETE FROM criogen_pending_writes WHERE checkpoint_id IN
                (SELECT id FROM criogen_checkpoints WHERE thread_id = ?)",
        )
        .bind(thread)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        sqlx::query("DELETE FROM criogen_checkpoints WHERE thread_id = ?")
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        let existed = sqlx::query("DELETE FROM criogen_threads WHERE thread_id = ?")
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?
            .rows_affected()
            > 0;
        tx.commit().await.map_err(sql_error)?;
        self.cache.forget(thread);
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
        let write = self
            .sealer
            .seal(&write_aad(thread, checkpoint, node), write)?;
        let added = sqlx::query(
            "INSERT INTO criogen_pending_writes (checkpoint_id, node, write, created_at_ms)
                SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (
                    SELECT 1 FROM criogen_checkpoints
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

    async fn list_threads(&self, query: &ThreadQuery) -> Result<Vec<ThreadInfo>, StorageError> {
        let rows = sqlx::query(
            "SELECT thread_id, head_version, created_at_ms, updated_at_ms
                FROM criogen_threads
                WHERE (?1 IS NULL OR thread_id > ?1)
                    AND (?2 IS NULL OR updated_at_ms < ?2)
                    AND (?3 IS NULL OR updated_at_ms >= ?3)
                ORDER BY thread_id LIMIT ?4",
        )
        .bind(&query.after)
        .bind(query.updated_before.map(clamp))
        .bind(query.updated_after.map(clamp))
        .bind(query.limit.min(i64::MAX as usize) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(sql_error)?;
        rows.iter()
            .map(|row| {
                Ok(ThreadInfo {
                    thread_id: get(row, "thread_id")?,
                    version: get::<i64>(row, "head_version")? as u64,
                    created_at_ms: get::<i64>(row, "created_at_ms")? as u64,
                    updated_at_ms: get::<i64>(row, "updated_at_ms")? as u64,
                })
            })
            .collect()
    }

    /// [`Checkpointer::prune_thread`], in one transaction (design 0008
    /// §15.1).
    async fn prune(&self, thread: &str, keep: Option<usize>) -> Result<u64, StorageError> {
        let mut tx = self.begin().await?;
        let head: Option<String> =
            sqlx::query_scalar("SELECT head_id FROM criogen_threads WHERE thread_id = ?")
                .bind(thread)
                .fetch_optional(&mut *tx)
                .await
                .map_err(sql_error)?;
        let Some(head) = head else {
            return Ok(0);
        };
        let links: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id, parent_id FROM criogen_checkpoints WHERE thread_id = ?")
                .bind(thread)
                .fetch_all(&mut *tx)
                .await
                .map_err(sql_error)?;
        let (kept, deleted) = prune_plan(&head, &links, keep);
        if deleted.is_empty() {
            return Ok(0);
        }
        // The oldest kept checkpoint becomes the start of history: a
        // keyframe if it was a delta on a deleted ancestor.
        if let Some(oldest) = kept.last() {
            let rows = sqlx::query(CHAIN)
                .bind(oldest)
                .bind(thread)
                .fetch_all(&mut *tx)
                .await
                .map_err(sql_error)?;
            if rows.len() > 1 {
                let (state, _) = self
                    .rebuild_rows(thread, oldest, &rows)?
                    .ok_or_else(|| corrupt(format!("checkpoint `{oldest}` has no rows")))?;
                let body = serde_json::to_string(&state).map_err(encode_error)?;
                let body = self.sealer.seal(&body_aad(thread, oldest), body)?;
                sqlx::query(
                    "UPDATE criogen_checkpoints SET kind = 'keyframe', depth = 0, body = ?
                        WHERE id = ?",
                )
                .bind(body)
                .bind(oldest)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?;
            }
            sqlx::query("UPDATE criogen_checkpoints SET parent_id = NULL WHERE id = ?")
                .bind(oldest)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?;
        }
        for chunk in deleted.chunks(BATCH) {
            let marks = vec!["?"; chunk.len()].join(", ");
            for table in [
                format!("DELETE FROM criogen_pending_writes WHERE checkpoint_id IN ({marks})"),
                format!("DELETE FROM criogen_checkpoints WHERE id IN ({marks})"),
            ] {
                let mut statement = sqlx::query(&table);
                for id in chunk {
                    statement = statement.bind(id);
                }
                statement.execute(&mut *tx).await.map_err(sql_error)?;
            }
        }
        tx.commit().await.map_err(sql_error)?;
        self.cache.forget(thread);
        Ok(deleted.len() as u64)
    }

    /// Rewrites every row that is plain or encrypted with an older key, with
    /// the active key (design 0008 §15.2). Each rewrite is compare-and-set
    /// on the old text, so a concurrent write wins. Fails with
    /// `encryption_key_missing` without keys.
    #[cfg(feature = "encryption")]
    pub async fn reencrypt(&self) -> Result<crate::ReencryptReport, StorageError> {
        let Some(prefix) = self.sealer.active_prefix() else {
            return Err(crate::crypto::key_missing(
                "reencrypt needs keys: open the storage with `connect_encrypted`",
            ));
        };
        let migrating = self.sealer.for_migration();
        let mut report = crate::ReencryptReport::default();
        for column in ["body", "snapshot"] {
            let aad = if column == "body" {
                body_aad
            } else {
                snapshot_aad
            };
            let mut after = String::new();
            loop {
                let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
                    "SELECT id, thread_id, {column} FROM criogen_checkpoints
                        WHERE id > ?1 AND {column} IS NOT NULL
                            AND substr({column}, 1, length(?2)) <> ?2
                        ORDER BY id LIMIT ?3"
                ))
                .bind(&after)
                .bind(&prefix)
                .bind(BATCH as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(sql_error)?;
                let Some(last) = rows.last() else { break };
                after = last.0.clone();
                for (id, thread, stored) in rows {
                    let aad = aad(&thread, &id);
                    let plain = migrating.open(&aad, stored.clone(), column)?;
                    let sealed = self.sealer.seal(&aad, plain)?;
                    report.checkpoints += sqlx::query(&format!(
                        "UPDATE criogen_checkpoints SET {column} = ?1
                            WHERE id = ?2 AND {column} = ?3"
                    ))
                    .bind(sealed)
                    .bind(&id)
                    .bind(stored)
                    .execute(&self.pool)
                    .await
                    .map_err(sql_error)?
                    .rows_affected();
                }
            }
        }
        let mut after = 0i64;
        loop {
            let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
                "SELECT w.seq, w.checkpoint_id, c.thread_id, w.node, w.write
                    FROM criogen_pending_writes w
                    JOIN criogen_checkpoints c ON c.id = w.checkpoint_id
                    WHERE w.seq > ?1 AND substr(w.write, 1, length(?2)) <> ?2
                    ORDER BY w.seq LIMIT ?3",
            )
            .bind(after)
            .bind(&prefix)
            .bind(BATCH as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(sql_error)?;
            let Some(last) = rows.last() else { break };
            after = last.0;
            for (seq, checkpoint, thread, node, stored) in rows {
                let aad = write_aad(&thread, &checkpoint, &node);
                let plain = migrating.open(&aad, stored, "task write")?;
                let sealed = self.sealer.seal(&aad, plain)?;
                report.task_writes +=
                    sqlx::query("UPDATE criogen_pending_writes SET write = ? WHERE seq = ?")
                        .bind(sealed)
                        .bind(seq)
                        .execute(&self.pool)
                        .await
                        .map_err(sql_error)?
                        .rows_affected();
            }
        }
        report.memory_items = self.memory_store().reencrypt(&prefix).await?;
        Ok(report)
    }
}

/// A time for an `INTEGER` column.
fn clamp(ms: u64) -> i64 {
    ms.min(i64::MAX as u64) as i64
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

    fn threads<'a>(
        &'a self,
        query: &'a ThreadQuery,
    ) -> BoxFuture<'a, Result<Vec<ThreadInfo>, StorageError>> {
        Box::pin(self.list_threads(query))
    }

    fn prune_thread<'a>(
        &'a self,
        thread: &'a str,
        keep: Option<usize>,
    ) -> BoxFuture<'a, Result<u64, StorageError>> {
        Box::pin(self.prune(thread, keep))
    }
}

impl crate::storage::Backend for SqliteCheckpointer {
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(SqliteCheckpointer::close(self))
    }

    #[cfg(feature = "encryption")]
    fn reencrypt(&self) -> BoxFuture<'_, Result<crate::ReencryptReport, StorageError>> {
        Box::pin(SqliteCheckpointer::reencrypt(self))
    }
}

fn get<'r, T>(row: &'r SqliteRow, column: &str) -> Result<T, StorageError>
where
    T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite>,
{
    row.try_get(column).map_err(sql_error)
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

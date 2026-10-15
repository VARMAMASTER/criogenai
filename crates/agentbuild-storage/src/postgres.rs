//! Postgres storage (design 0008 §4, §5.2, §14, §15): the SQLite format
//! and guarantees on a server, for several processes and machines, plus
//! thread locks across processes, retention and encryption at rest.

mod locks;
mod memory;

pub use locks::PostgresThreadLocks;
pub use memory::PostgresMemoryStore;

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use agentbuild_core::{BoxFuture, PendingWrite, State};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Postgres, Row, Transaction};

use crate::checkpoint::{
    Checkpoint, CheckpointMeta, Checkpointer, ThreadInfo, ThreadQuery, now_ms, prune_plan,
};
use crate::crypto::Sealer;
use crate::error::{StorageError, StorageErrorKind, corrupt};
use crate::sql::{
    KEYFRAME_INTERVAL, StateCache, body_aad, conflict, decode_snapshot, encode, encode_error,
    enum_text, open_chain, parse, parse_enum, rebuild, snapshot_aad, write_aad,
};

/// Schema changes, in order; the same versions as SQLite's. Each runs once,
/// in a transaction, and is recorded in `agentbuild_schema_migrations`.
/// Integers are `BIGINT`; JSON is stored as `TEXT` (as written, like
/// SQLite; `jsonb` would refuse `\u0000` in strings).
const MIGRATIONS: &[&str] = &[
    // 1: threads, checkpoints and task writes.
    "CREATE TABLE agentbuild_threads (
        thread_id TEXT PRIMARY KEY,
        head_id TEXT NOT NULL,
        head_version BIGINT NOT NULL,
        created_at_ms BIGINT NOT NULL,
        updated_at_ms BIGINT NOT NULL
    );
    CREATE TABLE agentbuild_checkpoints (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        parent_id TEXT,
        version BIGINT NOT NULL,
        source TEXT NOT NULL,
        status TEXT NOT NULL,
        run_id TEXT NOT NULL,
        step BIGINT NOT NULL,
        kind TEXT NOT NULL,
        depth BIGINT NOT NULL,
        body TEXT NOT NULL,
        snapshot TEXT,
        created_at_ms BIGINT NOT NULL
    );
    CREATE INDEX agentbuild_checkpoints_thread
        ON agentbuild_checkpoints (thread_id, version);
    CREATE TABLE agentbuild_pending_writes (
        seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
        checkpoint_id TEXT NOT NULL,
        node TEXT NOT NULL,
        write TEXT NOT NULL,
        created_at_ms BIGINT NOT NULL
    );
    CREATE INDEX agentbuild_pending_writes_checkpoint
        ON agentbuild_pending_writes (checkpoint_id, seq)",
    // 2: long-term memory. Byte order (`C`) for namespaces and keys: the
    // same order as SQLite, and a namespace prefix is an index range.
    "CREATE TABLE agentbuild_memory (
        namespace TEXT COLLATE \"C\" NOT NULL,
        key TEXT COLLATE \"C\" NOT NULL,
        value TEXT NOT NULL,
        search_text TEXT NOT NULL,
        created_at_ms BIGINT NOT NULL,
        updated_at_ms BIGINT NOT NULL,
        expires_at_ms BIGINT,
        PRIMARY KEY (namespace, key)
    );
    CREATE INDEX agentbuild_memory_expires
        ON agentbuild_memory (expires_at_ms) WHERE expires_at_ms IS NOT NULL",
    // 3: settings (whether the database is encrypted, key checks), and
    // finding idle threads for retention.
    "CREATE TABLE agentbuild_settings (
        name TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE INDEX agentbuild_threads_updated ON agentbuild_threads (updated_at_ms)",
    // 4: memory vectors for semantic search (design 0008 §16).
    "ALTER TABLE agentbuild_memory ADD COLUMN embedding TEXT",
];

/// Checkpoint `$1` of thread `$2` and its ancestors up to the nearest
/// keyframe, keyframe first.
const CHAIN: &str = "WITH RECURSIVE chain (id, parent_id, kind, depth, body, n) AS (
        SELECT id, parent_id, kind, depth, body, CAST(0 AS BIGINT)
            FROM agentbuild_checkpoints WHERE id = $1 AND thread_id = $2
        UNION ALL
        SELECT c.id, c.parent_id, c.kind, c.depth, c.body, chain.n + 1
            FROM agentbuild_checkpoints c JOIN chain ON c.id = chain.parent_id
            WHERE chain.kind = 'delta'
    )
    SELECT id, kind, depth, body FROM chain ORDER BY n DESC";

/// Rows deleted or rewritten per statement or batch.
const BATCH: usize = 200;

/// Connection settings for [`PostgresCheckpointer::connect_with`].
#[derive(Debug, Clone)]
pub struct PostgresOptions {
    max_connections: u32,
    lock_connections: u32,
    acquire_timeout: Duration,
    sealer: Sealer,
}

impl Default for PostgresOptions {
    fn default() -> Self {
        Self {
            max_connections: 10,
            lock_connections: 32,
            acquire_timeout: Duration::from_secs(30),
            sealer: Sealer::default(),
        }
    }
}

impl PostgresOptions {
    /// The defaults: 10 connections for reads and writes, up to 32 for
    /// thread locks, 30 s to get a connection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Connections for reading and writing checkpoints and memory.
    pub fn max_connections(mut self, n: u32) -> Self {
        self.max_connections = n.max(1);
        self
    }

    /// Connections for thread locks ([`PostgresThreadLocks`]), opened as
    /// needed. Each thread with a run in this process (or waiting for one)
    /// holds one for that time, so this caps the threads one process runs
    /// at once.
    pub fn lock_connections(mut self, n: u32) -> Self {
        self.lock_connections = n.max(1);
        self
    }

    /// How long an operation waits for a free connection before failing
    /// with `storage_unavailable`.
    pub fn acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Encrypts content at rest with the active key of `keys` (feature
    /// `encryption`, design 0008 §15.2).
    #[cfg(feature = "encryption")]
    pub fn encryption(mut self, keys: crate::Keyring) -> Self {
        self.sealer = Sealer::new(Some(keys));
        self
    }
}

/// Checkpoints in a Postgres database: for services with several processes
/// or machines. Same format as [`SqliteCheckpointer`](crate::SqliteCheckpointer)
/// (deltas with keyframes, compare-and-set commits, versioned migrations
/// that run on connect). Postgres 13 or newer.
///
/// Use it through [`Storage::connect`](crate::Storage::connect) with a
/// `postgres://` URL, which adds [`PostgresMemoryStore`] and
/// [`PostgresThreadLocks`] (so `OnBusy` holds across processes). TLS:
/// `?sslmode=require` (or `verify-full` with `sslrootcert=…`) in the URL.
///
/// Thread locks are session advisory locks: connect directly or through a
/// pooler in session mode (PgBouncer's transaction mode would hand the
/// session to someone else).
///
/// With [`PostgresOptions::encryption`] the columns holding content are
/// encrypted (design 0008 §15.2).
pub struct PostgresCheckpointer {
    pool: PgPool,
    lock_pool: PgPool,
    keyframe_interval: u32,
    cache: StateCache,
    sealer: Sealer,
}

impl std::fmt::Debug for PostgresCheckpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresCheckpointer")
            .field("keyframe_interval", &self.keyframe_interval)
            .field("encrypted", &self.sealer.is_on())
            .finish_non_exhaustive()
    }
}

impl PostgresCheckpointer {
    /// Connects to `url` (`postgres://user:password@host:5432/database`)
    /// with default [`PostgresOptions`] and applies pending migrations.
    /// Fails with `storage_unavailable` if the server cannot be reached, and
    /// with `encryption_key_missing` if the database is encrypted.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Self::connect_with(url, PostgresOptions::default()).await
    }

    /// [`connect`](Self::connect) with `options`.
    pub async fn connect_with(url: &str, options: PostgresOptions) -> Result<Self, StorageError> {
        // The URL is not repeated in errors: it may hold a password.
        let connect = PgConnectOptions::from_str(url).map_err(|err| {
            StorageError::new(
                StorageErrorKind::Unavailable,
                format!("invalid Postgres URL: {err}"),
            )
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(options.acquire_timeout)
            .connect_with(connect.clone())
            .await
            .map_err(sql_error)?;
        let lock_pool = PgPoolOptions::new()
            .max_connections(options.lock_connections)
            .acquire_timeout(options.acquire_timeout)
            .connect_lazy_with(connect);
        let checkpointer = Self {
            pool,
            lock_pool,
            keyframe_interval: KEYFRAME_INTERVAL,
            cache: StateCache::default(),
            sealer: options.sealer,
        };
        checkpointer.migrate().await?;
        checkpointer.check_settings().await?;
        Ok(checkpointer)
    }

    /// Writes the full state at least every `interval` checkpoints of a
    /// branch (default 25, minimum 1 = no deltas).
    pub fn keyframe_interval(mut self, interval: u32) -> Self {
        self.keyframe_interval = interval.max(1);
        self
    }

    /// The memory store in the same database (same connections).
    pub fn memory_store(&self) -> PostgresMemoryStore {
        PostgresMemoryStore::new(self.pool.clone(), self.sealer.clone())
    }

    /// Thread locks across processes on the same database.
    pub fn thread_locks(&self) -> PostgresThreadLocks {
        PostgresThreadLocks::new(self.lock_pool.clone())
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

    /// Closes the connections (also those of its memory store and thread
    /// locks). Later calls fail with `storage_unavailable`.
    pub async fn close(&self) {
        self.lock_pool.close().await;
        self.pool.close().await;
    }

    async fn migrate(&self) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(sql_error)?;
        // One process migrates at a time; the others wait here, then find
        // nothing left to do.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('agentbuild/migrate', 0))")
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agentbuild_schema_migrations (
                version BIGINT PRIMARY KEY,
                applied_at_ms BIGINT NOT NULL
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
            // One statement at a time (prepared statements); migrations have
            // no `;` inside literals.
            for statement in migration.split(';').map(str::trim) {
                if !statement.is_empty() {
                    sqlx::query(statement)
                        .execute(&mut *tx)
                        .await
                        .map_err(sql_error)?;
                }
            }
            sqlx::query(
                "INSERT INTO agentbuild_schema_migrations (version, applied_at_ms)
                    VALUES ($1, $2)",
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
                "INSERT INTO agentbuild_settings (name, value) VALUES ($1, $2)
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
            sqlx::query_as("SELECT name, value FROM agentbuild_settings")
                .fetch_all(&self.pool)
                .await
                .map_err(sql_error)?;
        Ok(rows.into_iter().collect())
    }

    async fn begin(&self) -> Result<Transaction<'static, Postgres>, StorageError> {
        self.pool.begin().await.map_err(sql_error)
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
        rows: &[PgRow],
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
            "SELECT node, write FROM agentbuild_pending_writes
                WHERE checkpoint_id = $1 ORDER BY seq",
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
                let head: Option<String> = sqlx::query_scalar(
                    "SELECT head_id FROM agentbuild_threads WHERE thread_id = $1",
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
                FROM agentbuild_checkpoints WHERE id = $1 AND thread_id = $2",
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
        // Compare-and-set. A concurrent writer's UPDATE waits for this
        // transaction, then sees the new version and changes nothing.
        let moved = if expected == 0 {
            sqlx::query(
                "INSERT INTO agentbuild_threads
                    (thread_id, head_id, head_version, created_at_ms, updated_at_ms)
                    VALUES ($1, $2, $3, $4, $4)
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
                "UPDATE agentbuild_threads SET head_id = $1, head_version = $2, updated_at_ms = $3
                    WHERE thread_id = $4 AND head_version = $5",
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
                "SELECT head_version FROM agentbuild_threads WHERE thread_id = $1",
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
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
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
                SELECT head_id, CAST(0 AS BIGINT) FROM agentbuild_threads WHERE thread_id = $1
                UNION ALL
                SELECT c.parent_id, branch.n + 1
                    FROM agentbuild_checkpoints c JOIN branch ON c.id = branch.id
                    WHERE c.parent_id IS NOT NULL AND branch.n + 1 < $2
            )
            SELECT c.id, c.parent_id, c.version, c.source, c.status, c.run_id, c.step,
                    c.created_at_ms
                FROM branch JOIN agentbuild_checkpoints c ON c.id = branch.id
                WHERE c.thread_id = $1
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
            "DELETE FROM agentbuild_pending_writes WHERE checkpoint_id IN
                (SELECT id FROM agentbuild_checkpoints WHERE thread_id = $1)",
        )
        .bind(thread)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        sqlx::query("DELETE FROM agentbuild_checkpoints WHERE thread_id = $1")
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        let existed = sqlx::query("DELETE FROM agentbuild_threads WHERE thread_id = $1")
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
            "INSERT INTO agentbuild_pending_writes (checkpoint_id, node, write, created_at_ms)
                SELECT $1::TEXT, $2::TEXT, $3::TEXT, $4::BIGINT WHERE EXISTS (
                    SELECT 1 FROM agentbuild_checkpoints
                        WHERE id = $1 AND thread_id = $5 AND snapshot IS NOT NULL
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
                FROM agentbuild_threads
                WHERE ($1::TEXT IS NULL OR thread_id > $1)
                    AND ($2::BIGINT IS NULL OR updated_at_ms < $2)
                    AND ($3::BIGINT IS NULL OR updated_at_ms >= $3)
                ORDER BY thread_id LIMIT $4",
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

    /// [`Checkpointer::prune_thread`], in one transaction that locks the
    /// thread's row: commits on the thread wait for it (design 0008 §15.1).
    async fn prune(&self, thread: &str, keep: Option<usize>) -> Result<u64, StorageError> {
        let mut tx = self.begin().await?;
        let head: Option<String> = sqlx::query_scalar(
            "SELECT head_id FROM agentbuild_threads WHERE thread_id = $1 FOR UPDATE",
        )
        .bind(thread)
        .fetch_optional(&mut *tx)
        .await
        .map_err(sql_error)?;
        let Some(head) = head else {
            return Ok(0);
        };
        let links: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id, parent_id FROM agentbuild_checkpoints WHERE thread_id = $1")
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
                    "UPDATE agentbuild_checkpoints SET kind = 'keyframe', depth = 0, body = $1
                        WHERE id = $2",
                )
                .bind(body)
                .bind(oldest)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?;
            }
            sqlx::query("UPDATE agentbuild_checkpoints SET parent_id = NULL WHERE id = $1")
                .bind(oldest)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?;
        }
        for chunk in deleted.chunks(BATCH) {
            sqlx::query("DELETE FROM agentbuild_pending_writes WHERE checkpoint_id = ANY($1)")
                .bind(chunk)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?;
            sqlx::query("DELETE FROM agentbuild_checkpoints WHERE id = ANY($1)")
                .bind(chunk)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?;
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
                "reencrypt needs keys: connect with `PostgresOptions::encryption`",
            ));
        };
        let mut report = crate::ReencryptReport::default();
        for column in ["body", "snapshot"] {
            let aad = if column == "body" {
                body_aad
            } else {
                snapshot_aad
            };
            let mut after = String::new();
            let select = format!(
                "SELECT id, thread_id, {column} FROM agentbuild_checkpoints
                    WHERE id > $1 AND {column} IS NOT NULL
                        AND substr({column}, 1, length($2)) <> $2
                    ORDER BY id LIMIT $3"
            );
            let update = format!(
                "UPDATE agentbuild_checkpoints SET {column} = $1 WHERE id = $2 AND {column} = $3"
            );
            loop {
                let rows: Vec<(String, String, String)> = sqlx::query_as(&select)
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
                    let plain = self.sealer.open(&aad, stored.clone(), column)?;
                    let sealed = self.sealer.seal(&aad, plain)?;
                    report.checkpoints += sqlx::query(&update)
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
                    FROM agentbuild_pending_writes w
                    JOIN agentbuild_checkpoints c ON c.id = w.checkpoint_id
                    WHERE w.seq > $1 AND substr(w.write, 1, length($2)) <> $2
                    ORDER BY w.seq LIMIT $3",
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
                let plain = self.sealer.open(&aad, stored, "task write")?;
                let sealed = self.sealer.seal(&aad, plain)?;
                report.task_writes +=
                    sqlx::query("UPDATE agentbuild_pending_writes SET write = $1 WHERE seq = $2")
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

/// A time for a `BIGINT` column.
fn clamp(ms: u64) -> i64 {
    ms.min(i64::MAX as u64) as i64
}

impl Checkpointer for PostgresCheckpointer {
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

impl crate::storage::Reencrypt for PostgresCheckpointer {
    #[cfg(feature = "encryption")]
    fn reencrypt(&self) -> BoxFuture<'_, Result<crate::ReencryptReport, StorageError>> {
        Box::pin(PostgresCheckpointer::reencrypt(self))
    }
}

fn get<'r, T>(row: &'r PgRow, column: &str) -> Result<T, StorageError>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(column).map_err(sql_error)
}

/// Postgres error codes for text it cannot store: a NUL character, or a
/// character the database encoding lacks; and a row over a size limit
/// (e.g. an index entry).
const UNSTORABLE: [&str; 3] = ["22021", "22P05", "54000"];

fn sql_error(err: sqlx::Error) -> StorageError {
    let kind = match &err {
        sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_)
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::TypeNotFound { .. } => StorageErrorKind::Corrupt,
        sqlx::Error::Database(db) if db.code().is_some_and(|c| UNSTORABLE.contains(&&*c)) => {
            StorageErrorKind::InvalidValue
        }
        _ => StorageErrorKind::Unavailable,
    };
    StorageError::new(kind, format!("Postgres: {err}"))
}

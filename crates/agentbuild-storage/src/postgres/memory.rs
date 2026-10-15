use std::time::Duration;

use agentbuild_core::BoxFuture;
use serde_json::Value;
use sqlx::postgres::{PgPool, PgRow};

use super::{get, sql_error};
use crate::checkpoint::now_ms;
use crate::error::StorageError;
use crate::sql::memory_item;
use crate::store::{
    MemoryItem, MemoryStore, Namespace, SearchQuery, check_prefix, check_put, expiry, query_terms,
    rank, search_text,
};

/// Long-term memory in a Postgres database (table `agentbuild_memory`).
/// Usually from [`PostgresCheckpointer::memory_store`](crate::PostgresCheckpointer::memory_store)
/// or [`Storage::connect`](crate::Storage::connect), sharing the database
/// with threads.
///
/// Keyword search narrows the rows in SQL (items containing any query word)
/// and ranks them as every backend does (see [`MemoryStore`]), so results
/// match SQLite's and the in-memory store's exactly.
#[derive(Clone)]
pub struct PostgresMemoryStore {
    pool: PgPool,
}

impl std::fmt::Debug for PostgresMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresMemoryStore")
            .finish_non_exhaustive()
    }
}

/// "namespace is `$1` or inside it": `$2` is `$1` + `/`, `$3` is `$1` +
/// `0` (the byte after `/`), so the inside is the index range [`$2`, `$3`)
/// in byte order. The root matches everything.
const IN_PREFIX: &str = "($1 = '' OR namespace = $1 OR (namespace >= $2 AND namespace < $3))";

const COLUMNS: &str =
    "namespace, key, value, search_text, created_at_ms, updated_at_ms, expires_at_ms";

impl PostgresMemoryStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connects to the database at `url` (as
    /// [`PostgresCheckpointer::connect`](crate::PostgresCheckpointer::connect)).
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Ok(crate::PostgresCheckpointer::connect(url)
            .await?
            .memory_store())
    }

    async fn put_item(
        &self,
        namespace: &Namespace,
        key: &str,
        value: Value,
        ttl: Option<Duration>,
    ) -> Result<(), StorageError> {
        let text = check_put(namespace, key, &value)?;
        let now = now_ms();
        // Overwriting an expired item starts a new one (new created_at).
        sqlx::query(
            "INSERT INTO agentbuild_memory
                (namespace, key, value, search_text, created_at_ms, updated_at_ms, expires_at_ms)
             VALUES ($1, $2, $3, $4, $5, $5, $6)
             ON CONFLICT (namespace, key) DO UPDATE SET
                value = EXCLUDED.value,
                search_text = EXCLUDED.search_text,
                created_at_ms = CASE
                    WHEN agentbuild_memory.expires_at_ms IS NOT NULL
                        AND agentbuild_memory.expires_at_ms <= EXCLUDED.updated_at_ms
                    THEN EXCLUDED.created_at_ms
                    ELSE agentbuild_memory.created_at_ms END,
                updated_at_ms = EXCLUDED.updated_at_ms,
                expires_at_ms = EXCLUDED.expires_at_ms",
        )
        .bind(namespace.path())
        .bind(key)
        .bind(text)
        .bind(search_text(key, &value))
        .bind(now as i64)
        .bind(expiry(now, ttl).map(|at| at.min(i64::MAX as u64) as i64))
        .execute(&self.pool)
        .await
        .map_err(sql_error)?;
        Ok(())
    }

    async fn get_item(
        &self,
        namespace: &Namespace,
        key: &str,
    ) -> Result<Option<MemoryItem>, StorageError> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM agentbuild_memory
             WHERE namespace = $1 AND key = $2
                AND (expires_at_ms IS NULL OR expires_at_ms > $3)"
        ))
        .bind(namespace.path())
        .bind(key)
        .bind(now_ms() as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(sql_error)?;
        row.map(|row| read_item(&row).map(|(item, _)| item))
            .transpose()
    }

    async fn search_items(
        &self,
        prefix: &Namespace,
        query: &SearchQuery,
    ) -> Result<Vec<MemoryItem>, StorageError> {
        let terms = query_terms(query);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM agentbuild_memory
             WHERE {IN_PREFIX} AND (expires_at_ms IS NULL OR expires_at_ms > $4)"
        );
        if terms.is_empty() {
            sql.push_str(" ORDER BY updated_at_ms DESC, namespace, key LIMIT $5");
        } else {
            // Rows containing any query word (or a word starting with it);
            // all of them are ranked below.
            let any: Vec<String> = (0..terms.len())
                .map(|i| format!("strpos(search_text, ${}) > 0", i + 5))
                .collect();
            sql.push_str(&format!(" AND ({})", any.join(" OR ")));
        }
        let path = prefix.path();
        let mut select = sqlx::query(&sql)
            .bind(path.clone())
            .bind(format!("{path}/"))
            .bind(format!("{path}0"))
            .bind(now_ms() as i64);
        if terms.is_empty() {
            select = select.bind(query.limit.min(i64::MAX as usize) as i64);
        } else {
            for term in &terms {
                select = select.bind(format!(" {term}"));
            }
        }
        let rows = select.fetch_all(&self.pool).await.map_err(sql_error)?;
        let candidates = rows.iter().map(read_item).collect::<Result<Vec<_>, _>>()?;
        Ok(rank(candidates, &terms, query.limit))
    }

    async fn delete_item(&self, namespace: &Namespace, key: &str) -> Result<bool, StorageError> {
        let live: Option<bool> = sqlx::query_scalar(
            "DELETE FROM agentbuild_memory WHERE namespace = $1 AND key = $2
             RETURNING expires_at_ms IS NULL OR expires_at_ms > $3",
        )
        .bind(namespace.path())
        .bind(key)
        .bind(now_ms() as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(sql_error)?;
        Ok(live.unwrap_or(false))
    }

    async fn delete_prefix(&self, prefix: &Namespace) -> Result<u64, StorageError> {
        check_prefix(prefix)?;
        let path = prefix.path();
        let done = sqlx::query(&format!("DELETE FROM agentbuild_memory WHERE {IN_PREFIX}"))
            .bind(path.clone())
            .bind(format!("{path}/"))
            .bind(format!("{path}0"))
            .execute(&self.pool)
            .await
            .map_err(sql_error)?;
        Ok(done.rows_affected())
    }

    async fn prune(&self) -> Result<u64, StorageError> {
        let done = sqlx::query(
            "DELETE FROM agentbuild_memory
             WHERE expires_at_ms IS NOT NULL AND expires_at_ms <= $1",
        )
        .bind(now_ms() as i64)
        .execute(&self.pool)
        .await
        .map_err(sql_error)?;
        Ok(done.rows_affected())
    }
}

/// An item and its search text.
fn read_item(row: &PgRow) -> Result<(MemoryItem, String), StorageError> {
    let item = memory_item(
        &get::<String>(row, "namespace")?,
        get(row, "key")?,
        &get::<String>(row, "value")?,
        get(row, "created_at_ms")?,
        get(row, "updated_at_ms")?,
        get(row, "expires_at_ms")?,
    )?;
    Ok((item, get(row, "search_text")?))
}

impl MemoryStore for PostgresMemoryStore {
    fn put<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
        value: Value,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(self.put_item(namespace, key, value, ttl))
    }

    fn get<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<MemoryItem>, StorageError>> {
        Box::pin(self.get_item(namespace, key))
    }

    fn search<'a>(
        &'a self,
        prefix: &'a Namespace,
        query: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<MemoryItem>, StorageError>> {
        Box::pin(self.search_items(prefix, query))
    }

    fn delete<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
    ) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(self.delete_item(namespace, key))
    }

    fn delete_namespace<'a>(
        &'a self,
        prefix: &'a Namespace,
    ) -> BoxFuture<'a, Result<u64, StorageError>> {
        Box::pin(self.delete_prefix(prefix))
    }

    fn prune_expired(&self) -> BoxFuture<'_, Result<u64, StorageError>> {
        Box::pin(self.prune())
    }
}

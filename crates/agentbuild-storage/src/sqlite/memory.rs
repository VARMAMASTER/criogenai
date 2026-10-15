use std::time::Duration;

use agentbuild_core::BoxFuture;
use serde_json::Value;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use super::{get, sql_error};
use crate::checkpoint::now_ms;
use crate::crypto::Sealer;
use crate::error::StorageError;
use crate::sql::{decode_embedding, decode_memory, encode_embedding, encode_memory};
use crate::store::{
    Embedding, MemoryItem, MemoryStore, Namespace, SearchQuery, check_embedding, check_prefix,
    check_put, expiry, is_live, query_terms, rank, rank_similar,
};

/// Long-term memory in a SQLite database (table `agentbuild_memory`).
/// Usually from [`SqliteCheckpointer::memory_store`](crate::SqliteCheckpointer::memory_store)
/// or [`Storage::connect`](crate::Storage::connect), sharing the database
/// (and encryption keys) with threads.
///
/// Keyword search narrows the rows in SQL (items containing any query word)
/// and ranks them as every backend does (see [`MemoryStore`]). With
/// encryption it reads and decrypts the live items of the prefix instead
/// (design 0008 §15.2).
#[derive(Clone)]
pub struct SqliteMemoryStore {
    pool: SqlitePool,
    sealer: Sealer,
}

impl std::fmt::Debug for SqliteMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMemoryStore")
            .field("encrypted", &self.sealer.is_on())
            .finish_non_exhaustive()
    }
}

/// The SQL condition for "namespace is `?1` or inside it" (`?2` is `?1`
/// plus `/`); the root matches everything.
const IN_PREFIX: &str = "(?1 = '' OR namespace = ?1 OR substr(namespace, 1, length(?2)) = ?2)";

const COLUMNS: &str =
    "namespace, key, value, search_text, created_at_ms, updated_at_ms, expires_at_ms, embedding";

/// `key IN (?n, ?n+1, …)` for `count` keys starting at parameter `first`.
fn key_in(first: usize, count: usize) -> String {
    let marks: Vec<String> = (first..first + count).map(|i| format!("?{i}")).collect();
    format!("key IN ({})", marks.join(", "))
}

impl SqliteMemoryStore {
    pub(crate) fn new(pool: SqlitePool, sealer: Sealer) -> Self {
        Self { pool, sealer }
    }

    /// Opens (or creates) the database at `url` (as
    /// [`SqliteCheckpointer::connect`](crate::SqliteCheckpointer::connect)).
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Ok(crate::SqliteCheckpointer::connect(url)
            .await?
            .memory_store())
    }

    async fn put_item(
        &self,
        namespace: &Namespace,
        key: &str,
        value: Value,
        ttl: Option<Duration>,
        embedding: Option<Embedding>,
    ) -> Result<(), StorageError> {
        if let Some(embedding) = &embedding {
            check_embedding(embedding)?;
        }
        let text = check_put(namespace, key, &value)?;
        let now = now_ms();
        let path = namespace.path();
        let (column, stored, search) = encode_memory(&self.sealer, &path, key, text, &value)?;
        let vector = embedding
            .map(|e| encode_embedding(&self.sealer, &path, &column, &e))
            .transpose()?;
        let expires = expiry(now, ttl).map(|at| at.min(i64::MAX as u64) as i64);
        if !self.sealer.is_on() {
            // Overwriting an expired item starts a new one (new created_at).
            sqlx::query(
                "INSERT INTO agentbuild_memory
                    (namespace, key, value, search_text, created_at_ms, updated_at_ms,
                     expires_at_ms, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)
                 ON CONFLICT (namespace, key) DO UPDATE SET
                    value = excluded.value,
                    search_text = excluded.search_text,
                    embedding = excluded.embedding,
                    created_at_ms = CASE
                        WHEN agentbuild_memory.expires_at_ms IS NOT NULL
                            AND agentbuild_memory.expires_at_ms <= excluded.updated_at_ms
                        THEN excluded.created_at_ms
                        ELSE agentbuild_memory.created_at_ms END,
                    updated_at_ms = excluded.updated_at_ms,
                    expires_at_ms = excluded.expires_at_ms",
            )
            .bind(path)
            .bind(column)
            .bind(stored)
            .bind(search)
            .bind(now as i64)
            .bind(expires)
            .bind(vector)
            .execute(&self.pool)
            .await
            .map_err(sql_error)?;
            return Ok(());
        }
        // Encrypted: the item may be stored under another key's tag (or
        // plain, from before encryption): replace every form of it, keeping
        // the first write's time while the item is live.
        let forms = self.sealer.memory_keys(&path, key);
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_error)?;
        let sql = format!(
            "SELECT created_at_ms, expires_at_ms FROM agentbuild_memory
                WHERE namespace = ?1 AND {}",
            key_in(2, forms.len())
        );
        let mut select = sqlx::query_as::<_, (i64, Option<i64>)>(&sql).bind(&path);
        for form in &forms {
            select = select.bind(form);
        }
        let found = select.fetch_all(&mut *tx).await.map_err(sql_error)?;
        let created = found
            .iter()
            .filter(|(_, expires)| is_live(expires.map(|at| at as u64), now))
            .map(|(created, _)| *created)
            .min()
            .unwrap_or(now as i64);
        let sql = format!(
            "DELETE FROM agentbuild_memory WHERE namespace = ?1 AND {}",
            key_in(2, forms.len())
        );
        let mut delete = sqlx::query(&sql).bind(&path);
        for form in &forms {
            delete = delete.bind(form);
        }
        delete.execute(&mut *tx).await.map_err(sql_error)?;
        sqlx::query(
            "INSERT INTO agentbuild_memory
                (namespace, key, value, search_text, created_at_ms, updated_at_ms, expires_at_ms,
                 embedding)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&path)
        .bind(column)
        .bind(stored)
        .bind(search)
        .bind(created)
        .bind(now as i64)
        .bind(expires)
        .bind(vector)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        tx.commit().await.map_err(sql_error)
    }

    async fn get_item(
        &self,
        namespace: &Namespace,
        key: &str,
    ) -> Result<Option<MemoryItem>, StorageError> {
        let path = namespace.path();
        let forms = self.sealer.memory_keys(&path, key);
        let sql = format!(
            "SELECT {COLUMNS} FROM agentbuild_memory
             WHERE namespace = ?1 AND (expires_at_ms IS NULL OR expires_at_ms > ?2) AND {}
             ORDER BY updated_at_ms DESC LIMIT 1",
            key_in(3, forms.len())
        );
        let mut select = sqlx::query(&sql).bind(&path).bind(now_ms() as i64);
        for form in &forms {
            select = select.bind(form);
        }
        let row = select.fetch_optional(&self.pool).await.map_err(sql_error)?;
        row.map(|row| self.read_item(&row).map(|(item, _)| item))
            .transpose()
    }

    async fn search_items(
        &self,
        prefix: &Namespace,
        query: &SearchQuery,
    ) -> Result<Vec<MemoryItem>, StorageError> {
        if let Some(vector) = &query.vector {
            return self.search_similar(prefix, query, vector).await;
        }
        let terms = query_terms(query);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM agentbuild_memory
             WHERE {IN_PREFIX} AND (expires_at_ms IS NULL OR expires_at_ms > ?3)"
        );
        // Encrypted rows have no searchable text: read them all and rank
        // them after decrypting.
        let narrow = !self.sealer.is_on();
        if terms.is_empty() && narrow {
            sql.push_str(" ORDER BY updated_at_ms DESC, namespace, key LIMIT ?4");
        } else if narrow {
            // Rows containing any query word (or a word starting with it);
            // all of them are ranked below.
            let any: Vec<String> = (0..terms.len())
                .map(|i| format!("instr(search_text, ?{}) > 0", i + 4))
                .collect();
            sql.push_str(&format!(" AND ({})", any.join(" OR ")));
        }
        let path = prefix.path();
        let mut select = sqlx::query(&sql)
            .bind(path.clone())
            .bind(format!("{path}/"))
            .bind(now_ms() as i64);
        if narrow && terms.is_empty() {
            select = select.bind(query.limit.min(i64::MAX as usize) as i64);
        } else if narrow {
            for term in &terms {
                select = select.bind(format!(" {term}"));
            }
        }
        let rows = select.fetch_all(&self.pool).await.map_err(sql_error)?;
        let candidates = rows
            .iter()
            .map(|row| self.read_item(row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rank(candidates, &terms, query.limit))
    }

    /// Semantic search: the live items of the prefix that have a vector,
    /// ranked in Rust (no vector index; design 0008 §16.2).
    async fn search_similar(
        &self,
        prefix: &Namespace,
        query: &SearchQuery,
        vector: &Embedding,
    ) -> Result<Vec<MemoryItem>, StorageError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM agentbuild_memory
             WHERE {IN_PREFIX} AND (expires_at_ms IS NULL OR expires_at_ms > ?3)
                AND embedding IS NOT NULL"
        );
        let path = prefix.path();
        let rows = sqlx::query(&sql)
            .bind(path.clone())
            .bind(format!("{path}/"))
            .bind(now_ms() as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(sql_error)?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(embedding) = self.read_embedding(row)? {
                candidates.push((self.read_item(row)?.0, embedding));
            }
        }
        Ok(rank_similar(
            candidates,
            vector,
            query.min_score,
            query.limit,
        ))
    }

    async fn delete_item(&self, namespace: &Namespace, key: &str) -> Result<bool, StorageError> {
        let path = namespace.path();
        let forms = self.sealer.memory_keys(&path, key);
        let sql = format!(
            "DELETE FROM agentbuild_memory WHERE namespace = ?1 AND {}
             RETURNING expires_at_ms IS NULL OR expires_at_ms > ?2",
            key_in(3, forms.len())
        );
        let mut delete = sqlx::query_scalar::<_, bool>(&sql)
            .bind(&path)
            .bind(now_ms() as i64);
        for form in &forms {
            delete = delete.bind(form);
        }
        let live = delete.fetch_all(&self.pool).await.map_err(sql_error)?;
        Ok(live.into_iter().any(|live| live))
    }

    async fn delete_prefix(&self, prefix: &Namespace) -> Result<u64, StorageError> {
        check_prefix(prefix)?;
        let path = prefix.path();
        let done = sqlx::query(&format!("DELETE FROM agentbuild_memory WHERE {IN_PREFIX}"))
            .bind(path.clone())
            .bind(format!("{path}/"))
            .execute(&self.pool)
            .await
            .map_err(sql_error)?;
        Ok(done.rows_affected())
    }

    async fn prune(&self) -> Result<u64, StorageError> {
        let done = sqlx::query(
            "DELETE FROM agentbuild_memory
             WHERE expires_at_ms IS NOT NULL AND expires_at_ms <= ?1",
        )
        .bind(now_ms() as i64)
        .execute(&self.pool)
        .await
        .map_err(sql_error)?;
        Ok(done.rows_affected())
    }

    /// Rewrites items not encrypted with the active key (`prefix` is
    /// `enc:v1:<active id>:`). A concurrent `put` of the same item wins.
    #[cfg(feature = "encryption")]
    pub(crate) async fn reencrypt(&self, prefix: &str) -> Result<u64, StorageError> {
        let mut done = 0;
        let mut after = (String::new(), String::new());
        loop {
            let sql = format!(
                "SELECT {COLUMNS} FROM agentbuild_memory
                    WHERE (namespace, key) > (?1, ?2)
                        AND (substr(value, 1, length(?3)) <> ?3
                            OR substr(embedding, 1, length(?3)) <> ?3)
                    ORDER BY namespace, key LIMIT ?4"
            );
            let rows = sqlx::query(&sql)
                .bind(&after.0)
                .bind(&after.1)
                .bind(prefix)
                .bind(super::BATCH as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(sql_error)?;
            let Some(last) = rows.last() else {
                return Ok(done);
            };
            after = (get(last, "namespace")?, get(last, "key")?);
            for row in &rows {
                let path: String = get(row, "namespace")?;
                let old_key: String = get(row, "key")?;
                let old_value: String = get(row, "value")?;
                let (item, _) = self.read_item(row)?;
                let embedding = self.read_embedding(row)?;
                let text = serde_json::to_string(&item.value).map_err(crate::sql::encode_error)?;
                let (column, stored, search) =
                    encode_memory(&self.sealer, &path, &item.key, text, &item.value)?;
                let vector = embedding
                    .map(|e| encode_embedding(&self.sealer, &path, &column, &e))
                    .transpose()?;
                let mut tx = self
                    .pool
                    .begin_with("BEGIN IMMEDIATE")
                    .await
                    .map_err(sql_error)?;
                let removed = sqlx::query(
                    "DELETE FROM agentbuild_memory WHERE namespace = ? AND key = ? AND value = ?",
                )
                .bind(&path)
                .bind(&old_key)
                .bind(&old_value)
                .execute(&mut *tx)
                .await
                .map_err(sql_error)?
                .rows_affected();
                if removed == 1 {
                    // A newer write under the new tag wins.
                    done += sqlx::query(
                        "INSERT INTO agentbuild_memory
                            (namespace, key, value, search_text, created_at_ms, updated_at_ms,
                             expires_at_ms, embedding)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                         ON CONFLICT (namespace, key) DO NOTHING",
                    )
                    .bind(&path)
                    .bind(column)
                    .bind(stored)
                    .bind(search)
                    .bind(item.created_at_ms as i64)
                    .bind(item.updated_at_ms as i64)
                    .bind(item.expires_at_ms.map(|at| at as i64))
                    .bind(vector)
                    .execute(&mut *tx)
                    .await
                    .map_err(sql_error)?
                    .rows_affected();
                }
                tx.commit().await.map_err(sql_error)?;
            }
        }
    }

    /// The row's vector, if it has one (decrypted if needed).
    fn read_embedding(&self, row: &SqliteRow) -> Result<Option<Embedding>, StorageError> {
        let Some(stored) = get::<Option<String>>(row, "embedding")? else {
            return Ok(None);
        };
        let path: String = get(row, "namespace")?;
        let column: String = get(row, "key")?;
        decode_embedding(&self.sealer, &path, &column, stored).map(Some)
    }

    /// An item and its search text (decrypted if needed).
    fn read_item(&self, row: &SqliteRow) -> Result<(MemoryItem, String), StorageError> {
        decode_memory(
            &self.sealer,
            &get::<String>(row, "namespace")?,
            get(row, "key")?,
            get(row, "value")?,
            get(row, "search_text")?,
            get(row, "created_at_ms")?,
            get(row, "updated_at_ms")?,
            get(row, "expires_at_ms")?,
        )
    }
}

impl MemoryStore for SqliteMemoryStore {
    fn put<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
        value: Value,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(self.put_item(namespace, key, value, ttl, None))
    }

    fn put_embedded<'a>(
        &'a self,
        namespace: &'a Namespace,
        key: &'a str,
        value: Value,
        ttl: Option<Duration>,
        embedding: Embedding,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(self.put_item(namespace, key, value, ttl, Some(embedding)))
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

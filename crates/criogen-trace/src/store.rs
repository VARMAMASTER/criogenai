//! Where spans are kept: SQLite, in a file or in memory (design 0023).

use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{Row, SqliteConnection};

use crate::otlp::{self, MAX_SPANS_PER_RUN, SpanRow};
use crate::{Prices, TraceError};

#[derive(Debug, Clone)]
pub(crate) struct Store {
    pool: SqlitePool,
    keep_runs: i64,
    keep_for: Duration,
}

fn db_error(e: sqlx::Error) -> TraceError {
    TraceError::Storage(e.to_string())
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS trace_spans (
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    start_ns INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (trace_id, span_id)
);
CREATE TABLE IF NOT EXISTS trace_runs (
    trace_id TEXT PRIMARY KEY,
    start_ns INTEGER NOT NULL,
    summary TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS trace_runs_start ON trace_runs (start_ns DESC);
";

impl Store {
    pub(crate) async fn open(
        path: Option<&Path>,
        keep_runs: usize,
        keep_for: Duration,
    ) -> Result<Self, TraceError> {
        let options = match path {
            Some(path) => {
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| TraceError::Storage(format!("{}: {e}", dir.display())))?;
                }
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
            }
            None => SqliteConnectOptions::from_str("sqlite::memory:").map_err(db_error)?,
        };
        // One connection: an in-memory database lives in its connection, and
        // the viewer's writes are small.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
            .map_err(db_error)?;
        sqlx::raw_sql(SCHEMA)
            .execute(&pool)
            .await
            .map_err(db_error)?;
        Ok(Self {
            pool,
            keep_runs: i64::try_from(keep_runs.max(1)).unwrap_or(i64::MAX),
            keep_for,
        })
    }

    /// Stores spans and updates the summaries of their runs.
    pub(crate) async fn insert(
        &self,
        rows: Vec<SpanRow>,
        prices: &Prices,
    ) -> Result<(), TraceError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let mut touched = BTreeSet::new();
        for row in rows {
            let count: i64 = sqlx::query("SELECT COUNT(*) FROM trace_spans WHERE trace_id = ?")
                .bind(&row.trace_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?
                .get(0);
            if count >= MAX_SPANS_PER_RUN as i64 {
                continue;
            }
            sqlx::query(
                "INSERT INTO trace_spans (trace_id, span_id, start_ns, body) VALUES (?, ?, ?, ?)
                 ON CONFLICT (trace_id, span_id) DO UPDATE SET start_ns = excluded.start_ns, body = excluded.body",
            )
            .bind(&row.trace_id)
            .bind(&row.span_id)
            .bind(row.start_ns)
            .bind(row.body.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            touched.insert(row.trace_id);
        }
        for trace_id in touched {
            let spans = spans_of(&mut tx, &trace_id).await?;
            let summary = otlp::summarize(&trace_id, &spans, prices);
            let start_ns = summary["start_ms"]
                .as_f64()
                .map_or(0, |ms| (ms * 1e6) as i64);
            sqlx::query(
                "INSERT INTO trace_runs (trace_id, start_ns, summary) VALUES (?, ?, ?)
                 ON CONFLICT (trace_id) DO UPDATE SET start_ns = excluded.start_ns, summary = excluded.summary",
            )
            .bind(&trace_id)
            .bind(start_ns)
            .bind(summary.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        self.prune(&mut tx).await?;
        tx.commit().await.map_err(db_error)
    }

    async fn prune(&self, tx: &mut SqliteConnection) -> Result<(), TraceError> {
        let oldest =
            now_ns().saturating_sub(i64::try_from(self.keep_for.as_nanos()).unwrap_or(i64::MAX));
        sqlx::query(
            "DELETE FROM trace_runs WHERE start_ns < ? OR trace_id NOT IN
             (SELECT trace_id FROM trace_runs ORDER BY start_ns DESC LIMIT ?)",
        )
        .bind(oldest)
        .bind(self.keep_runs)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query(
            "DELETE FROM trace_spans WHERE trace_id NOT IN (SELECT trace_id FROM trace_runs)",
        )
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    /// Run summaries, newest first, started before `before_ms` (if given),
    /// matching `q` in the name, models or ids (if given).
    pub(crate) async fn runs(
        &self,
        limit: i64,
        before_ms: Option<f64>,
        q: Option<&str>,
    ) -> Result<Vec<Value>, TraceError> {
        let before = before_ms.map_or(i64::MAX, |ms| (ms * 1e6) as i64);
        let like = q.map(|q| {
            let escaped = q
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        });
        let rows = sqlx::query(
            "SELECT summary FROM trace_runs WHERE start_ns < ? AND (? IS NULL OR summary LIKE ? ESCAPE '\\')
             ORDER BY start_ns DESC LIMIT ?",
        )
        .bind(before)
        .bind(like.as_deref())
        .bind(like.as_deref())
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(rows
            .iter()
            .filter_map(|r| serde_json::from_str(r.get::<&str, _>(0)).ok())
            .collect())
    }

    /// A run's summary and spans, or `None`.
    pub(crate) async fn run(
        &self,
        trace_id: &str,
    ) -> Result<Option<(Value, Vec<Value>)>, TraceError> {
        let mut conn = self.pool.acquire().await.map_err(db_error)?;
        let summary: Option<String> =
            sqlx::query("SELECT summary FROM trace_runs WHERE trace_id = ?")
                .bind(trace_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_error)?
                .map(|r| r.get(0));
        let Some(summary) = summary.and_then(|s| serde_json::from_str(&s).ok()) else {
            return Ok(None);
        };
        Ok(Some((summary, spans_of(&mut conn, trace_id).await?)))
    }

    pub(crate) async fn clear(&self) -> Result<(), TraceError> {
        sqlx::raw_sql("DELETE FROM trace_spans; DELETE FROM trace_runs;")
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    pub(crate) async fn close(&self) {
        self.pool.close().await;
    }
}

async fn spans_of(conn: &mut SqliteConnection, trace_id: &str) -> Result<Vec<Value>, TraceError> {
    let rows = sqlx::query("SELECT body FROM trace_spans WHERE trace_id = ? ORDER BY start_ns")
        .bind(trace_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(db_error)?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_str(r.get::<&str, _>(0)).ok())
        .collect())
}

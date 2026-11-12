use criogenai_core::BoxFuture;
use serde_json::{Map, Value};
use sqlx::postgres::{PgPool, PgRow};

use super::{get, sql_error};
use crate::audit::{
    AuditEvent, AuditLog, AuditQuery, AuditRecord, AuditVerification, ChainCheck, check_event,
};
use crate::checkpoint::now_ms;
use crate::error::StorageError;
use crate::sql::{encode_error, parse};

/// The audit trail in a Postgres database (table `criogen_audit`,
/// design 0010). From [`PostgresCheckpointer::audit_log`](crate::PostgresCheckpointer::audit_log);
/// appends hold a transaction advisory lock, so processes take turns and
/// the chain stays linear.
#[derive(Clone)]
pub struct PostgresAuditLog {
    pool: PgPool,
}

impl std::fmt::Debug for PostgresAuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresAuditLog").finish_non_exhaustive()
    }
}

const COLUMNS: &str =
    "seq, at_ms, action, actor, thread_id, run_id, subject, outcome, details, prev_hash, hash";

/// Records read per page by `verify`.
const PAGE: i64 = 1000;

impl PostgresAuditLog {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn add(&self, event: AuditEvent) -> Result<AuditRecord, StorageError> {
        check_event(&event)?;
        let mut tx = self.pool.begin().await.map_err(sql_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('criogen/audit', 0))")
            .execute(&mut *tx)
            .await
            .map_err(sql_error)?;
        let last: Option<(i64, String)> =
            sqlx::query_as("SELECT seq, hash FROM criogen_audit ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&mut *tx)
                .await
                .map_err(sql_error)?;
        let record = AuditRecord::chain(event, last.map(|(s, h)| (s as u64, h)), now_ms());
        let e = &record.event;
        sqlx::query(&format!(
            "INSERT INTO criogen_audit ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
        ))
        .bind(record.seq as i64)
        .bind(record.at_ms as i64)
        .bind(&e.action)
        .bind(&e.actor)
        .bind(&e.thread_id)
        .bind(&e.run_id)
        .bind(&e.subject)
        .bind(&e.outcome)
        .bind(serde_json::to_string(&e.details).map_err(encode_error)?)
        .bind(&record.prev_hash)
        .bind(&record.hash)
        .execute(&mut *tx)
        .await
        .map_err(sql_error)?;
        tx.commit().await.map_err(sql_error)?;
        Ok(record)
    }

    async fn find(&self, query: &AuditQuery) -> Result<Vec<AuditRecord>, StorageError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM criogen_audit
                WHERE ($1::TEXT IS NULL OR thread_id = $1) AND ($2::TEXT IS NULL OR actor = $2)
                    AND ($3::TEXT IS NULL OR action = $3) AND ($4::BIGINT IS NULL OR seq > $4)
                    AND ($5::BIGINT IS NULL OR at_ms >= $5) AND ($6::BIGINT IS NULL OR at_ms < $6)
                ORDER BY seq LIMIT $7"
        ))
        .bind(&query.thread_id)
        .bind(&query.actor)
        .bind(&query.action)
        .bind(query.after_seq.map(clamp))
        .bind(query.since_ms.map(clamp))
        .bind(query.until_ms.map(clamp))
        .bind(query.limit.min(i64::MAX as usize) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(sql_error)?;
        rows.iter().map(read_record).collect()
    }

    async fn check(&self) -> Result<AuditVerification, StorageError> {
        let mut check = ChainCheck::default();
        let mut after = 0i64;
        loop {
            let rows = sqlx::query(&format!(
                "SELECT {COLUMNS} FROM criogen_audit WHERE seq > $1 ORDER BY seq LIMIT $2"
            ))
            .bind(after)
            .bind(PAGE)
            .fetch_all(&self.pool)
            .await
            .map_err(sql_error)?;
            for row in &rows {
                let record = read_record(row)?;
                after = record.seq as i64;
                check.push(&record);
            }
            if (rows.len() as i64) < PAGE {
                return Ok(check.finish());
            }
        }
    }

    async fn last(&self) -> Result<Option<(u64, String)>, StorageError> {
        let last: Option<(i64, String)> =
            sqlx::query_as("SELECT seq, hash FROM criogen_audit ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(sql_error)?;
        Ok(last.map(|(s, h)| (s as u64, h)))
    }
}

fn clamp(n: u64) -> i64 {
    n.min(i64::MAX as u64) as i64
}

fn read_record(row: &PgRow) -> Result<AuditRecord, StorageError> {
    let details: Map<String, Value> = parse(&get::<String>(row, "details")?, "audit details")?;
    Ok(AuditRecord {
        seq: get::<i64>(row, "seq")? as u64,
        at_ms: get::<i64>(row, "at_ms")? as u64,
        event: AuditEvent {
            action: get(row, "action")?,
            actor: get(row, "actor")?,
            thread_id: get(row, "thread_id")?,
            run_id: get(row, "run_id")?,
            subject: get(row, "subject")?,
            outcome: get(row, "outcome")?,
            details,
        },
        prev_hash: get(row, "prev_hash")?,
        hash: get(row, "hash")?,
    })
}

impl AuditLog for PostgresAuditLog {
    fn append(&self, event: AuditEvent) -> BoxFuture<'_, Result<AuditRecord, StorageError>> {
        Box::pin(self.add(event))
    }

    fn query<'a>(
        &'a self,
        query: &'a AuditQuery,
    ) -> BoxFuture<'a, Result<Vec<AuditRecord>, StorageError>> {
        Box::pin(self.find(query))
    }

    fn verify(&self) -> BoxFuture<'_, Result<AuditVerification, StorageError>> {
        Box::pin(self.check())
    }

    fn head(&self) -> BoxFuture<'_, Result<Option<(u64, String)>, StorageError>> {
        Box::pin(self.last())
    }
}

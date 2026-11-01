//! The audit trail (design 0010): the contract every `AuditLog` keeps (on
//! the in-memory log, SQLite and, with a server, Postgres), tamper
//! detection through the database, appends from several processes, and
//! `Storage` recording its own actions, failing closed.

use std::sync::Arc;
use std::time::Duration;

use criogen_core::BoxFuture;
use criogen_storage::{
    AuditEvent, AuditLog, AuditQuery, AuditRecord, AuditVerification, GENESIS_HASH,
    InMemoryAuditLog, Namespace, Retention, Storage, StorageError, StorageErrorKind,
};
use serde_json::json;

/// The contract, on an empty log.
async fn contract(log: &dyn AuditLog) {
    assert_eq!(log.head().await.unwrap(), None);
    let empty = log.verify().await.unwrap();
    assert!(empty.ok && empty.records == 0);

    let first = log
        .append(
            AuditEvent::new("run.finished")
                .actor(Some("user:dr-rao"))
                .thread(Some("visit-1"))
                .run(Some("r1"))
                .subject("send")
                .outcome("completed")
                .detail("version", 2),
        )
        .await
        .unwrap();
    assert_eq!(first.seq, 1);
    assert_eq!(first.prev_hash, GENESIS_HASH);
    assert_eq!(first.hash, first.compute_hash());
    let mut last = first.clone();
    for i in 0..5 {
        last = log
            .append(
                AuditEvent::new("tool.executed")
                    .thread(Some(if i % 2 == 0 { "visit-1" } else { "visit-2" }))
                    .subject("lookup")
                    .outcome("success")
                    .detail("call_id", format!("call_{i}"))
                    .detail("duration_ms", i),
            )
            .await
            .unwrap();
    }
    assert_eq!(last.seq, 6);
    assert_eq!(log.head().await.unwrap(), Some((6, last.hash.clone())));

    // Read back exactly as written, in order.
    let all = log.query(&AuditQuery::new()).await.unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(all[0], first);
    assert_eq!(all[5], last);
    assert!(all.windows(2).all(|w| w[1].prev_hash == w[0].hash));
    assert_eq!(all[2].event.details["call_id"], json!("call_1"));

    // Filters and paging.
    let seqs = |records: Vec<AuditRecord>| records.iter().map(|r| r.seq).collect::<Vec<_>>();
    assert_eq!(
        seqs(
            log.query(&AuditQuery::new().thread("visit-1"))
                .await
                .unwrap()
        ),
        [1, 2, 4, 6]
    );
    assert_eq!(
        seqs(
            log.query(&AuditQuery::new().actor("user:dr-rao"))
                .await
                .unwrap()
        ),
        [1]
    );
    assert_eq!(
        seqs(
            log.query(
                &AuditQuery::new()
                    .action("tool.executed")
                    .after_seq(3)
                    .limit(2)
            )
            .await
            .unwrap()
        ),
        [4, 5]
    );
    let at = all[0].at_ms;
    assert_eq!(
        log.query(&AuditQuery::new().since_ms(at))
            .await
            .unwrap()
            .len(),
        6
    );
    assert!(
        log.query(&AuditQuery::new().until_ms(at))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        log.query(&AuditQuery::new().limit(0))
            .await
            .unwrap()
            .is_empty()
    );

    let report = log.verify().await.unwrap();
    assert!(report.ok);
    assert_eq!(report.records, 6);
    assert_eq!(report.first_broken, None);
    assert_eq!(report.head, Some((6, last.hash.clone())));
    // An event needs an action.
    assert_eq!(
        log.append(AuditEvent::new("")).await.unwrap_err().code(),
        "invalid_value"
    );
}

#[tokio::test]
async fn signed_anchors_catch_a_cut_tail_and_a_recomputed_chain() {
    let key = b"anchor-key-kept-outside-the-database";
    let event = |n: u32| AuditEvent::new("tool.executed").subject(format!("call-{n}"));
    let log = InMemoryAuditLog::new();
    for n in 1..=5 {
        log.append(event(n)).await.unwrap();
    }
    let anchor = log.anchor(key).await.unwrap().unwrap();
    assert_eq!(anchor.seq, 5);
    // More records after the anchor: still fine.
    log.append(event(6)).await.unwrap();
    assert!(log.verify_anchor(&anchor, key).await.unwrap().ok);

    // The tail cut off: a valid, shorter chain.
    let cut = InMemoryAuditLog::new();
    for n in 1..=3 {
        cut.append(event(n)).await.unwrap();
    }
    assert!(cut.verify().await.unwrap().ok);
    assert!(!cut.verify_anchor(&anchor, key).await.unwrap().ok);

    // Records rewritten and the whole chain recomputed, as anyone who can
    // write the table can: the chain is valid, the anchor's hash is not.
    let forged = InMemoryAuditLog::new();
    for n in 1..=6 {
        let rewritten = if n == 3 { event(99) } else { event(n) };
        forged.append(rewritten).await.unwrap();
    }
    assert!(forged.verify().await.unwrap().ok);
    let verdict = forged.verify_anchor(&anchor, key).await.unwrap();
    assert!(!verdict.ok);
    assert_eq!(verdict.first_broken, Some(5));

    // An anchor made up without the key, or checked with the wrong key.
    let made_up = criogen_storage::AuditAnchor::sign(5, anchor.hash.clone(), b"guess");
    assert_eq!(
        log.verify_anchor(&made_up, key).await.unwrap_err().code(),
        "invalid_value"
    );
    assert!(log.verify_anchor(&anchor, b"other-key").await.is_err());
}

#[tokio::test]
async fn the_in_memory_log_keeps_the_contract_and_detects_edits() {
    let log = InMemoryAuditLog::new();
    contract(&log).await;
    log.tamper(
        3,
        AuditEvent::new("tool.executed").subject("something-else"),
    );
    let report = log.verify().await.unwrap();
    assert!(!report.ok);
    assert_eq!(report.first_broken, Some(3));
}

/// A log whose appends fail, to check that actions fail closed.
struct Broken;

impl AuditLog for Broken {
    fn append(&self, _: AuditEvent) -> BoxFuture<'_, Result<AuditRecord, StorageError>> {
        Box::pin(async {
            Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "audit store down",
            ))
        })
    }

    fn query<'a>(
        &'a self,
        _: &'a AuditQuery,
    ) -> BoxFuture<'a, Result<Vec<AuditRecord>, StorageError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn verify(&self) -> BoxFuture<'_, Result<AuditVerification, StorageError>> {
        Box::pin(async { Err(StorageError::new(StorageErrorKind::Unavailable, "down")) })
    }

    fn head(&self) -> BoxFuture<'_, Result<Option<(u64, String)>, StorageError>> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn storage_records_its_own_actions_and_fails_closed() {
    // Off by default: nothing to record into.
    let storage = Storage::in_memory();
    assert!(storage.audit().is_none());
    storage.record(AuditEvent::new("x")).await.unwrap();

    let storage = Storage::in_memory()
        .with_audit()
        .retention(Retention::new().thread_ttl(Duration::ZERO));
    let ns = Namespace::parse("users/1").unwrap();
    storage
        .memory()
        .put(&ns, "old", json!(1), Some(Duration::ZERO))
        .await
        .unwrap();
    storage.prune().await.unwrap();
    // A pass that removed nothing records nothing.
    storage.prune().await.unwrap();
    let records = storage
        .audit()
        .unwrap()
        .query(&AuditQuery::new())
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event.action, "storage.pruned");
    assert_eq!(records[0].event.details["memory_items_deleted"], json!(1));

    let failing = Storage::in_memory().with_audit_log(Broken);
    let err = failing.record(AuditEvent::new("x")).await.unwrap_err();
    assert_eq!(err.code(), "audit_failed");
    assert!(
        err.message.contains("could not record `x`"),
        "{}",
        err.message
    );
    failing
        .memory()
        .put(&ns, "old", json!(1), Some(Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(failing.prune().await.unwrap_err().code(), "audit_failed");
    // The same log shared by two storages.
    let shared: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let a = Storage::in_memory().with_shared_audit_log(shared.clone());
    let b = Storage::in_memory().with_shared_audit_log(shared.clone());
    a.record(AuditEvent::new("a")).await.unwrap();
    b.record(AuditEvent::new("b")).await.unwrap();
    assert_eq!(shared.head().await.unwrap().unwrap().0, 2);
}

/// Appends from `processes` storages at once never fork the chain.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
async fn concurrent(storages: Vec<Storage>, each: usize) -> AuditVerification {
    let mut tasks = Vec::new();
    for (p, storage) in storages.iter().enumerate() {
        let storage = storage.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..each {
                storage
                    .record(AuditEvent::new("tool.executed").detail("who", format!("{p}:{i}")))
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    storages[0].audit().unwrap().verify().await.unwrap()
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;
    use criogen_storage::SqliteCheckpointer;

    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("criogen-audit-{}.db", uuid::Uuid::now_v7())))
        }

        fn url(&self) -> String {
            format!("sqlite://{}", self.0.display())
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut path = self.0.clone().into_os_string();
                path.push(suffix);
                let _ = std::fs::remove_file(path);
            }
        }
    }

    #[tokio::test]
    async fn the_sqlite_log_keeps_the_contract_and_detects_edits_and_deletions() {
        let db = TempDb::new();
        let checkpointer = SqliteCheckpointer::connect(&db.url()).await.unwrap();
        let log = checkpointer.audit_log();
        contract(&log).await;
        // Through `Storage::connect(...).with_audit()`: the same table.
        let storage = Storage::connect(&db.url()).await.unwrap().with_audit();
        assert_eq!(storage.audit().unwrap().head().await.unwrap().unwrap().0, 6);

        let raw = sqlx::SqlitePool::connect(&db.url()).await.unwrap();
        // The database refuses plain edits and deletions...
        for statement in [
            "UPDATE criogen_audit SET actor = 'someone-else' WHERE seq = 1",
            "DELETE FROM criogen_audit WHERE seq = 4",
        ] {
            let refused = sqlx::query(statement).execute(&raw).await.unwrap_err();
            assert!(refused.to_string().contains("append-only"), "{refused}");
        }
        assert!(log.verify().await.unwrap().ok);
        // ...so what follows needs schema rights: the triggers are dropped.
        let anchor = log.anchor(b"anchor-key").await.unwrap().unwrap();
        assert_eq!(anchor.seq, 6);
        for trigger in ["criogen_audit_no_update", "criogen_audit_no_delete"] {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(&raw)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE criogen_audit SET actor = 'someone-else' WHERE seq = 1")
            .execute(&raw)
            .await
            .unwrap();
        assert_eq!(log.verify().await.unwrap().first_broken, Some(1));
        sqlx::query("UPDATE criogen_audit SET actor = 'user:dr-rao' WHERE seq = 1")
            .execute(&raw)
            .await
            .unwrap();
        assert!(log.verify().await.unwrap().ok);
        sqlx::query("DELETE FROM criogen_audit WHERE seq = 4")
            .execute(&raw)
            .await
            .unwrap();
        assert_eq!(log.verify().await.unwrap().first_broken, Some(5));
        // A tail cut off keeps the chain valid, which only an anchor sees.
        sqlx::query("DELETE FROM criogen_audit WHERE seq >= 4")
            .execute(&raw)
            .await
            .unwrap();
        assert!(
            log.verify().await.unwrap().ok,
            "a cut tail is a valid chain"
        );
        let cut = log.verify_anchor(&anchor, b"anchor-key").await.unwrap();
        assert!(!cut.ok);
        assert_eq!(cut.first_broken, Some(6));
        raw.close().await;
        checkpointer.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn three_processes_append_to_one_chain() {
        let db = TempDb::new();
        let mut storages = Vec::new();
        for _ in 0..3 {
            storages.push(Storage::connect(&db.url()).await.unwrap().with_audit());
        }
        let report = concurrent(storages, 30).await;
        assert!(report.ok, "{report:?}");
        assert_eq!(report.records, 90);
    }
}

#[cfg(feature = "postgres")]
#[path = "support/postgres.rs"]
mod pg;

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use criogen_storage::PostgresCheckpointer;

    #[tokio::test]
    async fn the_postgres_log_keeps_the_contract_and_detects_edits() {
        let Some(db) = pg::TestDb::create().await else {
            return;
        };
        let checkpointer = PostgresCheckpointer::connect(&db.url).await.unwrap();
        let log = checkpointer.audit_log();
        contract(&log).await;
        let raw = db.raw().await;
        // The database refuses plain edits, deletions and truncation.
        for statement in [
            "UPDATE criogen_audit SET outcome = 'failed' WHERE seq = 2",
            "DELETE FROM criogen_audit WHERE seq = 2",
            "TRUNCATE criogen_audit",
        ] {
            let refused = sqlx::query(statement).execute(&raw).await.unwrap_err();
            assert!(refused.to_string().contains("append-only"), "{refused}");
        }
        assert!(log.verify().await.unwrap().ok);
        // ...so tampering needs ownership of the table.
        sqlx::query("ALTER TABLE criogen_audit DISABLE TRIGGER USER")
            .execute(&raw)
            .await
            .unwrap();
        sqlx::query("UPDATE criogen_audit SET outcome = 'failed' WHERE seq = 2")
            .execute(&raw)
            .await
            .unwrap();
        assert_eq!(log.verify().await.unwrap().first_broken, Some(2));
        raw.close().await;
        checkpointer.close().await;
        db.remove().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn three_processes_append_to_one_chain_on_postgres() {
        let Some(db) = pg::TestDb::create().await else {
            return;
        };
        let mut storages = Vec::new();
        for _ in 0..3 {
            storages.push(Storage::connect(&db.url).await.unwrap().with_audit());
        }
        let report = concurrent(storages.clone(), 30).await;
        assert!(report.ok, "{report:?}");
        assert_eq!(report.records, 90);
        drop(storages);
        db.remove().await;
    }
}

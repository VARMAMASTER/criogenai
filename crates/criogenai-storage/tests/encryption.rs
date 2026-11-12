//! Encryption at rest (design 0008 §15.2), on SQLite and (with a server)
//! Postgres: nothing readable in the content columns, keys checked at
//! connect, rotation, `reencrypt`, rows that cannot be moved.
#![cfg(all(feature = "encryption", any(feature = "sqlite", feature = "postgres")))]

#[path = "support/contract.rs"]
mod contract;

use criogenai_core::{PendingWrite, State};
use criogenai_storage::{
    Checkpoint, Embedding, EncryptionKey, Keyring, Namespace, SearchQuery, Source, Storage,
};
use serde_json::json;

/// A word that must never be stored readably.
const SECRET: &str = "penicillin";

fn key(id: &str, byte: u8) -> EncryptionKey {
    EncryptionKey::new(id, &[byte; 32]).unwrap()
}

fn ring(keys: &[(&str, u8)]) -> Keyring {
    let mut iter = keys.iter();
    let (id, byte) = iter.next().unwrap();
    iter.fold(Keyring::new(key(id, *byte)), |ring, (id, byte)| {
        ring.with_previous(key(id, *byte)).unwrap()
    })
}

/// A vector whose stored form would show its model name if readable.
fn vector() -> Embedding {
    Embedding::new("embedding-model-name", vec![0.25, -0.5, 1.0])
}

fn ns(path: &str) -> Namespace {
    Namespace::parse(path).unwrap()
}

/// Raw access to a test database.
enum Raw {
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::SqlitePool),
    #[cfg(feature = "postgres")]
    Postgres(sqlx::PgPool),
}

impl Raw {
    /// Every text that holds content or names it, one string per value.
    async fn texts(&self) -> Vec<String> {
        let queries = [
            "SELECT body FROM criogen_checkpoints",
            "SELECT snapshot FROM criogen_checkpoints WHERE snapshot IS NOT NULL",
            "SELECT write FROM criogen_pending_writes",
            "SELECT key FROM criogen_memory",
            "SELECT value FROM criogen_memory",
            "SELECT search_text FROM criogen_memory",
            "SELECT embedding FROM criogen_memory WHERE embedding IS NOT NULL",
        ];
        let mut all = Vec::new();
        for sql in queries {
            let rows: Vec<String> = match self {
                #[cfg(feature = "sqlite")]
                Raw::Sqlite(pool) => sqlx::query_scalar(sql).fetch_all(pool).await.unwrap(),
                #[cfg(feature = "postgres")]
                Raw::Postgres(pool) => sqlx::query_scalar(sql).fetch_all(pool).await.unwrap(),
            };
            all.extend(rows);
        }
        all
    }

    async fn execute(&self, sql: &str) {
        match self {
            #[cfg(feature = "sqlite")]
            Raw::Sqlite(pool) => {
                sqlx::query(sql).execute(pool).await.unwrap();
            }
            #[cfg(feature = "postgres")]
            Raw::Postgres(pool) => {
                sqlx::query(sql).execute(pool).await.unwrap();
            }
        }
    }

    async fn close(self) {
        match self {
            #[cfg(feature = "sqlite")]
            Raw::Sqlite(pool) => pool.close().await,
            #[cfg(feature = "postgres")]
            Raw::Postgres(pool) => pool.close().await,
        }
    }
}

/// Writes a thread with a snapshot and a task write, and memory items.
async fn write_secrets(storage: &Storage, thread: &str) {
    let store = storage.checkpointer();
    let first = Checkpoint::new(
        thread,
        1,
        Source::Input,
        State::new().with(
            "messages",
            json!([{"role": "user", "text": format!("allergic to {SECRET}")}]),
        ),
    )
    .run("r", 1)
    .snapshot(contract::snapshot(
        json!({"suspended": [{"node": "review", "payload": {"drug": SECRET}}]}),
    ));
    store.commit(first.clone(), 0).await.unwrap();
    let mut write = PendingWrite::default();
    write
        .memo
        .insert("call:1".into(), json!({"result": SECRET}));
    store
        .put_pending(thread, &first.id, "tools", write)
        .await
        .unwrap();
    let second = Checkpoint::new(
        thread,
        2,
        Source::Step,
        State::new().with(
            "messages",
            json!([{"role": "user", "text": format!("allergic to {SECRET}")}, {"role": "assistant", "text": "noted"}]),
        ),
    )
    .parent(&first.id);
    store.commit(second, 1).await.unwrap();
    let memory = storage.memory();
    let patient = ns(&format!("patients/{thread}"));
    memory
        .put_embedded(
            &patient,
            &format!("allergy_{SECRET}"),
            json!({"severity": "high", "drug": SECRET}),
            None,
            vector(),
        )
        .await
        .unwrap();
    memory
        .put(&patient, "dietary_preference", json!("vegetarian"), None)
        .await
        .unwrap();
}

/// Reads back what [`write_secrets`] wrote.
async fn read_secrets(storage: &Storage, thread: &str) {
    let head = storage
        .checkpointer()
        .load(thread, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.version, 2);
    assert!(
        head.state
            .get("messages")
            .unwrap()
            .to_string()
            .contains(SECRET)
    );
    let first = storage
        .checkpointer()
        .load(thread, head.parent_id.as_deref())
        .await
        .unwrap()
        .unwrap();
    let snapshot = first.snapshot.unwrap();
    assert_eq!(
        snapshot.pending_writes["tools"].memo["call:1"],
        json!({"result": SECRET})
    );
    assert_eq!(snapshot.suspended[0].payload, json!({"drug": SECRET}));
    let memory = storage.memory();
    let patient = ns(&format!("patients/{thread}"));
    let item = memory
        .get(&patient, &format!("allergy_{SECRET}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.key, format!("allergy_{SECRET}"));
    assert_eq!(item.value["drug"], json!(SECRET));
    let found = memory
        .search(
            &patient,
            &SearchQuery::new().text(format!("{SECRET} allergy")),
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, format!("allergy_{SECRET}"));
    assert_eq!(found[0].score, Some(1.0));
    // Its vector reads back too.
    let similar = memory
        .search(&patient, &SearchQuery::new().similar_to(vector()))
        .await
        .unwrap();
    assert_eq!(similar.len(), 1);
    assert_eq!(similar[0].key, format!("allergy_{SECRET}"));
    let newest = memory.search(&patient, &SearchQuery::new()).await.unwrap();
    assert_eq!(newest.len(), 2);
    assert!(newest[0].updated_at_ms >= newest[1].updated_at_ms);
}

async fn nothing_readable(raw: &Raw) {
    let texts = raw.texts().await;
    assert!(texts.len() >= 8, "{texts:?}");
    for text in &texts {
        assert!(!text.contains(SECRET), "readable: {text}");
        assert!(!text.contains("vegetarian"), "readable: {text}");
        assert!(!text.contains("dietary_preference"), "readable: {text}");
        assert!(!text.contains("embedding-model-name"), "readable: {text}");
    }
}

/// The whole story on one database: `url` is empty, `raw` looks at it.
async fn story(
    url: &str,
    raw: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Raw>>>,
) {
    // Encrypted from the start: every content column is unreadable, and
    // everything reads back.
    let storage = Storage::connect_encrypted(url, ring(&[("k1", 1)]))
        .await
        .unwrap();
    write_secrets(&storage, "t1").await;
    read_secrets(&storage, "t1").await;
    let db = raw().await;
    nothing_readable(&db).await;
    let texts = db.texts().await;
    assert!(
        texts
            .iter()
            .filter(|t| !t.is_empty())
            .all(|t| t.starts_with("enc:v1:k1:") || t.starts_with('~'))
    );
    db.close().await;
    // The checkpointer contract holds on encrypted storage.
    contract::all(storage.checkpointer().as_ref()).await;
    drop(storage);

    // Without keys, or with the wrong key: refused at connect.
    let err = Storage::connect(url).await.unwrap_err();
    assert_eq!(err.code(), "encryption_key_missing");
    let err = Storage::connect_encrypted(url, ring(&[("k1", 9)]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "encryption_key_invalid");

    // Rotation: k2 is active, k1 still reads.
    let rotated = Storage::connect_encrypted(url, ring(&[("k2", 2), ("k1", 1)]))
        .await
        .unwrap();
    read_secrets(&rotated, "t1").await;
    write_secrets(&rotated, "t2").await;
    // Replacing an item written under k1 leaves one row, under k2.
    rotated
        .memory()
        .put(
            &ns("patients/t1"),
            "dietary_preference",
            json!("vegan"),
            None,
        )
        .await
        .unwrap();
    let diet = rotated
        .memory()
        .get(&ns("patients/t1"), "dietary_preference")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(diet.value, json!("vegan"));
    drop(rotated);
    // k2 alone cannot read what k1 wrote…
    let only_k2 = Storage::connect_encrypted(url, ring(&[("k2", 2)]))
        .await
        .unwrap();
    let err = only_k2.checkpointer().load("t1", None).await.unwrap_err();
    assert_eq!(err.code(), "encryption_key_missing");
    assert!(err.message.contains("`k1`"), "{}", err.message);
    drop(only_k2);
    // …until everything is rewritten with k2.
    let rotated = Storage::connect_encrypted(url, ring(&[("k2", 2), ("k1", 1)]))
        .await
        .unwrap();
    let report = rotated.reencrypt().await.unwrap();
    assert!(report.checkpoints >= 3, "{report:?}");
    // t1's and the contract's.
    assert!(report.task_writes >= 1, "{report:?}");
    assert_eq!(report.memory_items, 1); // t1's allergy item; its diet was replaced
    assert_eq!(rotated.reencrypt().await.unwrap(), Default::default());
    drop(rotated);
    let only_k2 = Storage::connect_encrypted(url, ring(&[("k2", 2)]))
        .await
        .unwrap();
    read_secrets(&only_k2, "t1").await;
    read_secrets(&only_k2, "t2").await;
    let db = raw().await;
    nothing_readable(&db).await;
    assert!(
        db.texts()
            .await
            .iter()
            .all(|t| !t.starts_with("enc:v1:k1:"))
    );

    // A row moved into another row's place does not decrypt.
    let a = only_k2
        .checkpointer()
        .load("t1", None)
        .await
        .unwrap()
        .unwrap();
    let b = only_k2
        .checkpointer()
        .load("t2", None)
        .await
        .unwrap()
        .unwrap();
    db.execute(&format!(
        "UPDATE criogen_checkpoints SET body = (SELECT body FROM criogen_checkpoints \
         WHERE id = '{}') WHERE id = '{}'",
        b.id, a.id
    ))
    .await;
    db.close().await;
    let fresh = Storage::connect_encrypted(url, ring(&[("k2", 2)]))
        .await
        .unwrap();
    let err = fresh.checkpointer().load("t1", None).await.unwrap_err();
    assert_eq!(err.code(), "storage_corrupt");
    assert!(err.message.contains("moved"), "{}", err.message);
}

/// A database written without encryption, then opened with keys: old rows
/// read, `reencrypt` encrypts them.
async fn turning_it_on_later(
    url: &str,
    raw: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Raw>>>,
) {
    let plain = Storage::connect(url).await.unwrap();
    write_secrets(&plain, "old").await;
    drop(plain);
    let db = raw().await;
    assert!(db.texts().await.iter().any(|t| t.contains(SECRET)));
    db.close().await;

    let storage = Storage::connect_encrypted(url, ring(&[("k1", 1)]))
        .await
        .unwrap();
    // Until `reencrypt` seals them, plaintext rows are refused: in a
    // database that is encrypted, a plaintext row is a planted one.
    let refused = storage.checkpointer().load("old", None).await.unwrap_err();
    assert_eq!(refused.code(), "storage_corrupt");
    assert!(refused.message.contains("reencrypt"), "{}", refused.message);
    let refused = storage
        .memory()
        .search(&ns("patients/old"), &SearchQuery::new())
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "storage_corrupt");
    let report = storage.reencrypt().await.unwrap();
    assert_eq!(report.memory_items, 2);
    assert_eq!(report.task_writes, 1);
    read_secrets(&storage, "old").await;
    let db = raw().await;
    nothing_readable(&db).await;
    db.close().await;
    // From now on the database refuses to open without keys.
    assert_eq!(
        Storage::connect(url).await.unwrap_err().code(),
        "encryption_key_missing"
    );
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;

    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("criogenai-enc-{}.db", uuid::Uuid::now_v7())))
        }

        fn url(&self) -> String {
            format!("sqlite://{}", self.0.display())
        }

        fn raw(&self) -> impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Raw>>> {
            let url = self.url();
            move || {
                let url = url.clone();
                Box::pin(async move { Raw::Sqlite(sqlx::SqlitePool::connect(&url).await.unwrap()) })
            }
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
    async fn encryption_on_sqlite() {
        let db = TempDb::new();
        story(&db.url(), db.raw()).await;
    }

    #[tokio::test]
    async fn plaintext_planted_in_an_encrypted_database_is_refused() {
        let db = TempDb::new();
        let storage = Storage::connect_encrypted(&db.url(), ring(&[("k1", 1)]))
            .await
            .unwrap();
        write_secrets(&storage, "t").await;
        read_secrets(&storage, "t").await;
        // Someone with write access to the file replaces content with their
        // own: no sealed row, no authentication to fail, unless it is refused.
        let pool = sqlx::SqlitePool::connect(&db.url()).await.unwrap();
        for sql in [
            "UPDATE criogen_checkpoints SET body = '{\"messages\": [\"forged\"]}'",
            "UPDATE criogen_memory SET value = '\"a forged fact\"'",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        pool.close().await;
        let refused = storage.checkpointer().load("t", None).await.unwrap_err();
        assert_eq!(refused.code(), "storage_corrupt");
        let refused = storage
            .memory()
            .search(&ns("patients/t"), &SearchQuery::new())
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "storage_corrupt");
    }

    #[tokio::test]
    async fn encryption_turned_on_later_on_sqlite() {
        let db = TempDb::new();
        turning_it_on_later(&db.url(), db.raw()).await;
    }

    #[tokio::test]
    async fn deleted_rows_are_overwritten_in_the_file() {
        let db = TempDb::new();
        let storage = Storage::connect(&db.url()).await.unwrap();
        write_secrets(&storage, "gone").await;
        assert!(storage.checkpointer().delete_thread("gone").await.unwrap());
        assert_eq!(
            storage
                .memory()
                .delete_namespace(&ns("patients"))
                .await
                .unwrap(),
            2
        );
        drop(storage);
        // Checkpoint the WAL into the file, then look at the bytes.
        let pool = sqlx::SqlitePool::connect(&db.url()).await.unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let bytes = std::fs::read(&db.0).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(SECRET), "deleted content left in the file");
    }

    #[tokio::test]
    async fn in_memory_storage_accepts_keys_and_reencrypt_needs_a_database() {
        let storage = Storage::connect_encrypted("memory:", ring(&[("k1", 1)]))
            .await
            .unwrap();
        write_secrets(&storage, "t").await;
        read_secrets(&storage, "t").await;
        assert_eq!(
            storage.reencrypt().await.unwrap_err().code(),
            "encryption_key_missing"
        );
        let db = TempDb::new();
        let plain = Storage::connect(&db.url()).await.unwrap();
        assert_eq!(
            plain.reencrypt().await.unwrap_err().code(),
            "encryption_key_missing"
        );
    }
}

#[cfg(feature = "postgres")]
#[path = "support/postgres.rs"]
mod pg;

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;

    fn raw(url: &str) -> impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Raw>>> {
        let url = url.to_string();
        move || {
            let url = url.clone();
            Box::pin(async move { Raw::Postgres(sqlx::PgPool::connect(&url).await.unwrap()) })
        }
    }

    #[tokio::test]
    async fn encryption_on_postgres() {
        let Some(db) = pg::TestDb::create().await else {
            return;
        };
        story(&db.url, raw(&db.url)).await;
        db.remove().await;
    }

    #[tokio::test]
    async fn encryption_turned_on_later_on_postgres() {
        let Some(db) = pg::TestDb::create().await else {
            return;
        };
        turning_it_on_later(&db.url, raw(&db.url)).await;
        db.remove().await;
    }
}

//! Long-term memory (design 0008 §13): the contract every `MemoryStore`
//! keeps, run on the in-memory store and on SQLite.

use std::time::Duration;

use agentbuild_storage::{
    Embedding, InMemoryStore, MAX_DIMENSIONS, MemoryStore, Namespace, ScopedMemory, SearchQuery,
};
use serde_json::json;

fn ns(path: &str) -> Namespace {
    Namespace::parse(path).unwrap()
}

/// Waits until the clock moved, so the next write has a later timestamp.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(3)).await;
}

async fn keys(store: &dyn MemoryStore, prefix: &str, query: SearchQuery) -> Vec<String> {
    store
        .search(&ns(prefix), &query)
        .await
        .unwrap()
        .into_iter()
        .map(|item| format!("{}:{}", item.namespace, item.key))
        .collect()
}

/// The contract. Uses the real clock (SQLite needs it, and timestamps
/// order results).
async fn contract(store: &dyn MemoryStore) {
    let ada = ns("users/ada");
    let bob = ns("users/bob");

    // put / get, replace keeps created_at.
    store
        .put(&ada, "diet", json!("vegetarian, allergic to peanuts"), None)
        .await
        .unwrap();
    let first = store.get(&ada, "diet").await.unwrap().unwrap();
    assert_eq!(first.namespace, ada);
    assert_eq!(first.expires_at_ms, None);
    assert_eq!(first.score, None);
    tick().await;
    store
        .put(&ada, "diet", json!({"text": "vegan", "since": 2025}), None)
        .await
        .unwrap();
    let second = store.get(&ada, "diet").await.unwrap().unwrap();
    assert_eq!(second.value, json!({"text": "vegan", "since": 2025}));
    assert_eq!(second.created_at_ms, first.created_at_ms);
    assert!(second.updated_at_ms > first.updated_at_ms);
    assert!(store.get(&bob, "diet").await.unwrap().is_none());

    tick().await;
    store
        .put(&ada, "city", json!("Lives in Hyderabad"), None)
        .await
        .unwrap();
    tick().await;
    store
        .put(
            &bob,
            "diet",
            json!("eats everything, loves peanut butter"),
            None,
        )
        .await
        .unwrap();
    tick().await;
    store
        .put(
            &ns("users/ada/work"),
            "employer",
            json!("Works at a hospital"),
            None,
        )
        .await
        .unwrap();
    // A namespace that only shares a string prefix with users/ada.
    store
        .put(&ns("users/adam"), "diet", json!("vegan"), None)
        .await
        .unwrap();

    // Without text: newest first, in the prefix and the namespaces inside
    // it, but not `users/adam`.
    assert_eq!(
        keys(store, "users/ada", SearchQuery::new()).await,
        [
            "users/ada/work:employer",
            "users/ada:city",
            "users/ada:diet"
        ]
    );
    assert_eq!(
        keys(store, "users/ada", SearchQuery::new().limit(1)).await,
        ["users/ada/work:employer"]
    );
    assert_eq!(keys(store, "", SearchQuery::new()).await.len(), 5);

    // Keyword search: matching items only, best first.
    let found = store
        .search(
            &Namespace::root(),
            &SearchQuery::new().text("What is my diet? Vegan?"),
        )
        .await
        .unwrap();
    let ranked: Vec<(String, f64)> = found
        .iter()
        .map(|i| (format!("{}:{}", i.namespace, i.key), i.score.unwrap()))
        .collect();
    // Both words: ada's and adam's (adam newer); one word: bob.
    assert_eq!(
        ranked,
        [
            ("users/adam:diet".to_string(), 1.0),
            ("users/ada:diet".to_string(), 1.0),
            ("users/bob:diet".to_string(), 0.5),
        ]
    );
    // Word starts (3+ letters) count half; case does not matter.
    let found = store
        .search(&ns("users"), &SearchQuery::new().text("PEANUT hosp"))
        .await
        .unwrap();
    let ranked: Vec<(String, f64)> = found
        .iter()
        .map(|i| (i.key.clone(), i.score.unwrap()))
        .collect();
    assert_eq!(
        ranked,
        [
            ("diet".to_string(), 0.5),      // bob: "peanut" (whole word)
            ("employer".to_string(), 0.25)  // "hospital" starts with "hosp"
        ]
    );
    assert!(
        keys(store, "users", SearchQuery::new().text("pizza"))
            .await
            .is_empty()
    );
    // Only stop words: like no text.
    assert_eq!(
        keys(store, "users/bob", SearchQuery::new().text("what is my")).await,
        ["users/bob:diet"]
    );

    // Time to live.
    store
        .put(&bob, "mood", json!("tired"), Some(Duration::ZERO))
        .await
        .unwrap();
    store
        .put(&bob, "plan", json!("gym"), Some(Duration::from_secs(3600)))
        .await
        .unwrap();
    assert!(store.get(&bob, "mood").await.unwrap().is_none());
    let plan = store.get(&bob, "plan").await.unwrap().unwrap();
    assert!(plan.expires_at_ms.unwrap() > plan.updated_at_ms);
    assert!(
        !keys(store, "users/bob", SearchQuery::new())
            .await
            .contains(&"users/bob:mood".to_string())
    );
    assert_eq!(store.prune_expired().await.unwrap(), 1);
    assert_eq!(store.prune_expired().await.unwrap(), 0);
    // Writing again without a TTL keeps it for good.
    store.put(&bob, "plan", json!("gym"), None).await.unwrap();
    assert_eq!(
        store
            .get(&bob, "plan")
            .await
            .unwrap()
            .unwrap()
            .expires_at_ms,
        None
    );

    // delete, delete_namespace.
    assert!(store.delete(&bob, "plan").await.unwrap());
    assert!(!store.delete(&bob, "plan").await.unwrap());
    assert_eq!(store.delete_namespace(&ns("users/ada")).await.unwrap(), 3);
    assert_eq!(
        keys(store, "users", SearchQuery::new()).await,
        ["users/adam:diet", "users/bob:diet"]
    );
    assert_eq!(
        store
            .delete_namespace(&Namespace::root())
            .await
            .unwrap_err()
            .code(),
        "invalid_value"
    );

    // Values of every JSON type round-trip.
    for (key, value) in [
        ("n", json!(1.5)),
        ("b", json!(false)),
        ("z", json!(null)),
        ("l", json!([1, "two", {"three": 3}])),
        ("u", json!("ünïcødé ☕ 中文")),
    ] {
        store.put(&bob, key, value.clone(), None).await.unwrap();
        assert_eq!(store.get(&bob, key).await.unwrap().unwrap().value, value);
    }
    let found = store
        .search(&bob, &SearchQuery::new().text("中文"))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, "u");

    similar(store).await;
}

fn e(model: &str, vector: &[f32]) -> Embedding {
    Embedding::new(model, vector.to_vec())
}

/// Semantic search (design 0008 §16.2): vectors of one model, ranked by
/// cosine similarity, in the prefix, live only.
async fn similar(store: &dyn MemoryStore) {
    let ada = ns("vectors/ada");
    let put = |key: &'static str, embedding: Option<Embedding>| {
        let ada = ada.clone();
        async move {
            let value = json!(format!("fact {key}"));
            match embedding {
                Some(embedding) => store.put_embedded(&ada, key, value, None, embedding).await,
                None => store.put(&ada, key, value, None).await,
            }
            .unwrap()
        }
    };
    put("a", Some(e("m", &[1.0, 0.0, 0.0]))).await;
    tick().await;
    put("b", Some(e("m", &[0.8, 0.6, 0.0]))).await;
    tick().await;
    put("c", Some(e("m", &[0.0, 1.0, 0.0]))).await;
    // Another model, another size, no vector: never matched.
    put("d", Some(e("other", &[1.0, 0.0, 0.0]))).await;
    put("f", Some(e("m", &[1.0, 0.0]))).await;
    put("n", None).await;
    store
        .put_embedded(
            &ns("vectors/bob"),
            "x",
            json!("bob's"),
            None,
            e("m", &[1.0, 0.0, 0.0]),
        )
        .await
        .unwrap();
    store
        .put_embedded(
            &ada,
            "gone",
            json!("expired"),
            Some(Duration::ZERO),
            e("m", &[1.0, 0.0, 0.0]),
        )
        .await
        .unwrap();

    let query = || SearchQuery::new().similar_to(e("m", &[1.0, 0.0, 0.0]));
    let found = store.search(&ada, &query()).await.unwrap();
    let ranked: Vec<(&str, f64)> = found
        .iter()
        .map(|i| (i.key.as_str(), i.score.unwrap()))
        .collect();
    assert_eq!(ranked.len(), 3, "{ranked:?}");
    for ((key, score), (want_key, want_score)) in
        ranked.iter().zip([("a", 1.0), ("b", 0.8), ("c", 0.0)])
    {
        assert_eq!(*key, want_key);
        assert!((score - want_score).abs() < 1e-6, "{key}: {score}");
    }
    assert_eq!(found[0].value, json!("fact a"));
    assert_eq!(
        keys(store, "vectors/ada", query().limit(2)).await,
        ["vectors/ada:a", "vectors/ada:b"]
    );
    assert_eq!(
        keys(store, "vectors/ada", query().min_score(0.5)).await,
        ["vectors/ada:a", "vectors/ada:b"]
    );
    // The whole store: bob's item ties with a (same vector); newer first.
    assert_eq!(
        keys(store, "vectors", query().limit(2)).await,
        ["vectors/bob:x", "vectors/ada:a"]
    );
    // Text in the same query is ignored.
    assert_eq!(
        keys(
            store,
            "vectors/ada",
            query().text("nothing matches this").limit(1)
        )
        .await,
        ["vectors/ada:a"]
    );
    // A plain put drops the vector.
    put("a", None).await;
    assert_eq!(
        keys(store, "vectors/ada", query()).await,
        ["vectors/ada:b", "vectors/ada:c"]
    );
    assert!(store.get(&ada, "a").await.unwrap().is_some());

    // Vectors the store refuses.
    for bad in [
        e("m", &[]),
        e("m", &[f32::NAN]),
        e("m", &[f32::INFINITY, 1.0]),
        e("", &[1.0]),
        Embedding::new("m", vec![0.5; MAX_DIMENSIONS + 1]),
    ] {
        let err = store
            .put_embedded(&ada, "bad", json!(1), None, bad)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_value");
    }
    // The largest allowed vector round-trips.
    let big: Vec<f32> = (0..MAX_DIMENSIONS).map(|i| (i % 7) as f32 - 3.0).collect();
    store
        .put_embedded(
            &ada,
            "big",
            json!("big"),
            None,
            Embedding::new("big", big.clone()),
        )
        .await
        .unwrap();
    let found = store
        .search(
            &ada,
            &SearchQuery::new().similar_to(Embedding::new("big", big)),
        )
        .await
        .unwrap();
    assert_eq!(found[0].key, "big");
    assert!((found[0].score.unwrap() - 1.0).abs() < 1e-9);
    store.delete_namespace(&ns("vectors")).await.unwrap();
}

#[tokio::test]
async fn the_in_memory_store_keeps_the_contract() {
    contract(&InMemoryStore::new()).await;
}

#[tokio::test]
async fn scoped_memory_stays_in_its_namespace() {
    let store: std::sync::Arc<dyn MemoryStore> = std::sync::Arc::new(InMemoryStore::new());
    let ada = ScopedMemory::new(store.clone(), ns("users/ada"));
    let bob = ScopedMemory::new(store.clone(), ns("users/bob"));
    ada.put("diet", json!("vegan"), None).await.unwrap();
    bob.put("diet", json!("anything"), None).await.unwrap();
    assert_eq!(
        ada.get("diet").await.unwrap().unwrap().value,
        json!("vegan")
    );
    let found = bob.search(&SearchQuery::new().text("vegan")).await.unwrap();
    assert!(found.is_empty());
    assert_eq!(ada.clear().await.unwrap(), 1);
    assert!(ada.get("diet").await.unwrap().is_none());
    assert!(bob.get("diet").await.unwrap().is_some());
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;
    use agentbuild_storage::{SqliteCheckpointer, SqliteMemoryStore, Storage};

    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("agentbuild-memory-{}.db", uuid::Uuid::now_v7()))
    }

    fn remove(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::remove_file(file);
        }
    }

    #[tokio::test]
    async fn the_sqlite_store_keeps_the_contract() {
        contract(&SqliteMemoryStore::connect("sqlite::memory:").await.unwrap()).await;
        let path = temp_path();
        let url = format!("sqlite://{}", path.display());
        contract(&SqliteMemoryStore::connect(&url).await.unwrap()).await;
        remove(&path);
    }

    #[tokio::test]
    async fn memory_survives_reopening_and_shares_the_database_with_threads() {
        let path = temp_path();
        let url = format!("sqlite://{}", path.display());
        {
            let storage = Storage::connect(&url).await.unwrap();
            storage
                .memory()
                .put(&ns("users/ada"), "diet", json!("vegan"), None)
                .await
                .unwrap();
        }
        let checkpointer = SqliteCheckpointer::connect(&url).await.unwrap();
        let item = checkpointer
            .memory_store()
            .get(&ns("users/ada"), "diet")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.value, json!("vegan"));
        checkpointer.close().await;
        remove(&path);
    }

    #[tokio::test]
    async fn a_version_1_database_gets_the_memory_table() {
        let path = temp_path();
        let url = format!("sqlite://{}", path.display());
        let checkpointer = SqliteCheckpointer::connect(&url).await.unwrap();
        checkpointer.close().await;
        // Back to how version 1 left the file.
        let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
        for statement in [
            "DROP TABLE agentbuild_memory",
            "DROP TABLE agentbuild_settings",
            "DROP INDEX agentbuild_threads_updated",
            "DROP TABLE agentbuild_audit",
            "DELETE FROM agentbuild_schema_migrations WHERE version >= 2",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        pool.close().await;

        let checkpointer = SqliteCheckpointer::connect(&url).await.unwrap();
        assert_eq!(checkpointer.schema_version().await.unwrap(), 6);
        let store = checkpointer.memory_store();
        store
            .put(&ns("users/ada"), "diet", json!("vegan"), None)
            .await
            .unwrap();
        assert!(store.get(&ns("users/ada"), "diet").await.unwrap().is_some());
        checkpointer.close().await;
        remove(&path);
    }
}

#[cfg(feature = "postgres")]
#[path = "support/postgres.rs"]
mod pg;

#[cfg(feature = "postgres")]
#[tokio::test]
async fn the_postgres_store_keeps_the_contract() {
    use agentbuild_storage::{PostgresCheckpointer, Storage};

    let Some(db) = pg::TestDb::create().await else {
        return;
    };
    let checkpointer = PostgresCheckpointer::connect(&db.url).await.unwrap();
    let store = checkpointer.memory_store();
    contract(&store).await;

    // A prefix is a byte range [`p/`, `p0`): its edges stay outside.
    for path in ["users/ada0", "users/ada.", "users/adaé", "users/ada/x"] {
        store
            .put(&ns(path), "k", json!("edge"), None)
            .await
            .unwrap();
    }
    assert_eq!(
        keys(&store, "users/ada", SearchQuery::new()).await,
        ["users/ada/x:k"]
    );
    assert_eq!(store.delete_namespace(&ns("users/ada")).await.unwrap(), 1);
    checkpointer.close().await;

    // Through `Storage::connect`: the same database as threads.
    let storage = Storage::connect(&db.url).await.unwrap();
    assert!(
        storage
            .memory()
            .get(&ns("users/ada0"), "k")
            .await
            .unwrap()
            .is_some()
    );
    db.remove().await;
}

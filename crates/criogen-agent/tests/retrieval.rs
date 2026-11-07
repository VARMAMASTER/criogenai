//! Collections and retrievers (design 0025): add, search, filters, remove,
//! reopen, models, failures, encryption, and the tool in an agent. Simulated
//! embeddings and models only.

use std::sync::Arc;
use std::time::Duration;

use criogen_agent::Agent;
use criogen_agent::retrieval::{Collection, Document, IndexKind, RetrievalError};
use criogen_providers::{ContentBlock, FakeEmbedder, FakeModel, FakeReply, Role};
use criogen_storage::Storage;
use criogen_storage::vector::HnswConfig;
use serde_json::{Map, Value, json};

/// The fake embedder under another model name.
struct Renamed(FakeEmbedder);

impl criogen_providers::Embedder for Renamed {
    fn provider(&self) -> &str {
        "other"
    }
    fn model(&self) -> &str {
        "other-model"
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> criogen_providers::BoxFuture<'a, Result<Vec<Vec<f32>>, criogen_providers::ModelError>>
    {
        self.0.embed(texts)
    }
}

fn embedder() -> Arc<FakeEmbedder> {
    Arc::new(FakeEmbedder::new().synonym("vacation", "leave"))
}

fn handbook() -> Vec<Document> {
    vec![
        Document::new("leave", "Annual leave is 25 days a year.").meta("section", "hr"),
        Document::new("expenses", "Expenses are paid within 30 days of the claim.")
            .meta("section", "finance"),
        Document::new("parking", "Parking permits are issued by the front desk.")
            .meta("section", "facilities"),
        Document::new("sick", "Sick leave needs a note after three days.").meta("section", "hr"),
    ]
}

#[tokio::test]
async fn documents_are_found_by_meaning_and_filtered() {
    let storage = Storage::in_memory();
    let docs = Collection::open(storage.memory().clone(), "handbook", embedder())
        .await
        .unwrap();
    docs.add(handbook()).await.unwrap();
    assert_eq!(docs.len().await, 4);

    let hits = docs.search("how many vacation days", 2).await.unwrap();
    assert_eq!(hits[0].id, "leave", "{hits:?}");
    assert_eq!(hits[0].metadata["section"], "hr");
    assert!(hits[0].score > hits[1].score);

    let mut filter = Map::new();
    filter.insert("section".into(), json!("finance"));
    let hits = docs.search_where("days", 3, &filter).await.unwrap();
    assert!(hits.iter().all(|h| h.id == "expenses"), "{hits:?}");

    // Replace and remove.
    docs.add(vec![Document::new(
        "leave",
        "Annual leave is 30 days a year.",
    )])
    .await
    .unwrap();
    assert_eq!(docs.len().await, 4);
    assert!(
        docs.search("vacation days", 1).await.unwrap()[0]
            .text
            .contains("30 days")
    );
    assert!(docs.remove("leave").await.unwrap());
    assert!(!docs.remove("leave").await.unwrap());
    assert!(
        docs.search("vacation", 4)
            .await
            .unwrap()
            .iter()
            .all(|h| h.id != "leave")
    );
    assert!(docs.search("x", 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_hnsw_collection_finds_the_same_documents() {
    let storage = Storage::in_memory();
    let docs = Collection::open_with(
        storage.memory().clone(),
        "hnsw",
        embedder(),
        IndexKind::Hnsw(HnswConfig::default()),
    )
    .await
    .unwrap();
    docs.add(handbook()).await.unwrap();
    assert_eq!(
        docs.search("vacation days", 1).await.unwrap()[0].id,
        "leave"
    );
}

#[tokio::test]
async fn reopening_rebuilds_the_index_and_skips_other_models() {
    let storage = Storage::in_memory();
    let store = storage.memory().clone();
    let docs = Collection::open(store.clone(), "handbook", embedder())
        .await
        .unwrap();
    docs.add(handbook()).await.unwrap();

    // Another process, the same store: reopened, the documents are there.
    let again = Collection::open(store.clone(), "handbook", embedder())
        .await
        .unwrap();
    assert_eq!(again.len().await, 4);
    assert_eq!(again.search("vacation", 1).await.unwrap()[0].id, "leave");

    // A collection opened with another embedding model ignores these vectors.
    let other = Arc::new(Renamed(FakeEmbedder::new()));
    let different = Collection::open(store.clone(), "handbook", other)
        .await
        .unwrap();
    assert_eq!(different.len().await, 0);

    // Documents added elsewhere appear after reload.
    again
        .add(vec![Document::new("new", "Lunch is served at noon.")])
        .await
        .unwrap();
    assert_eq!(docs.len().await, 4);
    docs.reload().await.unwrap();
    assert_eq!(docs.len().await, 5);
}

#[tokio::test]
async fn bad_documents_and_embedder_failures_are_errors() {
    let storage = Storage::in_memory();
    let docs = Collection::open(storage.memory().clone(), "c", embedder())
        .await
        .unwrap();
    let err = docs.add(vec![Document::new("", "x")]).await.unwrap_err();
    assert_eq!(err.code(), "invalid_value");
    let err = docs
        .add(vec![Document::new("big", "x".repeat(100_001))])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("split it"));
    assert!(
        Collection::open(storage.memory().clone(), "", embedder())
            .await
            .is_err()
    );

    let failing = Collection::open(
        storage.memory().clone(),
        "f",
        Arc::new(FakeEmbedder::failing()),
    )
    .await
    .unwrap();
    let err = failing
        .add(vec![Document::new("a", "x")])
        .await
        .unwrap_err();
    assert!(matches!(err, RetrievalError::Embedding(_)));
    assert_eq!(err.code(), "model_failed");
    assert_eq!(failing.len().await, 0);
}

#[tokio::test]
async fn an_agent_searches_a_collection_through_its_tool() {
    let storage = Storage::in_memory();
    let docs = Collection::open(storage.memory().clone(), "handbook", embedder())
        .await
        .unwrap();
    docs.add(handbook()).await.unwrap();
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call(
            "search_handbook",
            json!({"query": "vacation days", "k": 2}),
        ))
        .then(FakeReply::text("You get 25 days."));
    let agent = Agent::builder(model)
        .tool(docs.as_tool("search_handbook", "Search the staff handbook."))
        .build()
        .unwrap();
    let result = agent.run("How much leave do I get?").await.unwrap();
    let message = result
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    let hits = message
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Json { value, .. } => Some(value.clone()),
            _ => None,
        })
        .expect("hits as JSON");
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["id"], "leave");
    assert!(hits[0]["text"].as_str().unwrap().contains("25 days"));
}

#[cfg(all(feature = "sqlite", feature = "encryption"))]
#[tokio::test]
async fn an_encrypted_database_keeps_vectors_encrypted_and_still_searches() {
    use criogen_storage::{EncryptionKey, Keyring};

    let dir = std::env::temp_dir().join(format!("criogen-retrieval-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("docs.db");
    let url = format!("sqlite://{}", path.display()).replace('\\', "/");
    let keys = || Keyring::new(EncryptionKey::new("k1", &[7u8; 32]).unwrap());
    {
        let storage = Storage::connect_encrypted(&url, keys()).await.unwrap();
        let docs = Collection::open(storage.memory().clone(), "handbook", embedder())
            .await
            .unwrap();
        docs.add(handbook()).await.unwrap();
        storage.close().await;
    }
    // On disk: neither the text nor the model name is readable.
    let raw = std::fs::read(&path).unwrap();
    let seen = String::from_utf8_lossy(&raw);
    assert!(
        !seen.contains("Annual leave"),
        "the text is stored in the clear"
    );
    assert!(
        !seen.contains("fake/"),
        "the vector's model is stored in the clear"
    );

    let storage = Storage::connect_encrypted(&url, keys()).await.unwrap();
    let docs = Collection::open(storage.memory().clone(), "handbook", embedder())
        .await
        .unwrap();
    assert_eq!(docs.search("vacation", 1).await.unwrap()[0].id, "leave");
    storage.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn any_retriever_can_be_a_tool() {
    use criogen_agent::retrieval::{Hit, Retriever, retriever_tool};
    use criogen_providers::BoxFuture;

    struct Fixed;
    impl Retriever for Fixed {
        fn retrieve<'a>(
            &'a self,
            query: &'a str,
            k: usize,
        ) -> BoxFuture<'a, Result<Vec<Hit>, RetrievalError>> {
            let hits = (0..k)
                .map(|i| Hit::new(format!("{query}-{i}"), "x".repeat(5_000), 1.0))
                .collect();
            Box::pin(async move { Ok(hits) })
        }
    }
    let tool = retriever_tool(Arc::new(Fixed), "lookup", "Look things up.");
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call(
            "lookup",
            json!({"query": "q", "k": 50}),
        ))
        .then(FakeReply::tool_call(
            "lookup",
            json!({"query": "q", "k": 3}),
        ))
        .then(FakeReply::text("ok"));
    let agent = Agent::builder(model).tool(tool).build().unwrap();
    let result = agent.run("go").await.unwrap();
    let tool_messages: Vec<_> = result
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 2);
    // More than 20 hits is refused by the schema; the model is told.
    let json_of = |m: &criogen_providers::Message| {
        m.content.iter().find_map(|b| match b {
            ContentBlock::Json { value, .. } => Some(value.clone()),
            _ => None,
        })
    };
    assert!(
        json_of(tool_messages[0]).is_none(),
        "{:?}",
        tool_messages[0]
    );
    let hits: Value = json_of(tool_messages[1]).unwrap();
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 3);
    // Texts are cut for the model.
    assert_eq!(hits[0]["text"].as_str().unwrap().chars().count(), 2_001);
}

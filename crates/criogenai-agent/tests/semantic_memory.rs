//! Semantic memory for agents (design 0008 §16.3), with a simulated
//! embedder: recall and `search_memory` by meaning, `remember` storing
//! vectors, and keyword fallback when the embedder fails.

use std::sync::Arc;
use std::time::Duration;

use criogenai_agent::{
    Agent, AgentInput, Memory, REMEMBER_TOOL, SEARCH_MEMORY_TOOL, memory_text, put_embedded,
};
use criogenai_core::{EventKind, event_channel};
use criogenai_providers::{ContentBlock, FakeEmbedder, FakeModel, FakeReply, Role, ToolStatus};
use criogenai_storage::{Embedding, Namespace, SearchQuery, Storage};
use serde_json::{Value, json};

fn ns(path: &str) -> Namespace {
    Namespace::parse(path).unwrap()
}

fn instant(replies: Vec<FakeReply>) -> Arc<FakeModel> {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    Arc::new(model)
}

/// "antibiotics" means "penicillin" to this embedder; no word is shared.
fn embedder() -> FakeEmbedder {
    FakeEmbedder::new().synonym("antibiotics", "penicillin")
}

/// Facts of patient p1, each with its vector (the app saves them).
async fn seed(storage: &Storage, embedder: &FakeEmbedder) {
    let store = storage.memory().as_ref();
    for (key, value) in [
        ("allergy", "Severe penicillin reaction in 2019"),
        ("city", "Lives in Hyderabad"),
        ("diet", "Vegetarian"),
    ] {
        put_embedded(store, embedder, &ns("patients/p1"), key, json!(value), None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn recalled(model: &FakeModel, n: usize) -> String {
    model.requests()[n]
        .messages
        .iter()
        .find(|m| m.id == "msg_memory")
        .map(|m| m.text())
        .unwrap_or_default()
}

/// The `memory_recalled` events of a run.
async fn run_with_events(agent: &Agent, input: AgentInput) -> Vec<Value> {
    let (sender, mut receiver) = event_channel(64);
    let run = agent.run_with(input, agent.run_options().events(sender));
    let events = async {
        let mut found = Vec::new();
        while let Some(event) = receiver.recv().await {
            if let EventKind::Custom { name, payload } = event.kind
                && name == "memory_recalled"
            {
                found.push(payload);
            }
        }
        found
    };
    let (result, events) = tokio::join!(run, events);
    result.unwrap();
    events
}

#[tokio::test]
async fn recall_finds_facts_by_meaning() {
    let storage = Storage::in_memory();
    let embedder = embedder();
    seed(&storage, &embedder).await;
    let model = instant(vec![FakeReply::text("Avoid penicillin.")]);
    let agent = Agent::builder_shared(model.clone())
        .storage(storage)
        .memory(
            Memory::by_state("patient", ns("patients"))
                .recall(1)
                .semantic(embedder.clone()),
        )
        .build()
        .unwrap();
    let events = run_with_events(
        &agent,
        AgentInput::from("Which antibiotics should I avoid?").with("patient", "p1"),
    )
    .await;
    // No word in common, but the allergy is recalled (not the newest fact).
    let text = recalled(&model, 0);
    assert!(
        text.contains("- allergy: Severe penicillin reaction"),
        "{text}"
    );
    assert!(!text.contains("Vegetarian"), "{text}");
    assert_eq!(
        events,
        [json!({"namespace": "patients/p1", "keys": ["allergy"], "search": "semantic"})]
    );
    // The question was embedded, and the facts as `key: value`.
    let calls = embedder.calls();
    assert_eq!(
        calls.last().unwrap(),
        &vec!["Which antibiotics should I avoid?".to_string()]
    );
    assert_eq!(
        calls[0],
        vec![memory_text(
            "allergy",
            &json!("Severe penicillin reaction in 2019")
        )]
    );
    assert_eq!(calls[0][0], "allergy: Severe penicillin reaction in 2019");

    // Keyword search would not have found it: the newest fact instead.
    let model = instant(vec![FakeReply::text("ok")]);
    let keywords = Agent::builder_shared(model.clone())
        .storage(agent_storage(&agent))
        .memory(Memory::by_state("patient", ns("patients")).recall(1))
        .build()
        .unwrap();
    keywords
        .run(AgentInput::from("Which antibiotics should I avoid?").with("patient", "p1"))
        .await
        .unwrap();
    assert!(recalled(&model, 0).contains("- diet: Vegetarian"));
}

fn agent_storage(agent: &Agent) -> Storage {
    agent.storage().clone()
}

#[tokio::test]
async fn remember_stores_a_vector_and_search_memory_uses_it() {
    let storage = Storage::in_memory();
    let embedder = embedder();
    let model = instant(vec![
        FakeReply::tool_call(
            REMEMBER_TOOL,
            json!({"key": "allergy", "value": "Severe penicillin reaction"}),
        ),
        FakeReply::text("Noted."),
        FakeReply::tool_call(
            SEARCH_MEMORY_TOOL,
            json!({"query": "antibiotics to avoid", "limit": 1}),
        ),
        FakeReply::text("Penicillin."),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .storage(storage.clone())
        .memory(
            Memory::by_state("patient", ns("patients"))
                .recall(0)
                .semantic(embedder.clone()),
        )
        .build()
        .unwrap();
    let input = || AgentInput::from("note it").with("patient", "p2");
    agent.run(input()).await.unwrap();
    // Stored with the vector of `allergy: …`, under the embedder's name.
    let query = Embedding::new(
        "fake/fake-embedding",
        embedder.vector("allergy: Severe penicillin reaction"),
    );
    let found = storage
        .memory()
        .search(&ns("patients/p2"), &SearchQuery::new().similar_to(query))
        .await
        .unwrap();
    assert_eq!(found[0].key, "allergy");
    assert!((found[0].score.unwrap() - 1.0).abs() < 1e-6);

    agent.run(input()).await.unwrap();
    let results = model_results(&model, 3);
    assert_eq!(results[0].0, Some(ToolStatus::Success));
    assert_eq!(
        serde_json::from_str::<Value>(&results[0].1).unwrap(),
        json!([{"key": "allergy", "value": "Severe penicillin reaction"}])
    );
    let spec = model.requests()[0]
        .tools
        .iter()
        .find(|t| t.name == SEARCH_MEMORY_TOOL)
        .unwrap()
        .description
        .clone();
    assert!(spec.contains("by meaning"), "{spec}");
}

#[tokio::test]
async fn a_failing_embedder_falls_back_to_keywords_and_remember_can_retry() {
    let storage = Storage::in_memory();
    // A fact saved without a vector (keywords still find it).
    storage
        .memory()
        .put(&ns("patients/p3"), "allergy", json!("penicillin"), None)
        .await
        .unwrap();
    let model = instant(vec![
        FakeReply::tool_call(REMEMBER_TOOL, json!({"key": "diet", "value": "vegan"})),
        FakeReply::tool_call(SEARCH_MEMORY_TOOL, json!({"query": "penicillin"})),
        FakeReply::text("done"),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .storage(storage.clone())
        .memory(Memory::by_state("patient", ns("patients")).semantic(FakeEmbedder::failing()))
        .build()
        .unwrap();
    let events = run_with_events(
        &agent,
        AgentInput::from("penicillin?").with("patient", "p3"),
    )
    .await;
    assert_eq!(events[0]["search"], "keyword");
    assert_eq!(events[0]["fallback"], "server");
    assert!(recalled(&model, 0).contains("- allergy: penicillin"));
    // remember failed and saved nothing; the model was told it may retry.
    let results = model_results(&model, 1);
    assert_eq!(results[0].0, Some(ToolStatus::Error));
    assert!(
        results[0].1.contains("embedding failed"),
        "{}",
        results[0].1
    );
    assert!(
        storage
            .memory()
            .get(&ns("patients/p3"), "diet")
            .await
            .unwrap()
            .is_none()
    );
    // search_memory fell back to keywords.
    let results = model_results(&model, 2);
    assert_eq!(
        serde_json::from_str::<Value>(&results[0].1).unwrap(),
        json!([{"key": "allergy", "value": "penicillin"}])
    );

    // The helper reports the embedder's failure.
    let err = put_embedded(
        storage.memory().as_ref(),
        &FakeEmbedder::failing(),
        &ns("patients/p3"),
        "k",
        json!("v"),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "model_failed");
    assert_eq!(err.details.unwrap()["model_error"]["kind"], "server");
}

#[tokio::test]
async fn an_empty_question_recalls_the_newest_without_embedding() {
    let storage = Storage::in_memory();
    let embedder = embedder();
    seed(&storage, &embedder).await;
    let before = embedder.calls().len();
    let model = instant(vec![FakeReply::text("ok")]);
    let agent = Agent::builder_shared(model.clone())
        .storage(storage)
        .memory(
            Memory::by_state("patient", ns("patients"))
                .recall(2)
                .semantic(embedder.clone()),
        )
        .build()
        .unwrap();
    agent
        .run(AgentInput::from("").with("patient", "p1"))
        .await
        .unwrap();
    assert_eq!(embedder.calls().len(), before);
    let text = recalled(&model, 0);
    assert!(
        text.contains("- diet: Vegetarian") && text.contains("- city:"),
        "{text}"
    );
}

/// The tool results request `n` ends with: (status, text).
fn model_results(model: &FakeModel, n: usize) -> Vec<(Option<ToolStatus>, String)> {
    let messages = &model.requests()[n].messages;
    let mut results: Vec<_> = messages
        .iter()
        .rev()
        .take_while(|m| m.role == Role::Tool)
        .map(|m| {
            let text = m
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Json { value, .. } => value.to_string(),
                    _ => m.text(),
                })
                .collect::<String>();
            (m.status, text)
        })
        .collect();
    results.reverse();
    results
}

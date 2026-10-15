//! Agents on Postgres (design 0008 §14): processes that share a thread
//! take turns, interrupt or reject each other, and share long-term memory.
//! Each `Storage::connect` stands for a process (its own connections and
//! thread locks). Crash recovery across processes is in `durable.rs`.
//!
//! Needs a server: `bash scripts/with-postgres.sh cargo test …` (skips
//! without one). Thread ids and namespaces are unique per run, so a
//! long-lived server works too.
#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::time::Duration;

use agentbuild_agent::{Agent, AgentInput, Memory, REMEMBER_TOOL};
use agentbuild_providers::{FakeModel, FakeReply, Role};
use agentbuild_storage::{Namespace, OnBusy, Storage};
use serde_json::json;

fn server() -> Option<String> {
    let url = std::env::var("AGENTBUILD_TEST_POSTGRES_URL").ok();
    if url.is_none() {
        assert!(std::env::var_os("AGENTBUILD_REQUIRE_POSTGRES").is_none());
        eprintln!("skipped: no AGENTBUILD_TEST_POSTGRES_URL (see scripts/with-postgres.sh)");
    }
    url
}

fn unique(name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{name}-{nanos}-{}", std::process::id())
}

/// A process with an agent whose model answers after 400 ms.
async fn process(url: &str, on_busy: OnBusy) -> Agent {
    let model = FakeModel::with_delay(Duration::from_millis(400), Duration::from_millis(400));
    Agent::builder(model)
        .storage(Storage::connect(url).await.unwrap().on_busy(on_busy))
        .build()
        .unwrap()
}

/// Process A sends "one"; while it runs, process B sends "two".
async fn overlapping(
    url: &str,
    second: OnBusy,
) -> (Result<u64, String>, Result<Vec<(Role, String)>, String>) {
    let (a, b) = (process(url, OnBusy::Wait).await, process(url, second).await);
    let thread = unique("chat");
    let first = {
        let chat = a.thread(&thread);
        tokio::spawn(async move { chat.send("one").await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = b.thread(&thread).send("two").await;
    let first = first.await.unwrap();
    b.thread(&thread).delete().await.unwrap();
    (
        first.map(|r| r.version).map_err(|e| e.code),
        second
            .map(|r| {
                r.result
                    .messages
                    .iter()
                    .map(|m| (m.role, m.text()))
                    .collect()
            })
            .map_err(|e| e.code),
    )
}

#[tokio::test]
async fn processes_on_one_thread_take_turns() {
    let Some(url) = server() else {
        return;
    };
    let (first, second) = overlapping(&url, OnBusy::Wait).await;
    assert_eq!(first, Ok(2));
    let texts: Vec<String> = second.unwrap().into_iter().map(|(_, t)| t).collect();
    assert_eq!(
        texts,
        [
            "one",
            "[fake-llm] reply to: one",
            "two",
            "[fake-llm] reply to: two"
        ]
    );
}

#[tokio::test]
async fn a_process_can_interrupt_or_be_rejected_by_another() {
    let Some(url) = server() else {
        return;
    };
    let (first, second) = overlapping(&url, OnBusy::Interrupt).await;
    assert_eq!(first, Err("cancelled".into()));
    // The second run starts from the cancelled run's saved state.
    let roles: Vec<Role> = second.unwrap().into_iter().map(|(role, _)| role).collect();
    assert_eq!(roles, [Role::User, Role::User, Role::Assistant]);

    let (first, second) = overlapping(&url, OnBusy::Reject).await;
    assert_eq!(first, Ok(2));
    assert_eq!(second, Err("thread_busy".into()));
}

#[tokio::test]
async fn memory_is_shared_by_every_process() {
    let Some(url) = server() else {
        return;
    };
    let patients = Namespace::parse("patients").unwrap();
    let patient = unique("p");
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call(
            REMEMBER_TOOL,
            json!({"key": "allergy", "value": "penicillin"}),
        ))
        .then(FakeReply::text("Noted."));
    let a = Agent::builder(model)
        .storage(Storage::connect(&url).await.unwrap())
        .memory(Memory::by_state("patient", patients.clone()))
        .build()
        .unwrap();
    a.thread(unique("visit"))
        .send(AgentInput::from("I'm allergic to penicillin").with("patient", patient.as_str()))
        .await
        .unwrap();

    let model = Arc::new(
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO).then(FakeReply::text("Noted.")),
    );
    let b = Agent::builder_shared(model.clone())
        .storage(Storage::connect(&url).await.unwrap())
        .memory(Memory::by_state("patient", patients.clone()))
        .build()
        .unwrap();
    b.thread(unique("visit"))
        .send(
            AgentInput::from("Which antibiotic suits my allergy?")
                .with("patient", patient.as_str()),
        )
        .await
        .unwrap();
    let recalled = model.requests()[0]
        .messages
        .iter()
        .find(|m| m.id == "msg_memory")
        .map(|m| m.text())
        .unwrap();
    assert!(recalled.contains("- allergy: penicillin"), "{recalled}");
    let removed = b
        .storage()
        .memory()
        .delete_namespace(&patients.child(patient).unwrap())
        .await
        .unwrap();
    assert_eq!(removed, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_processes_share_many_busy_threads_without_losing_a_message() {
    let Some(url) = server() else {
        return;
    };
    // Three processes; models answer after 5–20 ms.
    let mut agents = Vec::new();
    for _ in 0..3 {
        let model = FakeModel::with_delay(Duration::from_millis(5), Duration::from_millis(20));
        agents.push(
            Agent::builder(model)
                .storage(Storage::connect(&url).await.unwrap())
                .build()
                .unwrap(),
        );
    }
    let run = unique("stress");
    // 8 threads; every process sends 3 messages to each, all at once. They
    // queue across processes (OnBusy::Wait): 9 turns per thread.
    let mut tasks = Vec::new();
    for thread in 0..8 {
        for (process, agent) in agents.iter().enumerate() {
            let chat = agent.thread(format!("{run}-{thread}"));
            tasks.push(tokio::spawn(async move {
                for turn in 0..3 {
                    chat.send(format!("p{process} t{turn}")).await.unwrap();
                }
            }));
        }
    }
    for task in tasks {
        task.await.unwrap();
    }
    for thread in 0..8 {
        let chat = agents[thread % 3].thread(format!("{run}-{thread}"));
        let messages = chat.messages().await.unwrap();
        assert_eq!(messages.len(), 18, "thread {thread}");
        // Turns never interleave: each answer follows its question.
        for pair in messages.chunks(2) {
            assert!(
                pair[1].text().ends_with(&pair[0].text()),
                "{} / {}",
                pair[0].text(),
                pair[1].text()
            );
        }
        // Each process's turns land in its own order.
        for process in 0..3 {
            let own: Vec<String> = messages
                .iter()
                .step_by(2)
                .map(|m| m.text())
                .filter(|t| t.starts_with(&format!("p{process} ")))
                .collect();
            let expected: Vec<String> = (0..3).map(|t| format!("p{process} t{t}")).collect();
            assert_eq!(own, expected, "thread {thread}");
        }
        assert!(chat.delete().await.unwrap());
    }
}

//! An agent on encrypted storage with retention (design 0008 §15): a
//! conversation and its remembered facts never reach the database file in
//! readable form, and a thread shortened by retention goes on as before.
#![cfg(all(feature = "sqlite", feature = "encryption"))]

use std::sync::Arc;
use std::time::Duration;

use agentbuild_agent::{Agent, AgentInput, Memory, REMEMBER_TOOL};
use agentbuild_providers::{FakeModel, FakeReply};
use agentbuild_storage::{EncryptionKey, Keyring, Namespace, Retention, Storage};
use serde_json::json;

const SECRET: &str = "penicillin";

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("agentbuild-at-rest-{}.db", uuid_like())))
    }

    fn url(&self) -> String {
        format!("sqlite://{}", self.0.display())
    }

    /// The database file and its write-ahead log, as text.
    fn bytes(&self) -> String {
        let mut all = Vec::new();
        for suffix in ["", "-wal"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            all.extend(std::fs::read(path).unwrap_or_default());
        }
        String::from_utf8_lossy(&all).into_owned()
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

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}

fn keys() -> Keyring {
    Keyring::new(EncryptionKey::new("k1", &[7; 32]).unwrap())
}

#[tokio::test]
async fn a_patient_conversation_is_encrypted_and_survives_retention() {
    let db = TempDb::new();
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in [
        FakeReply::tool_call(
            REMEMBER_TOOL,
            json!({"key": format!("allergy_{SECRET}"), "value": format!("Severe {SECRET} allergy")}),
        ),
        FakeReply::text("Noted."),
        FakeReply::text("You are allergic to it."),
        FakeReply::text("Still noted."),
    ] {
        model = model.then(reply);
    }
    let model = Arc::new(model);
    let storage = Storage::connect_encrypted(&db.url(), keys())
        .await
        .unwrap()
        .retention(Retention::new().keep_checkpoints(2));
    let agent = Agent::builder_shared(model.clone())
        .storage(storage.clone())
        .memory(Memory::by_state(
            "patient",
            Namespace::parse("patients").unwrap(),
        ))
        .build()
        .unwrap();

    let chat = agent.thread("visit-7f3a");
    chat.send(AgentInput::from(format!("I am allergic to {SECRET}.")).with("patient", "p-7f3a"))
        .await
        .unwrap();
    chat.send("What am I allergic to?").await.unwrap();
    // The fact was remembered and recalled.
    let recalled = model.requests()[2]
        .messages
        .iter()
        .find(|m| m.id == "msg_memory")
        .map(|m| m.text())
        .unwrap();
    assert!(recalled.contains(SECRET), "{recalled}");

    // Nothing readable in the file or its log.
    assert!(!db.bytes().contains(SECRET));
    assert!(!db.bytes().contains("allergic"));

    // Retention keeps the last two checkpoints; the thread goes on with its
    // whole conversation.
    let before = chat.history(100).await.unwrap().len();
    assert!(before > 2, "{before}");
    let report = storage.prune().await.unwrap();
    assert_eq!(report.checkpoints_deleted as usize, before - 2);
    assert_eq!(chat.history(100).await.unwrap().len(), 2);
    let reply = chat.send("Thanks.").await.unwrap();
    assert_eq!(reply.result.messages.len(), 8);
    let last = model.requests().last().unwrap().clone();
    assert!(last.messages.iter().any(|m| m.text().contains(SECRET)));
    assert!(!db.bytes().contains(SECRET));

    // Without the keys the database does not open at all.
    drop(agent);
    let err = Storage::connect(&db.url()).await.unwrap_err();
    assert_eq!(err.code(), "encryption_key_missing");
}

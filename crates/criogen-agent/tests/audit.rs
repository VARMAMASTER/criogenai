//! The audit trail of agents (design 0010): who did what on a thread,
//! approvals recorded before the call runs, no content in any record,
//! fail closed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogen_agent::{
    Agent, AgentInput, Approval, Decision, Memory, REMEMBER_TOOL, ToolError, tool_fn,
};
use criogen_core::BoxFuture;
use criogen_providers::{FakeModel, FakeReply, ToolSpec};
use criogen_storage::{
    AuditEvent, AuditLog, AuditQuery, AuditRecord, AuditVerification, Namespace, Storage,
    StorageError, StorageErrorKind, sha256_hex,
};
use serde_json::{Value, json};

fn instant(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    model
}

type Log = Arc<Mutex<Vec<String>>>;

fn prescribe(log: &Log) -> impl criogen_agent::Tool {
    let log = log.clone();
    tool_fn(
        ToolSpec::new("send_prescription", "", json!({"type": "object"})),
        move |args, _ctx| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(Value::Object(args).to_string());
                Ok::<_, ToolError>("sent")
            }
        },
    )
}

/// A secret that must never appear in a record: in the user message, the
/// tool arguments and the tool result.
const SECRET: &str = "amoxicillin-500mg";

fn prescribing_agent(storage: &Storage, log: &Log) -> Agent {
    Agent::builder(instant(vec![
        FakeReply::tool_call("send_prescription", json!({"drug": SECRET})),
        FakeReply::text("Sent."),
    ]))
    .tool(prescribe(log))
    .approval(Approval::new().tool("send_prescription"))
    .storage(storage.clone())
    .build()
    .unwrap()
}

async fn records(storage: &Storage) -> Vec<AuditRecord> {
    storage
        .audit()
        .unwrap()
        .query(&AuditQuery::new())
        .await
        .unwrap()
}

fn summary(records: &[AuditRecord]) -> Vec<String> {
    records
        .iter()
        .map(|r| {
            format!(
                "{} {} {} {}",
                r.event.action,
                r.event.actor.as_deref().unwrap_or("-"),
                r.event.subject.as_deref().unwrap_or("-"),
                r.event.outcome.as_deref().unwrap_or("-"),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_thread_records_who_did_what_and_nothing_it_said() {
    let storage = Storage::in_memory().with_audit();
    let log = Log::default();
    let agent = prescribing_agent(&storage, &log);
    let reply = agent
        .thread("visit-1")
        .actor("user:nurse-kim")
        .send(format!("Patient needs {SECRET}"))
        .await
        .unwrap();
    assert!(reply.needs_approval());
    let call_id = reply.approvals()[0].call_id.clone();
    assert!(log.lock().unwrap().is_empty());

    agent
        .thread("visit-1")
        .actor("user:dr-rao")
        .approve()
        .await
        .unwrap();
    assert_eq!(log.lock().unwrap().len(), 1);

    let all = records(&storage).await;
    assert_eq!(
        summary(&all),
        [
            "model.called user:nurse-kim fake/fake ok",
            "run.finished user:nurse-kim send suspended",
            "approval.requested user:nurse-kim send_prescription pending",
            "approval.decided user:dr-rao send_prescription approve",
            "tool.executed user:dr-rao send_prescription success",
            "model.called user:dr-rao fake/fake ok",
            "run.finished user:dr-rao decide completed",
        ]
    );
    // One run, one thread, the same call.
    assert!(
        all.iter()
            .all(|r| r.event.thread_id.as_deref() == Some("visit-1"))
    );
    let run = all[0].event.run_id.clone().unwrap();
    assert!(
        all.iter()
            .all(|r| r.event.run_id.as_deref() == Some(run.as_str()))
    );
    for record in &all[2..5] {
        assert_eq!(record.event.details["call_id"], json!(call_id));
    }
    // The decision is recorded before the call ran.
    assert!(all[3].seq < all[4].seq);
    let head = agent.thread("visit-1").head().await.unwrap().unwrap();
    assert_eq!(all[1].event.details["version"], json!(reply.version));
    assert_eq!(all[6].event.details["version"], json!(head.version));
    // No content anywhere.
    let text = serde_json::to_string(&all).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    assert!(!text.contains("Patient needs"), "{text}");
    assert!(storage.audit().unwrap().verify().await.unwrap().ok);
}

#[tokio::test]
async fn rejections_invalid_decisions_rewinds_and_deletes() {
    let storage = Storage::in_memory().with_audit();
    let log = Log::default();
    let agent = prescribing_agent(&storage, &log);
    let chat = agent.thread("visit-2").actor("user:dr-rao");
    chat.send("hi").await.unwrap();

    // An invalid decision changes nothing and records nothing.
    let before = records(&storage).await.len();
    let err = chat
        .decide([("no-such-call".to_string(), Decision::approve())])
        .await
        .unwrap_err();
    assert_eq!(err.code, "resume_invalid");
    assert_eq!(records(&storage).await.len(), before);

    chat.reject("not today").await.unwrap();
    let all = records(&storage).await;
    let actions: Vec<&str> = all.iter().map(|r| r.event.action.as_str()).collect();
    assert!(actions.contains(&"approval.decided"));
    // Rejected: never executed.
    assert!(!actions.contains(&"tool.executed"));
    assert!(log.lock().unwrap().is_empty());
    let decided = all
        .iter()
        .find(|r| r.event.action == "approval.decided")
        .unwrap();
    assert_eq!(decided.event.outcome.as_deref(), Some("reject"));
    assert!(!serde_json::to_string(&all).unwrap().contains("not today"));

    let first = chat.history(10).await.unwrap().last().unwrap().id.clone();
    chat.rewind(&first).await.unwrap();
    assert!(chat.delete().await.unwrap());
    assert!(!chat.delete().await.unwrap());
    let tail = summary(&records(&storage).await);
    assert_eq!(
        tail[tail.len() - 3..],
        [
            format!("thread.rewound user:dr-rao {first} ok"),
            "thread.deleted user:dr-rao - deleted".to_string(),
            "thread.deleted user:dr-rao - not_found".to_string(),
        ]
    );
}

#[tokio::test]
async fn memory_writes_are_recorded_by_key_hash_and_runs_without_a_thread_too() {
    let storage = Storage::in_memory().with_audit();
    let agent = Agent::builder(instant(vec![
        FakeReply::tool_call(
            REMEMBER_TOOL,
            json!({"key": "allergy_penicillin", "value": "severe"}),
        ),
        FakeReply::text("Noted."),
    ]))
    .storage(storage.clone())
    .memory(Memory::by_state(
        "patient",
        Namespace::parse("patients").unwrap(),
    ))
    .build()
    .unwrap();
    agent
        .run(AgentInput::from("remember it").with("patient", "p1"))
        .await
        .unwrap();
    let all = records(&storage).await;
    let saved = all
        .iter()
        .find(|r| r.event.action == "memory.saved")
        .unwrap();
    assert_eq!(saved.event.details["namespace"], json!("patients/p1"));
    assert_eq!(
        saved.event.details["key_sha256"],
        json!(sha256_hex("allergy_penicillin"))
    );
    let text = serde_json::to_string(&all).unwrap();
    assert!(
        !text.contains("penicillin") && !text.contains("severe"),
        "{text}"
    );
    // A plain run: no thread, no actor.
    assert!(
        all.iter()
            .all(|r| r.event.thread_id.is_none() && r.event.actor.is_none())
    );
    assert_eq!(
        all.iter()
            .filter(|r| r.event.action == "tool.executed")
            .count(),
        1
    );
}

/// An audit log that is down.
struct Down;

impl AuditLog for Down {
    fn append(&self, _: AuditEvent) -> BoxFuture<'_, Result<AuditRecord, StorageError>> {
        Box::pin(async { Err(StorageError::new(StorageErrorKind::Unavailable, "down")) })
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
async fn nothing_runs_unrecorded() {
    let storage = Storage::in_memory().with_audit_log(Down);
    let log = Log::default();
    let agent = prescribing_agent(&storage, &log);
    let err = agent.thread("t").send("hi").await.unwrap_err();
    assert_eq!(err.code, "audit_failed");

    // A tool call that cannot be recorded fails its step, and its result
    // is not saved (a retry runs and records it).
    let agent = Agent::builder(instant(vec![
        FakeReply::tool_call("send_prescription", json!({})),
        FakeReply::text("done"),
    ]))
    .tool(prescribe(&log))
    .storage(Storage::in_memory().with_audit_log(OnlyModels::default()))
    .build()
    .unwrap();
    let err = agent.run("go").await.unwrap_err();
    assert_eq!(err.code, "audit_failed", "{}", err.message);
}

/// Records model calls but fails on anything else.
#[derive(Default)]
struct OnlyModels(criogen_storage::InMemoryAuditLog);

impl AuditLog for OnlyModels {
    fn append(&self, event: AuditEvent) -> BoxFuture<'_, Result<AuditRecord, StorageError>> {
        if event.action == "model.called" {
            self.0.append(event)
        } else {
            Down.append(event)
        }
    }

    fn query<'a>(
        &'a self,
        query: &'a AuditQuery,
    ) -> BoxFuture<'a, Result<Vec<AuditRecord>, StorageError>> {
        self.0.query(query)
    }

    fn verify(&self) -> BoxFuture<'_, Result<AuditVerification, StorageError>> {
        self.0.verify()
    }

    fn head(&self) -> BoxFuture<'_, Result<Option<(u64, String)>, StorageError>> {
        self.0.head()
    }
}

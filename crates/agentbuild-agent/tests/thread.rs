//! Threads: conversations kept in storage across runs (design 0008 §11),
//! with `FakeModel` on the paused clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentbuild_agent::{Agent, Approval, Decision, ToolError, tool_fn};
use agentbuild_core::{BoxFuture, CancelToken, PendingWrite};
use agentbuild_providers::{
    FakeModel, FakeReply, Message, ModelError, ModelErrorKind, Role, ToolSpec,
};
use agentbuild_storage::{
    Checkpoint, CheckpointMeta, CheckpointStatus, Checkpointer, Durability, InMemoryCheckpointer,
    OnBusy, Source, Storage, StorageError, StorageErrorKind,
};
use serde_json::{Map, Value, json};

type Log = Arc<Mutex<Vec<String>>>;

fn instant(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    model
}

fn tool(name: &'static str, log: &Log) -> impl agentbuild_agent::Tool {
    let log = log.clone();
    tool_fn(
        ToolSpec::new(name, "", json!({"type": "object"})),
        move |args, _ctx| {
            let log = log.clone();
            async move {
                log.lock()
                    .unwrap()
                    .push(format!("{name}({})", Value::Object(args)));
                Ok::<_, ToolError>(format!("{name} done"))
            }
        },
    )
}

fn texts(messages: &[Message]) -> Vec<(Role, String)> {
    messages.iter().map(|m| (m.role, m.text())).collect()
}

fn sources(history: &[CheckpointMeta]) -> Vec<(u64, Source)> {
    history.iter().map(|c| (c.version, c.source)).collect()
}

/// An agent that asks to refund before answering, with refunds gated.
fn refund_agent(storage: &Storage, log: &Log, replies: Vec<FakeReply>) -> Agent {
    Agent::builder(instant(replies))
        .tool(tool("refund", log))
        .approval(Approval::new().tool("refund"))
        .storage(storage.clone())
        .build()
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_thread_remembers_the_conversation() {
    let model = Arc::new(instant(vec![
        FakeReply::text("Hello Ada"),
        FakeReply::text("Your name is Ada"),
    ]));
    let agent = Agent::builder_shared(model.clone()).build().unwrap();
    let chat = agent.thread("user-42");
    assert_eq!(chat.id(), "user-42");
    assert!(chat.head().await.unwrap().is_none());
    assert!(chat.messages().await.unwrap().is_empty());

    // Each call writes an `input` checkpoint before the first step and its
    // outcome at the end (durability `step`).
    let first = chat.send("I am Ada").await.unwrap();
    assert_eq!((first.text(), first.version), (Some("Hello Ada"), 2));
    let second = chat.send("What is my name?").await.unwrap();
    assert_eq!(second.version, 4);
    assert_eq!(second.text(), Some("Your name is Ada"));
    // Bookkeeping is per run; the transcript carries over.
    assert_eq!(second.result.model_calls, 1);
    assert_eq!(
        texts(&model.requests()[1].messages),
        [
            (Role::User, "I am Ada".into()),
            (Role::Assistant, "Hello Ada".into()),
            (Role::User, "What is my name?".into()),
        ]
    );
    assert_eq!(chat.messages().await.unwrap().len(), 4);

    let history = chat.history(10).await.unwrap();
    assert_eq!(
        sources(&history),
        [
            (4, Source::Step),
            (3, Source::Input),
            (2, Source::Step),
            (1, Source::Input)
        ]
    );
    assert_eq!(history[0].id, second.checkpoint_id);
    assert_eq!(history[0].parent_id.as_deref(), Some(&*history[1].id));
    assert_eq!(history[1].parent_id.as_deref(), Some(&*first.checkpoint_id));
    assert_eq!(history[0].status, CheckpointStatus::Completed);
    assert_eq!(history[1].status, CheckpointStatus::InProgress);
    assert_eq!(history[0].run_id, second.result.run.run_id);
    assert_eq!(history[1].run_id, second.result.run.run_id);
    assert_eq!(chat.history(1).await.unwrap().len(), 1);

    // Another thread on the same agent starts empty.
    let other = agent.thread("user-7").send("hi").await.unwrap();
    assert_eq!(other.version, 2);
    assert_eq!(other.result.messages.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_approval_waits_in_storage_for_another_agent_instance() {
    let storage = Storage::in_memory();
    let log = Log::default();
    let calls = || {
        vec![
            FakeReply::tool_call("refund", json!({"order": "A-1001"})),
            FakeReply::text("Refunded."),
        ]
    };
    let first = refund_agent(&storage, &log, calls());
    let chat = first.thread("user-42");
    let paused = chat.send("Refund my order").await.unwrap();
    assert!(paused.needs_approval());
    assert_eq!(paused.approvals()[0].tool, "refund");
    assert!(log.lock().unwrap().is_empty());

    let busy = chat.send("Hello?").await.unwrap_err();
    assert_eq!(busy.code, "thread_suspended");
    let not_failed = chat.retry().await.unwrap_err();
    assert_eq!(not_failed.code, "resume_invalid");
    assert!(
        not_failed
            .message
            .contains("no failed or interrupted run to retry"),
        "{}",
        not_failed.message
    );
    // input, the model's step, the suspension.
    assert_eq!(chat.head().await.unwrap().unwrap().version, 3);

    // A new process: another agent instance on the same storage. Its model
    // script starts after the tool call.
    let second = refund_agent(&storage, &log, calls().split_off(1));
    let chat = second.thread("user-42");
    let pending = chat.pending_approvals().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].args["order"], "A-1001");
    let done = chat.approve().await.unwrap();
    assert_eq!(done.text(), Some("Refunded."));
    assert_eq!(*log.lock().unwrap(), [r#"refund({"order":"A-1001"})"#]);
    assert!(chat.pending_approvals().await.unwrap().is_empty());

    // Approving twice (double click) finds nothing pending.
    let again = chat.approve().await.unwrap_err();
    assert_eq!(again.code, "resume_invalid");
    // The approval: input (with the decisions), review, tools, model.
    assert_eq!(
        sources(&chat.history(10).await.unwrap()),
        [
            (7, Source::Step),
            (6, Source::Step),
            (5, Source::Step),
            (4, Source::Input),
            (3, Source::Suspend),
            (2, Source::Step),
            (1, Source::Input)
        ]
    );
    assert_eq!(
        chat.history(10).await.unwrap()[4].status,
        CheckpointStatus::Suspended
    );
}

#[tokio::test(start_paused = true)]
async fn reject_and_decide_answer_pending_calls() {
    let storage = Storage::in_memory();
    let log = Log::default();
    let model = Arc::new(instant(vec![
        FakeReply::tool_call("refund", json!({"order": "A-1"})),
        FakeReply::text("OK, no refund."),
        FakeReply::tool_call("refund", json!({"order": "A-2"})),
        FakeReply::text("Refunded A-3."),
    ]));
    let agent = Agent::builder_shared(model.clone())
        .tool(tool("refund", &log))
        .approval(Approval::new().tool("refund"))
        .storage(storage)
        .build()
        .unwrap();
    let chat = agent.thread("t");

    chat.send("refund A-1").await.unwrap();
    let rejected = chat.reject("Too expensive").await.unwrap();
    assert_eq!(rejected.text(), Some("OK, no refund."));
    let result = rejected.result.messages[2].text();
    assert!(result.contains("Too expensive"), "{result}");

    let paused = chat.send("refund A-2").await.unwrap();
    let call_id = paused.approvals()[0].call_id.clone();
    // A decision for an unknown call is refused; the approval stays.
    let wrong = chat
        .decide([("nope".to_string(), Decision::approve())])
        .await
        .unwrap_err();
    assert_eq!(wrong.code, "resume_invalid");
    assert_eq!(chat.head().await.unwrap().unwrap().version, 10);
    assert_eq!(chat.pending_approvals().await.unwrap().len(), 1);

    let mut args = Map::new();
    args.insert("order".into(), json!("A-3"));
    let edited = chat
        .decide([(call_id, Decision::edit(args))])
        .await
        .unwrap();
    assert_eq!(edited.text(), Some("Refunded A-3."));
    assert_eq!(*log.lock().unwrap(), [r#"refund({"order":"A-3"})"#]);
    assert_eq!(edited.version, 14);
}

#[tokio::test(start_paused = true)]
async fn a_failed_run_is_saved_and_retried_without_rerunning_tools() {
    let log = Log::default();
    let error = ModelError::new(ModelErrorKind::InvalidRequest, "bad request");
    let agent = Agent::builder(instant(vec![
        FakeReply::tool_call("lookup", json!({})),
        FakeReply::Error(error),
        FakeReply::text("Found it."),
    ]))
    .tool(tool("lookup", &log))
    .build()
    .unwrap();
    let chat = agent.thread("t");

    let failed = chat.send("find it").await.unwrap_err();
    assert_eq!(failed.code, "model_failed");
    let head = chat.head().await.unwrap().unwrap();
    assert_eq!(
        (head.source, head.status()),
        (Source::Failure, CheckpointStatus::Failed)
    );
    assert_eq!(head.state, failed.snapshot().unwrap().state);
    assert_eq!(log.lock().unwrap().len(), 1);

    let retried = chat.retry().await.unwrap();
    assert_eq!(retried.text(), Some("Found it."));
    assert_eq!(log.lock().unwrap().len(), 1, "the lookup is not run again");
    assert_eq!(retried.version, 6);
    let nothing = chat.retry().await.unwrap_err();
    assert_eq!(nothing.code, "resume_invalid");
}

#[tokio::test(start_paused = true)]
async fn send_after_a_failure_continues_from_the_saved_state() {
    let agent = Agent::builder(instant(vec![
        FakeReply::Error(ModelError::new(ModelErrorKind::InvalidRequest, "bad")),
        FakeReply::text("Hi again"),
    ]))
    .build()
    .unwrap();
    let chat = agent.thread("t");
    chat.send("first").await.unwrap_err();
    let reply = chat.send("second").await.unwrap();
    assert_eq!(
        texts(&reply.result.messages),
        [
            (Role::User, "first".into()),
            (Role::User, "second".into()),
            (Role::Assistant, "Hi again".into()),
        ]
    );
}

/// Two `send`s on one thread, the second started while the first runs
/// (`FakeModel` answers after 2–3 s).
async fn overlapping(storage: Storage) -> (Result<u64, String>, Result<Vec<Message>, String>) {
    let model = Arc::new(FakeModel::new());
    let agent = Agent::builder_shared(model)
        .storage(storage)
        .build()
        .unwrap();
    let first = {
        let chat = agent.thread("t");
        tokio::spawn(async move { chat.send("one").await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let second = agent.thread("t").send("two").await;
    let first = first.await.unwrap();
    (
        first.map(|r| r.version).map_err(|e| e.code),
        second.map(|r| r.result.messages).map_err(|e| e.code),
    )
}

#[tokio::test(start_paused = true)]
async fn a_second_send_waits_its_turn() {
    let (first, second) = overlapping(Storage::in_memory()).await;
    assert_eq!(first, Ok(2));
    let second = second.unwrap();
    assert_eq!(
        texts(&second)
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>(),
        [
            "one",
            "[fake-llm] reply to: one",
            "two",
            "[fake-llm] reply to: two"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn busy_threads_can_reject_time_out_or_interrupt() {
    let (first, second) = overlapping(Storage::in_memory().on_busy(OnBusy::Reject)).await;
    assert_eq!((first, second.unwrap_err()), (Ok(2), "thread_busy".into()));

    let storage = Storage::in_memory().busy_timeout(Duration::from_millis(500));
    let (first, second) = overlapping(storage).await;
    assert_eq!((first, second.unwrap_err()), (Ok(2), "thread_busy".into()));

    let storage = Storage::in_memory().on_busy(OnBusy::Interrupt);
    let (first, second) = overlapping(storage).await;
    assert_eq!(first, Err("cancelled".into()));
    // The second run starts from the cancelled run's saved state.
    let roles: Vec<Role> = second.unwrap().iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::User, Role::Assistant]);
}

#[tokio::test(start_paused = true)]
async fn cancel_stops_the_active_run_and_keeps_the_callers_token() {
    let agent = Agent::builder(FakeModel::new()).build().unwrap();
    let chat = agent.thread("t");
    assert!(!chat.cancel(), "nothing is running");

    let running = {
        let chat = chat.clone();
        tokio::spawn(async move { chat.send("hi").await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(chat.cancel());
    assert_eq!(running.await.unwrap().unwrap_err().code, "cancelled");
    assert_eq!(
        chat.head().await.unwrap().unwrap().status(),
        CheckpointStatus::Failed
    );

    let token = CancelToken::new();
    let running = {
        let chat = chat.clone();
        let options = agent.run_options().cancel_token(token.clone());
        tokio::spawn(async move { chat.send_with("hi", options).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    token.cancel();
    assert_eq!(running.await.unwrap().unwrap_err().code, "cancelled");
}

#[tokio::test(start_paused = true)]
async fn rewind_makes_an_older_checkpoint_the_head() {
    let agent = Agent::builder(instant(vec![
        FakeReply::text("a1"),
        FakeReply::text("a2"),
        FakeReply::text("a2 again"),
    ]))
    .build()
    .unwrap();
    let chat = agent.thread("t");
    let first = chat.send("q1").await.unwrap();
    chat.send("q2").await.unwrap();

    let head = chat.rewind(&first.checkpoint_id).await.unwrap();
    assert_eq!((head.version, head.source), (5, Source::Fork));
    assert_eq!(head.parent_id.as_deref(), Some(&*first.checkpoint_id));
    assert_eq!(chat.messages().await.unwrap().len(), 2);
    assert_eq!(
        sources(&chat.history(10).await.unwrap()),
        [(5, Source::Fork), (2, Source::Step), (1, Source::Input)]
    );

    let reply = chat.send("q2, differently").await.unwrap();
    assert_eq!(reply.version, 7);
    assert_eq!(
        texts(&reply.result.messages)
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>(),
        ["q1", "a1", "q2, differently", "a2 again"]
    );

    let missing = chat.rewind("no-such-checkpoint").await.unwrap_err();
    assert_eq!(missing.code, "checkpoint_not_found");
}

#[tokio::test(start_paused = true)]
async fn rewinding_to_a_suspension_makes_it_pending_again() {
    let storage = Storage::in_memory();
    let log = Log::default();
    let agent = refund_agent(
        &storage,
        &log,
        vec![
            FakeReply::tool_call("refund", json!({})),
            FakeReply::text("No refund."),
            FakeReply::text("Refunded."),
        ],
    );
    let chat = agent.thread("t");
    let paused = chat.send("refund").await.unwrap();
    chat.reject("no").await.unwrap();
    chat.rewind(&paused.checkpoint_id).await.unwrap();
    assert_eq!(chat.pending_approvals().await.unwrap().len(), 1);
    assert_eq!(chat.approve().await.unwrap().text(), Some("Refunded."));
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn delete_removes_the_thread() {
    let agent = Agent::builder(instant(vec![])).build().unwrap();
    let chat = agent.thread("t");
    chat.send("hi").await.unwrap();
    assert!(chat.delete().await.unwrap());
    assert!(chat.head().await.unwrap().is_none());
    assert!(chat.history(10).await.unwrap().is_empty());
    assert!(!chat.delete().await.unwrap());
    assert_eq!(chat.send("hi").await.unwrap().version, 2);
}

/// Loads from memory but cannot write.
struct ReadOnly(InMemoryCheckpointer);

impl Checkpointer for ReadOnly {
    fn load<'a>(
        &'a self,
        thread: &'a str,
        at: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Checkpoint>, StorageError>> {
        self.0.load(thread, at)
    }

    fn commit(&self, _: Checkpoint, _: u64) -> BoxFuture<'_, Result<(), StorageError>> {
        Box::pin(async {
            Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "database is down",
            ))
        })
    }

    fn history<'a>(
        &'a self,
        thread: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<CheckpointMeta>, StorageError>> {
        self.0.history(thread, limit)
    }

    fn delete_thread<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<bool, StorageError>> {
        self.0.delete_thread(thread)
    }

    fn put_pending<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: PendingWrite,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async {
            Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "database is down",
            ))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_commit_reports_the_storage_error_with_the_run_error() {
    let agent = Agent::builder(instant(vec![
        FakeReply::text("hi"),
        FakeReply::Error(ModelError::new(ModelErrorKind::InvalidRequest, "bad")),
    ]))
    .storage(Storage::new(ReadOnly(InMemoryCheckpointer::new())).durability(Durability::Exit))
    .build()
    .unwrap();
    let chat = agent.thread("t");

    let error = chat.send("hello").await.unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert!(error.run_error.is_none());
    assert!(chat.head().await.unwrap().is_none());

    let error = chat.send("again").await.unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert_eq!(
        error.details.as_ref().unwrap()["run_error"]["code"],
        "model_failed"
    );
    assert!(error.snapshot().is_some(), "the run's snapshot is kept");
}

#[tokio::test(start_paused = true)]
async fn an_error_before_the_run_starts_saves_nothing() {
    let storage = Storage::in_memory();
    let agent = Agent::builder(instant(vec![FakeReply::text("hi")]))
        .storage(storage.clone())
        .build()
        .unwrap();
    // A thread whose stored transcript is not a list of messages.
    let broken = Checkpoint::new(
        "t",
        1,
        Source::Manual,
        agentbuild_core::State::new().with("messages", "oops"),
    );
    storage.checkpointer().commit(broken, 0).await.unwrap();
    let error = agent.thread("t").send("hello").await.unwrap_err();
    assert_eq!(error.code, "invalid_state");
    assert_eq!(agent.thread("t").head().await.unwrap().unwrap().version, 1);
}

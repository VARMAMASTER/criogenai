//! Saving a run while it executes (design 0008 §5.1, durability `step`),
//! with the simulated LLM on Tokio's paused clock.

#[path = "support/fake_llm.rs"]
mod fake_llm;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogen_core::{
    BoxFuture, END, NodeError, NodeOutput, PendingWrite, Persister, RunOptions, Snapshot, State,
    Update, Workflow, WorkflowBuilder, node_with_context,
};
use fake_llm::FakeLlm;
use serde_json::json;

/// What a persister was asked to save, in order.
#[derive(Debug, Clone, PartialEq)]
enum Saved {
    Checkpoint(Snapshot),
    Task(usize, String, PendingWrite),
}

/// Records every save; can fail checkpoints from a given step, or task
/// writes.
#[derive(Default)]
struct Recorder {
    saved: Mutex<Vec<Saved>>,
    fail_checkpoint_at: Option<usize>,
    fail_tasks: bool,
}

impl Recorder {
    fn saved(&self) -> Vec<Saved> {
        self.saved.lock().unwrap().clone()
    }

    /// The last checkpoint with the task writes made after it merged in:
    /// what storage would hand back after a crash.
    fn recovered(&self) -> Snapshot {
        let saved = self.saved();
        let at = saved
            .iter()
            .rposition(|s| matches!(s, Saved::Checkpoint(_)))
            .expect("a checkpoint was saved");
        let Saved::Checkpoint(mut snapshot) = saved[at].clone() else {
            unreachable!()
        };
        for item in &saved[at + 1..] {
            let Saved::Task(step, node, write) = item else {
                unreachable!()
            };
            assert_eq!(*step, snapshot.step);
            let entry = snapshot.pending_writes.entry(node.clone()).or_default();
            entry.memo.extend(write.memo.clone());
            if write.output.is_some() {
                entry.output = write.output.clone();
            }
        }
        snapshot
    }
}

impl Persister for Recorder {
    fn save_checkpoint(&self, snapshot: Snapshot) -> BoxFuture<'static, Result<(), NodeError>> {
        let fail = self.fail_checkpoint_at.is_some_and(|s| snapshot.step >= s);
        if !fail {
            self.saved.lock().unwrap().push(Saved::Checkpoint(snapshot));
        }
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if fail {
                Err(NodeError::new("disk full").with_kind("storage_unavailable"))
            } else {
                Ok(())
            }
        })
    }

    fn save_task(
        &self,
        step: usize,
        node: String,
        write: PendingWrite,
    ) -> BoxFuture<'static, Result<(), NodeError>> {
        let fail = self.fail_tasks;
        if !fail {
            self.saved
                .lock()
                .unwrap()
                .push(Saved::Task(step, node, write));
        }
        Box::pin(async move {
            if fail {
                Err(NodeError::new("disk full").with_kind("storage_unavailable"))
            } else {
                Ok(())
            }
        })
    }
}

#[derive(Clone, Default)]
struct Counter(Arc<AtomicUsize>);

impl Counter {
    fn hit(&self) -> usize {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// `a` asks the LLM; `b` sends an email (counted, saved with `memo.once`)
/// and then asks the LLM; `c` asks the LLM.
fn workflow(llm: &Arc<FakeLlm>, emails: &Counter) -> Workflow {
    let ask = |key: &'static str| {
        let llm = llm.clone();
        node_with_context(move |_ctx, _state| {
            let llm = llm.clone();
            async move { Ok(Update::new().set(key, llm.complete(key).await?).into()) }
        })
    };
    let send = {
        let (llm, emails) = (llm.clone(), emails.clone());
        node_with_context(move |ctx, _state| {
            let (llm, emails) = (llm.clone(), emails.clone());
            async move {
                let receipt: String = ctx
                    .memo()
                    .once("email", || async { Ok(format!("mail-{}", emails.hit())) })
                    .await?;
                let reply = llm.complete("b").await?;
                Ok(NodeOutput::from(
                    Update::new().set("receipt", receipt).set("b", reply),
                ))
            }
        })
    };
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", ask("a"))
        .add_node("b", send)
        .add_node("c", ask("c"))
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("b", "c")
        .add_edge("c", END);
    builder.build().unwrap()
}

fn steps_and_frontiers(saved: &[Saved]) -> Vec<(usize, Vec<String>)> {
    saved
        .iter()
        .filter_map(|s| match s {
            Saved::Checkpoint(snapshot) => Some((snapshot.step, snapshot.frontier.clone())),
            Saved::Task(..) => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn every_step_and_task_is_saved_before_the_run_goes_on() {
    let llm = Arc::new(FakeLlm::new());
    let emails = Counter::default();
    let recorder = Arc::new(Recorder::default());
    let output = workflow(&llm, &emails)
        .run_with(
            State::new(),
            RunOptions::default().persist(recorder.clone()),
        )
        .await
        .unwrap();
    assert_eq!(output.state.get("receipt"), Some(&json!("mail-1")));

    let saved = recorder.saved();
    // A checkpoint before each step; none after the last (the caller saves
    // the outcome).
    assert_eq!(
        steps_and_frontiers(&saved),
        [
            (0, vec!["a".to_string()]),
            (1, vec!["b".to_string()]),
            (2, vec!["c".to_string()]),
        ]
    );
    let Saved::Checkpoint(second) = &saved[2] else {
        panic!("{saved:?}")
    };
    assert_eq!(second.state.get("a"), output.state.get("a"));
    assert!(second.suspended.is_empty() && second.error.is_none());
    assert!(second.pending_writes.is_empty());

    // Within step 1: the memo entry is saved before the output.
    let tasks: Vec<(usize, &str, bool)> = saved
        .iter()
        .filter_map(|s| match s {
            Saved::Task(step, node, write) => Some((*step, node.as_str(), write.output.is_some())),
            Saved::Checkpoint(_) => None,
        })
        .collect();
    assert_eq!(
        tasks,
        [
            (0, "a", true),
            (1, "b", false),
            (1, "b", true),
            (2, "c", true)
        ]
    );
    let Saved::Task(_, _, memo) = &saved[3] else {
        panic!("{saved:?}")
    };
    assert_eq!(memo.memo.get("email"), Some(&json!("mail-1")));
}

#[tokio::test(start_paused = true)]
async fn a_crashed_run_continues_from_storage_without_repeating_saved_work() {
    let llm = Arc::new(FakeLlm::new());
    let emails = Counter::default();
    let recorder = Arc::new(Recorder::default());
    let workflow = workflow(&llm, &emails);
    let run = {
        let (workflow, recorder) = (workflow.clone(), recorder.clone());
        tokio::spawn(async move {
            workflow
                .run_with(State::new(), RunOptions::default().persist(recorder))
                .await
        })
    };
    // `a` takes 2–3 s; `b` has sent its email and waits for the LLM.
    tokio::time::sleep(llm.delay_for("a") + Duration::from_millis(100)).await;
    assert_eq!(emails.get(), 1);
    run.abort(); // the process dies
    assert!(run.await.unwrap_err().is_cancelled());

    let snapshot = recorder.recovered();
    assert_eq!(
        (snapshot.step, snapshot.frontier.clone()),
        (1, vec!["b".into()])
    );
    let output = workflow.retry(&snapshot).await.unwrap();
    assert_eq!(emails.get(), 1, "the email was not sent again");
    assert_eq!(output.state.get("receipt"), Some(&json!("mail-1")));
    assert!(output.state.get("c").is_some());
}

#[tokio::test(start_paused = true)]
async fn a_failed_checkpoint_stops_the_run_with_its_progress() {
    let llm = Arc::new(FakeLlm::new());
    let emails = Counter::default();
    let recorder = Arc::new(Recorder {
        fail_checkpoint_at: Some(1),
        ..Recorder::default()
    });
    let workflow = workflow(&llm, &emails);
    let err = workflow
        .run_with(State::new(), RunOptions::default().persist(recorder))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "storage_failed");
    assert!(err.to_string().contains("disk full"), "{err}");
    assert_eq!(emails.get(), 0, "b never started");
    // The step that committed is in the error's state and snapshot.
    let snapshot = err.snapshot().unwrap();
    assert_eq!(
        (snapshot.step, snapshot.frontier.clone()),
        (1, vec!["b".into()])
    );
    assert!(err.state.get("a").is_some());
    assert_eq!(snapshot.error.as_ref().unwrap().code, "storage_failed");

    // Once storage works again, the run continues.
    let output = workflow.retry(snapshot).await.unwrap();
    assert!(output.state.get("c").is_some());
}

#[tokio::test(start_paused = true)]
async fn a_failed_task_write_keeps_the_finished_work_in_memory() {
    let llm = Arc::new(FakeLlm::new());
    let emails = Counter::default();
    let recorder = Arc::new(Recorder {
        fail_tasks: true,
        ..Recorder::default()
    });
    let workflow = workflow(&llm, &emails);
    let err = workflow
        .run_with(State::new(), RunOptions::default().persist(recorder))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "storage_failed");
    // `a` finished, but its output could not be saved: the snapshot keeps
    // it, so a retry does not ask the LLM again.
    let snapshot = err.snapshot().unwrap();
    assert_eq!(snapshot.step, 0);
    assert!(snapshot.pending_writes["a"].output.is_some());
    let calls = llm.calls();
    workflow.retry(snapshot).await.unwrap();
    assert_eq!(llm.calls(), calls + 2, "only b and c asked the LLM");
}

#[tokio::test(start_paused = true)]
async fn a_failed_memo_save_fails_the_task() {
    let llm = Arc::new(FakeLlm::new());
    let emails = Counter::default();
    let recorder = Arc::new(Recorder {
        fail_tasks: true,
        ..Recorder::default()
    });
    // Start at `b` (its memo save is the first task write).
    let mut snapshot = {
        let healthy = Arc::new(Recorder::default());
        let workflow = workflow(&llm, &emails);
        let run = {
            let healthy = healthy.clone();
            tokio::spawn(async move {
                workflow
                    .run_with(State::new(), RunOptions::default().persist(healthy))
                    .await
            })
        };
        tokio::time::sleep(llm.delay_for("a") + Duration::from_millis(100)).await;
        run.abort();
        let _ = run.await;
        healthy.recovered()
    };
    snapshot.pending_writes.clear();
    assert_eq!(snapshot.frontier, ["b"]);
    let err = workflow(&llm, &emails)
        .retry_with(&snapshot, RunOptions::default().persist(recorder))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "node_failed");
    let source = match &err.kind {
        criogen_core::RunErrorKind::NodeFailed { source, .. } => source,
        other => panic!("{other:?}"),
    };
    assert_eq!(source.kind.as_deref(), Some("storage_failed"));
    // The email went out once; the memo entry is in the snapshot.
    let memo = &err.snapshot().unwrap().pending_writes["b"].memo;
    assert_eq!(
        memo.get("email"),
        Some(&json!(format!("mail-{}", emails.get())))
    );
}

#[tokio::test(start_paused = true)]
async fn resume_values_are_saved_with_the_first_checkpoint() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "ask",
            node_with_context(|ctx, _state| async move {
                Ok(match ctx.resume() {
                    Some(answer) => Update::new().set("answer", answer.clone()).into(),
                    None => NodeOutput::suspend(Update::new(), json!({"question": "ok?"})),
                })
            }),
        )
        .set_entry("ask")
        .add_edge("ask", END);
    let workflow = builder.build().unwrap();
    let suspended = workflow.run(State::new()).await.unwrap();
    let recorder = Arc::new(Recorder::default());
    workflow
        .resume_with(
            suspended.snapshot().unwrap(),
            [("ask", json!("yes"))],
            RunOptions::default().persist(recorder.clone()),
        )
        .await
        .unwrap();
    let saved = recorder.saved();
    let Saved::Checkpoint(first) = &saved[0] else {
        panic!("{saved:?}")
    };
    // No suspension left: a crash now continues with the answer.
    assert!(first.suspended.is_empty());
    assert_eq!(first.pending_writes["ask"].resume, Some(json!("yes")));
    assert_eq!(first.frontier, ["ask"]);
}

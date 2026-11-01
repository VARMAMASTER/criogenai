//! Patterns and specialists (design 0019), with simulated models only.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogen_agent::patterns::{
    self, CEILING, Member, SwarmStart, Tree, loop_, parallel, review_and_critique,
    review_and_critique_with, sequential,
};
use criogen_agent::{
    Agent, PatternError, PatternStop, ReviewDecision, Specialist, SpecialistError, Step,
    specialists,
};
use criogen_core::{END, NodeError, State, Update, WorkflowBuilder, node_fn};
use criogen_providers::{FakeModel, FakeReply, Model};
use criogen_storage::Storage;
use serde_json::json;
use tokio::time::Instant;

fn fake(replies: &[&str]) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(FakeReply::text(*reply));
    }
    model
}

fn agent(replies: &[&str]) -> Agent {
    Agent::builder(fake(replies)).build().unwrap()
}

/// One scripted reply for every call, whoever makes it (each fresh run is turn 0).
fn same_reply(reply: &str) -> Arc<dyn Model> {
    Arc::new(
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
            .by_turn()
            .then(FakeReply::text(reply)),
    )
}

fn steps(result: &criogen_agent::PatternResult) -> Vec<&str> {
    result.trace.iter().map(|t| t.step.as_str()).collect()
}

#[tokio::test(start_paused = true)]
async fn sequential_passes_text_along_and_traces_every_step() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_shout = seen.clone();
    let shout = Step::function(move |text| {
        let seen = seen_by_shout.clone();
        async move {
            seen.lock().unwrap().push(text.clone());
            text.to_uppercase()
        }
    });
    let flow = sequential([
        ("research", Step::from(agent(&["facts about solar"]))),
        ("shout", shout),
        ("write", Step::from(agent(&["A short article."]))),
    ])
    .unwrap();
    let result = flow.run("solar panels").await.unwrap();
    assert_eq!(result.output.as_deref(), Some("A short article."));
    assert_eq!(*seen.lock().unwrap(), vec!["facts about solar"]);
    assert_eq!(steps(&result), ["research", "shout", "write"]);
    assert_eq!(result.stop_reason, PatternStop::Done);
    assert_eq!(result.to_string(), "A short article.");
    assert!(result.explain().contains("1. research"));
    assert_eq!(flow.name(), "sequential");
}

#[tokio::test(start_paused = true)]
async fn steps_can_be_registered_specialists_built_with_a_model() {
    let model = same_reply("same answer");
    let flow = sequential([
        Step::registered("researcher", model.clone()).unwrap(),
        Step::registered("writer", model).unwrap(),
    ])
    .unwrap();
    let result = flow.run("topic").await.unwrap();
    assert_eq!(result.output.as_deref(), Some("same answer"));
    assert_eq!(steps(&result), ["researcher", "writer"]);
}

#[test]
fn bad_steps_are_reported_before_anything_runs() {
    let err = Step::registered("no-such-specialist", same_reply("x")).unwrap_err();
    assert!(err.to_string().contains("registered"), "{err}");
    assert!(matches!(
        err,
        PatternError::Specialist(SpecialistError::Unknown { .. })
    ));
    assert!(matches!(
        sequential(Vec::<Step>::new()),
        Err(PatternError::Invalid(_))
    ));
    assert!(matches!(
        parallel(Vec::<Step>::new(), None),
        Err(PatternError::Invalid(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn parallel_runs_branches_together_and_combines_in_order() {
    let branches = [
        ("sentiment", agent(&["positive"])),
        ("keywords", agent(&["solar, cost"])),
        ("urgency", agent(&["low"])),
    ];
    let synth = Step::from(agent(&["Positive, about solar, low urgency."])).named("summary");
    let flow = parallel(branches, Some(synth)).unwrap();
    let result = flow.run("I love my new solar panels").await.unwrap();
    assert_eq!(
        result.output.as_deref(),
        Some("Positive, about solar, low urgency.")
    );
    let mut names = steps(&result);
    names.sort();
    assert_eq!(
        names,
        ["keywords", "sentiment", "summary (synthesis)", "urgency"]
    );

    let plain = parallel([("a", agent(&["one"])), ("b", agent(&["two"]))], None).unwrap();
    let result = plain.run("x").await.unwrap();
    assert_eq!(result.output.as_deref(), Some("## a\none\n\n## b\ntwo"));
}

#[tokio::test(start_paused = true)]
async fn parallel_branches_really_overlap() {
    // Four branches of 0.4 s each: 0.4 s if they overlap, 1.6 s if they do not.
    let slow: Vec<_> = (0..4)
        .map(|i| {
            let model =
                FakeModel::with_delay(Duration::from_millis(400), Duration::from_millis(400));
            (
                format!("b{i}"),
                Agent::builder(model.then(FakeReply::text("done")))
                    .build()
                    .unwrap(),
            )
        })
        .collect();
    let started = Instant::now();
    parallel(slow, None).unwrap().run("go").await.unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn loop_stops_on_a_condition_or_at_the_cap() {
    let counter = Arc::new(AtomicU32::new(0));
    let step = |counter: &Arc<AtomicU32>| {
        let counter = counter.clone();
        Step::function(move |text| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { format!("{text}+") }
        })
    };

    let done = loop_([step(&counter)], |_text, n| n == 3, 10)
        .unwrap()
        .run("x")
        .await
        .unwrap();
    assert_eq!(done.output.as_deref(), Some("x+++"));
    assert_eq!(done.stop_reason, PatternStop::Condition);
    assert_eq!(counter.load(Ordering::SeqCst), 3);

    counter.store(0, Ordering::SeqCst);
    let capped = loop_([step(&counter)], |_, _| false, 4)
        .unwrap()
        .run("x")
        .await
        .unwrap();
    assert_eq!(capped.stop_reason, PatternStop::MaxIterations);
    assert_eq!(counter.load(Ordering::SeqCst), 4);

    // A hard ceiling, whatever is asked.
    counter.store(0, Ordering::SeqCst);
    loop_([step(&counter)], |_, _| false, 10_000)
        .unwrap()
        .run("x")
        .await
        .unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), CEILING);
    assert!(matches!(
        loop_([step(&counter)], |_, _| true, 0),
        Err(PatternError::Invalid(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn review_and_critique_revises_until_approved() {
    let generator = agent(&["draft one", "draft two", "draft three"]);
    let critic = agent(&["REVISE: add a number", "REVISE: shorter", "APPROVED"]);
    let flow = review_and_critique(generator, critic, 5).unwrap();
    let result = flow.run("write").await.unwrap();
    assert_eq!(result.output.as_deref(), Some("draft three"));
    assert_eq!(result.stop_reason, PatternStop::Approved);
    assert_eq!(&steps(&result)[..2], ["step1 (draft 1)", "step2"]);
    assert_eq!(result.trace.len(), 6);
}

#[tokio::test(start_paused = true)]
async fn review_and_critique_keeps_the_last_draft_when_never_approved() {
    let flow = review_and_critique(
        agent(&["d1", "d2", "d3"]),
        agent(&["REVISE: a", "REVISE: b", "REVISE: c"]),
        3,
    )
    .unwrap();
    let result = flow.run("write").await.unwrap();
    assert_eq!(result.stop_reason, PatternStop::MaxRounds);
    assert_eq!(result.output.as_deref(), Some("d3"));
}

#[tokio::test(start_paused = true)]
async fn review_feedback_reaches_the_generator() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let generator = Step::function(move |text| {
        let log = log.clone();
        async move {
            let mut log = log.lock().unwrap();
            log.push(text);
            format!("draft {}", log.len())
        }
    });
    review_and_critique(generator, agent(&["REVISE: add a citation", "APPROVED"]), 3)
        .unwrap()
        .run("write")
        .await
        .unwrap();
    {
        let seen = seen.lock().unwrap();
        assert!(
            seen[1].contains("add a citation") && seen[1].contains("draft 1"),
            "{}",
            seen[1]
        );
    }

    // The approval test can be your own.
    let own = review_and_critique_with(agent(&["a"]), agent(&["fine"]), 3, |reply| reply == "fine")
        .unwrap()
        .run("x")
        .await
        .unwrap();
    assert_eq!(own.stop_reason, PatternStop::Approved);
}

#[tokio::test(start_paused = true)]
async fn iterative_refinement_stops_at_the_threshold() {
    let refine = Step::function(|text| async move { format!("{text}!") });
    let flow = patterns::iterative_refinement(refine, |t| t.len() as f64 / 5.0, 1.0, 9).unwrap();
    let result = flow.run("ab").await.unwrap();
    assert_eq!(result.output.as_deref(), Some("ab!!!"));
    assert_eq!(result.stop_reason, PatternStop::Condition);
}

fn echo(text: &'static str) -> Step {
    Step::function(move |_| async move { text.to_string() })
}

#[tokio::test(start_paused = true)]
async fn swarm_hands_off_until_final_and_is_bounded() {
    let team = [
        ("market", echo("Demand is high.\nHANDOFF: engineering")),
        (
            "engineering",
            echo("Feasible in 3 months.\nHANDOFF: finance"),
        ),
        ("finance", echo("Budget fits.\nFINAL")),
    ];
    let result = patterns::swarm(team, SwarmStart::Agent("market".into()), 6)
        .unwrap()
        .run("Should we build it?")
        .await
        .unwrap();
    assert_eq!(result.stop_reason, PatternStop::Final);
    assert_eq!(steps(&result), ["market", "engineering", "finance"]);

    let ping = [("a", echo("x\nHANDOFF: b")), ("b", echo("y\nHANDOFF: a"))];
    let endless = patterns::swarm(ping, SwarmStart::First, 3)
        .unwrap()
        .run("go")
        .await
        .unwrap();
    assert_eq!(endless.stop_reason, PatternStop::MaxHandoffs);
    assert!(endless.trace.len() <= 5);

    // Model output is data: an unknown name ends the run, a hand-off to oneself is fine.
    let stray = patterns::swarm(
        [("a", echo("x\nHANDOFF: nobody")), ("b", echo("y"))],
        SwarmStart::First,
        6,
    )
    .unwrap()
    .run("go")
    .await
    .unwrap();
    assert_eq!(stray.stop_reason, PatternStop::UnknownHandoff);
    let selfish = patterns::swarm(
        [("a", echo("x\nhandoff: a")), ("b", echo("y"))],
        SwarmStart::First,
        2,
    )
    .unwrap()
    .run("go")
    .await
    .unwrap();
    assert_eq!(selfish.stop_reason, PatternStop::MaxHandoffs);

    assert!(patterns::swarm([("only", echo("x"))], SwarmStart::First, 3).is_err());
    assert!(
        patterns::swarm(
            [("begin", echo("x")), ("b", echo("y"))],
            SwarmStart::First,
            3
        )
        .is_err()
    );
    assert!(
        patterns::swarm(
            [("a", echo("x")), ("b", echo("y"))],
            SwarmStart::Agent("z".into()),
            3
        )
        .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn swarm_with_a_dispatcher_chooses_the_first_agent() {
    let team = [
        ("a", echo("a did it\nFINAL")),
        ("b", echo("b did it\nFINAL")),
    ];
    let flow = patterns::swarm(team, SwarmStart::Dispatcher(agent(&["b"]).into()), 6).unwrap();
    let result = flow.run("go").await.unwrap();
    assert_eq!(result.trace[0].step, "dispatcher");
    assert_eq!(result.trace[1].step, "b");
    assert!(result.output.unwrap().starts_with("b did it"));
}

#[tokio::test(start_paused = true)]
async fn human_review_pauses_and_resumes_with_a_decision() {
    let flow = patterns::human_review(agent(&["Release 120 rows."])).unwrap();
    let paused = flow.run("Release the dataset?").await.unwrap();
    assert!(paused.suspended);
    assert_eq!(paused.stop_reason, PatternStop::Suspended);
    assert_eq!(
        paused.waiting_for.as_ref().unwrap()["draft"],
        "Release 120 rows."
    );
    let done = flow.resume(&paused, ReviewDecision::Approve).await.unwrap();
    assert_eq!(done.output.as_deref(), Some("Release 120 rows."));

    let flow = patterns::human_review(agent(&["draft"])).unwrap();
    let paused = flow.run("again").await.unwrap();
    let edited = flow
        .resume(&paused, ReviewDecision::Edit("Release 100 rows.".into()))
        .await
        .unwrap();
    assert_eq!(edited.output.as_deref(), Some("Release 100 rows."));
    assert_eq!(edited.stop_reason, PatternStop::Approved);

    let flow = patterns::human_review(agent(&["draft"])).unwrap();
    let paused = flow.run("third").await.unwrap();
    let rejected = flow.resume(&paused, ReviewDecision::Reject).await.unwrap();
    assert_eq!(rejected.stop_reason, PatternStop::Rejected);
    assert_eq!(rejected.output.as_deref(), Some(""));
    assert!(matches!(
        flow.resume(&rejected, ReviewDecision::Approve).await,
        Err(PatternError::NotSuspended)
    ));

    // A malformed decision fails the run and leaves the pause usable.
    let flow = patterns::human_review(agent(&["draft"])).unwrap();
    let paused = flow.run("x").await.unwrap();
    let err = flow
        .resume(&paused, json!({"decision": "edit"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("decision must be"), "{err}");
    assert!(flow.resume(&paused, "approve").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn human_review_is_durable_across_pattern_instances() {
    let storage = Storage::in_memory();
    let first = patterns::human_review(agent(&["draft"])).unwrap();
    let turn = first
        .thread(&storage, "t-1")
        .run(patterns::Pattern::input("go"))
        .await
        .unwrap();
    assert!(turn.is_suspended());
    assert_eq!(turn.interrupts()[0].node, "review");

    // Another process: a new instance of the same pattern, the same storage.
    let other = patterns::human_review(agent(&["never used"])).unwrap();
    let done = other
        .thread(&storage, "t-1")
        .resume([("review", json!("approve"))])
        .await
        .unwrap();
    assert_eq!(done.state().get("output").unwrap(), "draft");
}

#[tokio::test(start_paused = true)]
async fn a_pattern_can_be_extended_with_more_nodes() {
    let flow = sequential([agent(&["x"])]).unwrap();
    let flow = flow
        .extend(|builder| {
            builder
                .add_node(
                    "stamp",
                    node_fn(|state| async move {
                        let current = state.get("current").and_then(|v| v.as_str()).unwrap_or("");
                        Ok(Update::new().set("current", format!("{current} [stamped]")))
                    }),
                )
                .add_edge("finish", "stamp")
                .add_edge("stamp", END);
        })
        .unwrap();
    let result = flow.run("go").await.unwrap();
    assert_eq!(result.state.get("current").unwrap(), "x [stamped]");

    // An extension that does not make a valid graph is refused.
    let broken = sequential([agent(&["x"])]).unwrap().extend(|builder| {
        builder.add_edge("finish", "nowhere");
    });
    assert!(matches!(broken, Err(PatternError::Build(_))));
}

#[tokio::test(start_paused = true)]
async fn custom_wraps_any_graph() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "only",
            node_fn(|state| async move {
                let input = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
                Ok(Update::new().set("output", input.to_uppercase()))
            }),
        )
        .set_entry("only")
        .add_edge("only", END);
    let flow = patterns::custom(builder, 8).unwrap();
    assert_eq!(
        flow.run("hello").await.unwrap().output.as_deref(),
        Some("HELLO")
    );
}

#[tokio::test(start_paused = true)]
async fn a_failing_step_fails_the_run_with_its_error() {
    let flow = sequential([Step::try_function(|_| async {
        Err(NodeError::new("boom").with_kind("my_error"))
    })])
    .unwrap();
    let err = flow.run("x").await.unwrap_err();
    assert!(matches!(err, PatternError::Run(_)));
    assert!(err.to_string().contains("boom"), "{err}");
}

// -------------------------------------------------------------- agent patterns

#[tokio::test(start_paused = true)]
async fn react_caps_model_calls() {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for _ in 0..20 {
        model = model.then(FakeReply::tool_call("poke", json!({})));
    }
    let poke = criogen_agent::tool_fn(
        criogen_providers::ToolSpec::new("poke", "Pokes.", json!({"type": "object"})),
        |_, _| async { Ok::<_, criogen_agent::ToolError>("again") },
    );
    let looping = patterns::react(
        Arc::new(model),
        [Arc::new(poke) as Arc<dyn criogen_agent::Tool>],
        3,
    )
    .unwrap();
    let err = looping.run("go").await.unwrap_err();
    assert_eq!(err.code, "limit_exceeded");
    assert!(patterns::react(same_reply("x"), [], 0).is_err());
}

#[tokio::test(start_paused = true)]
async fn coordinator_routes_to_a_specialist_and_nests_as_a_step() {
    let orders = agent(&["Order A-1 shipped."]);
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call(
            "orders",
            json!({"input": "where is A-1"}),
        ))
        .then(FakeReply::text("It shipped."));
    let triage = patterns::coordinator(
        Arc::new(model),
        [(
            "orders",
            Member::described(orders, "order status questions"),
        )],
        None,
    )
    .unwrap();
    // An agent pattern is a step like any other.
    let flow = sequential([("triage", Step::from(triage))]).unwrap();
    let result = flow.run("where is A-1").await.unwrap();
    assert_eq!(result.output.as_deref(), Some("It shipped."));
}

#[tokio::test(start_paused = true)]
async fn coordinator_checks_its_specialists_and_lists_them() {
    let none: [(&str, Member); 0] = [];
    assert!(patterns::coordinator(same_reply("x"), none, None).is_err());
    assert!(
        patterns::coordinator(same_reply("x"), [("x", Member::from("no-such-one"))], None).is_err()
    );

    // The roster reaches the model.
    let model = Arc::new(fake(&["ok"]));
    let boss = patterns::coordinator(
        model.clone(),
        [
            ("orders", Member::described(agent(&["x"]), "order status")),
            ("writer", Member::from("writer")),
        ],
        Some("You triage."),
    )
    .unwrap();
    boss.run("hi").await.unwrap();
    let request = &model.requests()[0];
    let system = request.messages[0].text();
    assert!(system.starts_with("You triage."), "{system}");
    assert!(system.contains("- orders: order status"), "{system}");
    assert!(system.contains("- writer: Writes clear prose"), "{system}");
}

#[tokio::test(start_paused = true)]
async fn hierarchical_builds_coordinators_of_coordinators() {
    // One shared script: root -> research team -> web worker.
    let model: Arc<dyn Model> = Arc::new(
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
            .then(FakeReply::tool_call("research", json!({"input": "q"})))
            .then(FakeReply::tool_call("web", json!({"input": "q"})))
            .then(FakeReply::text("digest"))
            .then(FakeReply::text("done")),
    );
    let tree: Vec<(&str, Tree)> = vec![
        (
            "research",
            Tree::team(
                "gathers facts",
                [("web", Tree::from(agent(&["web facts"])))],
            ),
        ),
        ("writing", Tree::from(agent(&["x"]))),
    ];
    let root = patterns::hierarchical(model, tree, None).unwrap();
    let result = root.run("go").await.unwrap();
    assert_eq!(result.text(), Some("done"));
    assert_eq!(result.tool_calls, 1);
}

// ------------------------------------------------------------------ specialists

#[test]
fn builtin_specialists_are_registered() {
    let names = specialists::available();
    for expected in [
        "researcher",
        "summarizer",
        "classifier",
        "extractor",
        "planner",
        "writer",
        "critic",
        "fact_checker",
        "code_reviewer",
        "translator",
        "support_agent",
    ] {
        assert!(names.iter().any(|n| n == expected), "{expected}");
    }
    assert!(specialists::critic().instructions().contains("APPROVED"));
}

#[tokio::test(start_paused = true)]
async fn a_specialist_builds_an_agent() {
    let built = specialists::researcher()
        .build(Arc::new(fake(&["ok"])))
        .unwrap();
    assert_eq!(built.run("x").await.unwrap().text(), Some("ok"));
    let model = Arc::new(fake(&["ok"]));
    let tweaked = specialists::writer()
        .build_with(model.clone(), |b| b.instructions("Only poems."))
        .unwrap();
    tweaked.run("x").await.unwrap();
    assert_eq!(model.requests()[0].messages[0].text(), "Only poems.");
}

#[test]
fn extend_makes_a_new_recipe_and_leaves_the_original_alone() {
    let original = specialists::researcher().instructions().to_string();
    let careful = specialists::researcher()
        .extend("careful-researcher")
        .append_instructions("Always give two sources.");
    assert_eq!(careful.name(), "careful-researcher");
    assert!(careful.instructions().starts_with(&original));
    assert!(careful.instructions().ends_with("Always give two sources."));
    assert_eq!(specialists::researcher().instructions(), original);

    let search = criogen_agent::tool_fn(
        criogen_providers::ToolSpec::new("search", "Searches.", json!({"type": "object"})),
        |_, _| async { Ok::<_, criogen_agent::ToolError>("result") },
    );
    let with_tools = careful.extend("searcher").tool(search);
    assert_eq!(with_tools.tool_names(), ["search"]);
    assert!(careful.tool_names().is_empty());
    let replaced = specialists::writer()
        .extend("poet")
        .instructions_replaced("Write only poems.");
    assert_eq!(replaced.instructions(), "Write only poems.");
    assert_eq!(specialists::writer().name(), "writer");
}

#[tokio::test(start_paused = true)]
async fn register_get_and_name_rules() {
    let mine = Specialist::new("my-triage", "Triage for my shop", "Be quick.").unwrap();
    specialists::register(mine).unwrap();
    assert_eq!(
        specialists::get("my-triage").unwrap().description(),
        "Triage for my shop"
    );
    assert!(matches!(
        specialists::register(Specialist::new("my-triage", "d", "i").unwrap()),
        Err(SpecialistError::AlreadyRegistered(_))
    ));
    specialists::register_replace(Specialist::new("my-triage", "d2", "i2").unwrap());
    assert_eq!(specialists::get("my-triage").unwrap().description(), "d2");

    // A registered name works as a step.
    let flow = sequential([Step::registered("my-triage", same_reply("done")).unwrap()]).unwrap();
    assert_eq!(flow.run("x").await.unwrap().output.as_deref(), Some("done"));

    assert!(specialists::unregister("my-triage"));
    assert!(specialists::get("my-triage").is_err());
    assert!(matches!(
        Specialist::new("bad name!", "d", "i"),
        Err(SpecialistError::InvalidName(_))
    ));
    assert!(matches!(
        Specialist::new("fine", "  ", "i"),
        Err(SpecialistError::MissingDescription)
    ));
    assert!(Specialist::new("x".repeat(65), "d", "i").is_err());
}

// ------------------------------------------------------------------- one-liners

#[tokio::test(start_paused = true)]
async fn quick_builds_a_pattern_from_one_line() {
    let model = same_reply("ok");
    let seq = patterns::quick("researcher > writer", model.clone()).unwrap();
    assert_eq!(
        steps(&seq.run("x").await.unwrap()),
        ["researcher", "writer"]
    );
    let par = patterns::quick("researcher | critic", model.clone()).unwrap();
    let result = par.run("x").await.unwrap();
    let mut names = steps(&result);
    names.sort();
    assert_eq!(names, ["critic", "researcher"]);
    let review = patterns::quick("writer <-> critic", same_reply("APPROVED")).unwrap();
    assert_eq!(
        review.run("x").await.unwrap().stop_reason,
        PatternStop::Approved
    );
    let single = patterns::quick("summarizer", model).unwrap();
    assert_eq!(single.run("x").await.unwrap().output.as_deref(), Some("ok"));
}

#[test]
fn quick_explains_what_it_cannot_read() {
    let m = same_reply("ok");
    let message = |spec: &str| patterns::quick(spec, m.clone()).unwrap_err().to_string();
    assert!(message("researcher > writer | critic").contains("one kind of connector"));
    assert!(message("researcher >").contains("empty"));
    assert!(message("researcher > nobody").contains("registered"));
    assert!(message("writer <-> critic <-> researcher").contains("exactly two"));
    assert!(message("  ").contains("quick needs"));
    // Odd characters do not break the parser.
    assert!(patterns::quick("é > ü", m.clone()).is_err());
}

#[test]
fn patterns_have_debug_and_stop_reasons_have_names() {
    let flow = sequential([agent(&["x"])]).unwrap();
    assert!(format!("{flow:?}").contains("sequential"));
    assert_eq!(PatternStop::MaxHandoffs.as_str(), "max_handoffs");
    assert_eq!(PatternStop::UnknownHandoff.to_string(), "unknown_handoff");
    assert_eq!(
        serde_json::to_value(PatternStop::MaxRounds).unwrap(),
        "max_rounds"
    );
    assert_eq!(State::new().get("x"), None);
}

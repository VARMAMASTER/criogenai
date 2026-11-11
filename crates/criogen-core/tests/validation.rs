//! Graph validation rules enforced by `WorkflowBuilder::build`.

use criogen_core::{
    END, Node, NodeError, Update, ValidationIssue, WorkflowBuilder, node_fn, router_fn,
};

fn noop() -> impl Node {
    node_fn(|_| async { Ok(Update::new()) })
}

fn issues(b: &WorkflowBuilder) -> Vec<ValidationIssue> {
    match b.build() {
        Ok(_) => Vec::new(),
        Err(e) => e.issues,
    }
}

fn unknown(node: &str, context: &str) -> ValidationIssue {
    ValidationIssue::UnknownNode {
        node: node.into(),
        context: context.into(),
    }
}

#[test]
fn empty_workflow() {
    assert_eq!(
        issues(&WorkflowBuilder::new()),
        vec![ValidationIssue::NoNodes]
    );
}

#[test]
fn missing_entry() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop()).add_edge("a", END);
    assert_eq!(issues(&b), vec![ValidationIssue::MissingEntry]);
    // The message names the method in every language, not only Rust's.
    let message = ValidationIssue::MissingEntry.to_string();
    assert!(
        message.contains("`start`") && message.contains("`set_entry`"),
        "{message}"
    );
}

#[test]
fn bad_names() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("a", noop())
        .add_node("", noop())
        .add_node("__end__", noop())
        .set_entry("a")
        .add_edge("a", END);
    let found = issues(&b);
    assert!(found.contains(&ValidationIssue::DuplicateNode("a".into())));
    assert!(found.contains(&ValidationIssue::EmptyName));
    assert!(found.contains(&ValidationIssue::ReservedName("__end__".into())));
}

#[test]
fn unknown_references_are_all_reported() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .set_entry("ghost")
        .add_edge("a", "typo")
        .add_router(
            "a",
            router_fn(|_| async { Ok::<_, NodeError>("x") }),
            [("x", "missing")],
        )
        .add_join(["nope"], "a");
    let found = issues(&b);
    assert!(found.contains(&unknown("ghost", "entry")));
    assert!(found.contains(&unknown("typo", "edge `a` -> `typo`")));
    assert!(found.contains(&unknown("missing", "route `x` of router on `a`")));
    assert!(found.contains(&unknown("nope", "join into `a`")));
}

#[test]
fn end_is_not_a_valid_edge_source() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .set_entry("a")
        .add_edge("a", END)
        .add_edge(END, "a");
    assert_eq!(issues(&b), vec![unknown(END, "edge `__end__` -> `a`")]);
}

#[test]
fn dead_end_and_unreachable() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("stuck", noop())
        .add_node("island", noop())
        .set_entry("a")
        .add_edge("a", "stuck")
        .add_edge("island", END);
    assert_eq!(
        issues(&b),
        vec![
            ValidationIssue::DeadEnd("stuck".into()),
            ValidationIssue::Unreachable("island".into())
        ]
    );
}

#[test]
fn router_rules() {
    let r = || router_fn(|_| async { Ok::<_, NodeError>("x") });
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .set_entry("a")
        .add_edge("a", "b")
        .add_router("a", r(), [("x", END), ("x", "b")])
        .add_router("a", r(), [("y", END)])
        .add_router("b", r(), Vec::<(&str, &str)>::new());
    let found = issues(&b);
    assert!(found.contains(&ValidationIssue::DuplicateRouteLabel {
        node: "a".into(),
        label: "x".into()
    }));
    assert!(found.contains(&ValidationIssue::MultipleRouters("a".into())));
    assert!(found.contains(&ValidationIssue::MixedEdges("a".into())));
    assert!(found.contains(&ValidationIssue::EmptyRoutes("b".into())));
}

#[test]
fn join_target_must_only_be_reached_by_its_join() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .add_node("j", noop())
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("a", "j")
        .add_join(["a", "b"], "j")
        .add_edge("j", END);
    assert_eq!(
        issues(&b),
        vec![ValidationIssue::JoinTargetHasOtherInbound("j".into())]
    );
}

#[test]
fn join_source_needs_no_other_outgoing_edge() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .add_node("j", noop())
        .set_entry("a")
        .add_edge("a", "b")
        .add_join(["a", "b"], "j")
        .add_edge("j", END);
    assert!(b.build().is_ok());
}

#[test]
fn build_error_lists_every_issue() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .set_entry("a")
        .add_edge("a", "x")
        .add_edge("a", "y");
    let msg = b.build().err().unwrap().to_string();
    assert_eq!(
        msg,
        "invalid workflow (2 issue(s))\n  - edge `a` -> `x` refers to unknown node `x`\n  - edge `a` -> `y` refers to unknown node `y`"
    );
}

#[test]
fn builder_can_be_extended_after_a_failed_build() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop()).set_entry("a");
    assert!(b.build().is_err());
    b.add_edge("a", END);
    assert!(b.build().is_ok());
}

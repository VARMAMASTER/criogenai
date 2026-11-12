type Error = Box<dyn std::error::Error>;

pub async fn workflows() -> Result<(), Error> {
    // [show]
    use criogenai_core::{
        END, NodeOutput, State, Update, WorkflowBuilder, node_fn, node_with_context, router_fn,
    };
    use criogenai_storage::Storage;
    use serde_json::json;

    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "classify",
            node_fn(|state| async move {
                let text = state.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let category = if text.contains("charge") {
                    "billing"
                } else {
                    "other"
                };
                Ok(Update::new().set("category", category))
            }),
        )
        .add_node(
            "approve",
            node_with_context(|ctx, state| async move {
                // Pause for a person; continue when someone answers.
                Ok(match ctx.resume() {
                    None => {
                        NodeOutput::suspend(Update::new(), json!({"refund": state.get("amount")}))
                    }
                    Some(answer) => Update::new().set("approved", answer == "yes").into(),
                })
            }),
        )
        .add_node(
            "answer",
            node_fn(|_| async { Ok(Update::new().set("reply", "A human will follow up.")) }),
        )
        .set_entry("classify")
        .add_router(
            "classify",
            router_fn(|state| async move {
                Ok(state
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("other")
                    .to_string())
            }),
            [("billing", "approve"), ("other", "answer")],
        )
        .add_edge("approve", END)
        .add_edge("answer", END);
    let workflow = builder.build()?; // every graph problem, reported at once

    // Durable: the pause waits in the database, for days if needed.
    let storage = Storage::connect("sqlite://app.db").await?;
    let input = State::new()
        .with("text", "I was charged twice")
        .with("amount", 40);
    let turn = storage
        .workflow_thread(&workflow, "ticket-7")
        .run(input)
        .await?;
    if turn.is_suspended() {
        // Later, from any process:
        storage
            .workflow_thread(&workflow, "ticket-7")
            .actor("user:boss")
            .resume([("approve", json!("yes"))])
            .await?;
    }
    // [/show]
    Ok(())
}

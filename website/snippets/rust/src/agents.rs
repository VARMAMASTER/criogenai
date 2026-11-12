type Error = Box<dyn std::error::Error>;
use criogenai_providers::{Endpoint, ProviderModel};

fn model() -> Result<ProviderModel, Error> {
    Ok(ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?)
}

pub async fn tools() -> Result<(), Error> {
    // [show]
    use criogenai_agent::{Agent, Limits, ToolError, tool_fn};
    use criogenai_providers::ToolSpec;
    use serde_json::json;

    let refund = tool_fn(
        ToolSpec::new(
            "issue_refund",
            "Refund an order.",
            json!({
                "type": "object",
                "properties": {
                    "order_id": {"type": "string"},
                    "amount": {"type": "number", "minimum": 0, "maximum": 500}
                },
                "required": ["order_id", "amount"]
            }),
        ),
        |args, _ctx| async move {
            // Arguments arrive already checked against the schema.
            if args["amount"].as_f64() > Some(200.0) {
                // The model sees this and can explain or try another way.
                return Err(ToolError::new("Refunds over 200 need a manager"));
            }
            Ok(json!({"refunded": args["amount"]}))
        },
    );

    let agent = Agent::builder(model()?)
        .instructions("You are a support agent.")
        .tool(refund)
        .limits(Limits::default().model_calls(10).tool_calls(20))
        .build()?;
    // [/show]
    Ok(())
}

pub async fn structured() -> Result<(), Error> {
    // [show structured]
    use criogenai_agent::Agent;
    use serde_json::json;

    let agent = Agent::builder(model()?)
        .output(json!({
            "type": "object",
            "properties": {
                "category": {"enum": ["billing", "shipping", "other"]},
                "refund": {"type": "boolean"}
            },
            "required": ["category", "refund"]
        }))
        .build()?;

    let result = agent.run("I was charged twice for order A-1001").await?;
    let output = result.output; // Some({"category": "billing", "refund": true})
    // [/show structured]
    Ok(())
}

pub async fn testing() -> Result<(), Error> {
    // [show testing]
    use criogenai_agent::Agent;
    use criogenai_providers::{FakeModel, FakeReply};
    use serde_json::json;

    // Scripted replies: no network, no cost, the same answer every time.
    let model = FakeModel::new()
        .then(FakeReply::tool_call(
            "get_order",
            json!({"order_id": "A-1001"}),
        ))
        .then(FakeReply::text("It ships tomorrow."));
    let agent = Agent::builder(model).build()?; // add your tools as usual
    // [/show testing]
    Ok(())
}

pub async fn subagents() -> Result<(), Error> {
    // [show subagents]
    use criogenai_agent::Agent;

    let researcher = Agent::builder(model()?)
        .instructions("Find facts about orders. Be precise.")
        .build()?;
    let support = Agent::builder(model()?)
        .instructions("You are a support agent.")
        // The researcher becomes a tool: it takes {"input": text} and answers.
        .tool(researcher.as_tool("research", "Looks up facts about an order"))
        .build()?;
    // [/show subagents]
    let _ = support;
    Ok(())
}

pub async fn hooks() -> Result<(), Error> {
    // [show hooks]
    use criogenai_agent::{
        Agent, Hook, HookAction, HookContext, HookError, ToolCallRequest, ToolError, ToolNext,
        ToolOutput,
    };
    use criogenai_providers::BoxFuture;

    /// Blocks large refunds and stops long runs.
    struct Guard;

    impl Hook for Guard {
        fn name(&self) -> &str {
            "guard"
        }

        fn before_model<'a>(
            &'a self,
            ctx: &'a HookContext,
        ) -> BoxFuture<'a, Result<HookAction, HookError>> {
            Box::pin(async move {
                if ctx.model_calls >= 10 {
                    let output = "Stopped after 10 model calls.".into();
                    return Ok(HookAction::proceed().end(Some(output)));
                }
                Ok(HookAction::proceed())
            })
        }

        // Rust has one method around each tool call (and `wrap_model` around
        // each model call); `next.call` runs the tool.
        fn wrap_tool<'a>(
            &'a self,
            call: ToolCallRequest,
            next: ToolNext<'a>,
        ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            Box::pin(async move {
                let amount = call.args.get("amount").and_then(|a| a.as_f64());
                if call.name == "issue_refund" && amount > Some(500.0) {
                    return Err(ToolError::new("Refunds over 500 need a manager."));
                }
                next.call(call).await
            })
        }
    }

    let support = Agent::builder(model()?).hook(Guard).build()?;
    // [/show hooks]
    let _ = support;
    Ok(())
}

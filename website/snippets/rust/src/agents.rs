type Error = Box<dyn std::error::Error>;
use agentbuild_providers::{Endpoint, ProviderModel};

fn model() -> Result<ProviderModel, Error> {
    Ok(ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?)
}

pub async fn tools() -> Result<(), Error> {
    // [show]
    use agentbuild_agent::{Agent, Limits, ToolError, tool_fn};
    use agentbuild_providers::ToolSpec;
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
    use agentbuild_agent::Agent;
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
    use agentbuild_agent::Agent;
    use agentbuild_providers::{FakeModel, FakeReply};
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

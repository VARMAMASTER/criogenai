type Error = Box<dyn std::error::Error>;

pub async fn quickstart() -> Result<(), Error> {
    // [show]
    use criogen_agent::{Agent, ToolError, tool_fn};
    use criogen_providers::{Endpoint, ProviderModel, ToolSpec};
    use serde_json::json;

    let model = ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?;

    let get_order = tool_fn(
        ToolSpec::new(
            "get_order",
            "Look up an order by id.",
            json!({
                "type": "object",
                "properties": {"order_id": {"type": "string"}},
                "required": ["order_id"]
            }),
        ),
        |args, _ctx| async move {
            match args["order_id"].as_str() {
                Some("A-1001") => Ok(json!({"status": "shipped", "eta": "tomorrow"})),
                other => Err(ToolError::new(format!("No order {other:?}"))),
            }
        },
    );

    let agent = Agent::builder(model)
        .instructions("You are a support agent. Be brief.")
        .tool(get_order)
        .build()?;

    let result = agent.run("Where is order A-1001?").await?;
    println!("{}", result.text().unwrap_or_default());
    // [/show]
    Ok(())
}

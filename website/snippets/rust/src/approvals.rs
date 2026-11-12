type Error = Box<dyn std::error::Error>;
use criogenai_providers::{Endpoint, ProviderModel};

pub async fn approvals() -> Result<(), Error> {
    let model = ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?;
    let refund = criogenai_agent::tool_fn(
        criogenai_providers::ToolSpec::new(
            "issue_refund",
            "Refund an order.",
            serde_json::json!({"type": "object"}),
        ),
        |_args, _ctx| async { Ok::<_, criogenai_agent::ToolError>("refunded") },
    );
    // [show]
    use criogenai_agent::{Agent, Approval};
    use criogenai_storage::Storage;

    let agent = Agent::builder(model)
        .tool(refund)
        .approval(Approval::new().tool("issue_refund"))
        .storage(Storage::connect("sqlite://support.db").await?)
        .build()?;

    let chat = agent.thread("ticket-7").actor("user:ada");
    let reply = chat
        .send("Order A-1001 arrived broken, please refund it")
        .await?;
    if reply.needs_approval() {
        for request in reply.approvals() {
            println!(
                "{} wants {}",
                request.tool,
                serde_json::Value::Object(request.args)
            );
        }
    }

    // Hours later, in another process: the approval waited in the database.
    let reply = agent
        .thread("ticket-7")
        .actor("user:dr-rao")
        .approve()
        .await?;
    // Or .reject("Refund policy says no"), or one decision per call:
    // .decide([(call_id, Decision::edit(new_args))]) / Decision::respond("Already refunded").
    // [/show]
    Ok(())
}

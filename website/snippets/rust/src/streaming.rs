type Error = Box<dyn std::error::Error>;
use criogenai_agent::Agent;

pub async fn streaming(agent: Agent) -> Result<(), Error> {
    // [show]
    use criogenai_agent::AgentEvent;

    let mut stream = agent.thread("user-42").stream("Summarise my last order");
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Text { text, .. } => print!("{text}"),
            AgentEvent::ToolCallStarted { tool, .. } => println!("\n[{tool}…]"),
            AgentEvent::ApprovalNeeded(requests) => println!("\nwaiting for approval"),
            _ => {}
        }
    }
    let reply = stream.finish().await?; // saved, like `send`
    // [/show]
    Ok(())
}

type Error = Box<dyn std::error::Error>;
use criogen_agent::Agent;

pub async fn threads(agent: Agent) -> Result<(), Error> {
    // [show]
    let chat = agent.thread("user-42");
    chat.send("Hi, I am Ada").await?;
    let reply = chat.send("What is my name?").await?; // remembers the conversation

    let history = chat.history(20).await?; // checkpoints, newest first
    chat.rewind(&history[1].id).await?; // go back one step

    // After a crash mid-run: continue where it stopped.
    // Finished tool calls are not run again.
    chat.retry().await?;

    chat.delete().await?;
    // [/show]
    Ok(())
}

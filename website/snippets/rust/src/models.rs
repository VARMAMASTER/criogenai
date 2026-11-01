type Error = Box<dyn std::error::Error>;

pub async fn models() -> Result<(), Error> {
    let key = std::env::var("OPENAI_API_KEY")?;
    // [show]
    use criogen_agent::Agent;
    use criogen_providers::{Endpoint, ProviderModel};

    let openai = ProviderModel::new(Endpoint::openai(key), "gpt-5-mini")?;
    let claude = ProviderModel::new(
        Endpoint::anthropic(std::env::var("ANTHROPIC_API_KEY")?),
        "claude-sonnet-5-5",
    )?;
    let gemini = ProviderModel::new(
        Endpoint::gemini(std::env::var("GEMINI_API_KEY")?),
        "gemini-2.5-flash",
    )?;
    // Self-hosted: nothing leaves your network.
    let local = ProviderModel::new(Endpoint::ollama(), "llama3.2")?;
    let vllm = ProviderModel::new(Endpoint::vllm("http://gpu-box:8000/v1", None), "qwen3")?;

    // Rate limits, overload and outages go to the next model.
    let agent = Agent::builder(openai).fallback(local).build()?;
    // [/show]
    Ok(())
}

pub async fn direct() -> Result<(), Error> {
    let key = std::env::var("OPENAI_API_KEY")?;
    // [show direct]
    use std::sync::Arc;

    use criogen_agent::ResilientModel;
    use criogen_core::RetryPolicy;
    use criogen_providers::{Endpoint, Message, Model, ModelRequest, ProviderModel};

    let openai = ProviderModel::new(Endpoint::openai(key), "gpt-5-mini")?;
    let local = ProviderModel::new(Endpoint::ollama(), "llama3.2")?;
    // Retries and fallbacks as for an agent.
    let model = ResilientModel::new(Arc::new(openai))
        .fallback(Arc::new(local))
        .retry(RetryPolicy::new().max_attempts(4));
    let request = ModelRequest::new(vec![
        Message::system("Two sentences."),
        Message::user("Summarize: …"),
    ]);
    let response = model.invoke(&request).await?;
    println!("{}", response.message.text());
    // [/show direct]
    Ok(())
}

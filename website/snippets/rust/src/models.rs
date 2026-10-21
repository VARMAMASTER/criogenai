type Error = Box<dyn std::error::Error>;

pub async fn models() -> Result<(), Error> {
    let key = std::env::var("OPENAI_API_KEY")?;
    // [show]
    use agentbuild_agent::Agent;
    use agentbuild_providers::{Endpoint, ProviderModel};

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

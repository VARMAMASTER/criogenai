type Error = Box<dyn std::error::Error>;
use agentbuild_providers::{Endpoint, ProviderModel};

pub async fn memory() -> Result<(), Error> {
    let model = ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?;
    // [show]
    use agentbuild_agent::{Agent, AgentInput, Memory};
    use agentbuild_storage::{Namespace, Storage};

    let storage = Storage::connect("sqlite://agent.db").await?;
    let agent = Agent::builder(model)
        .storage(storage.clone())
        // One memory per user: users/<user_id>. Facts are recalled before
        // each model call; the model gets remember, forget and search tools.
        .memory(Memory::by_state("user_id", Namespace::parse("users")?))
        .build()?;

    let monday = AgentInput::from("I'm vegetarian").with("user_id", "42");
    agent.thread("monday").send(monday).await?;

    // A later conversation recalls it.
    let friday = AgentInput::from("Dinner ideas?").with("user_id", "42");
    agent.thread("friday").send(friday).await?;

    // Delete everything about one user.
    let user = Namespace::parse("users/42")?;
    storage.memory().delete_namespace(&user).await?;
    // [/show]
    Ok(())
}

pub async fn semantic() -> Result<(), Error> {
    let key = std::env::var("OPENAI_API_KEY")?;
    let model = ProviderModel::new(Endpoint::openai(key.clone()), "gpt-5-mini")?;
    let storage = agentbuild_storage::Storage::in_memory();
    // [show semantic]
    use agentbuild_agent::{Agent, Memory, put_embedded};
    use agentbuild_providers::ProviderEmbedder;
    use agentbuild_storage::Namespace;
    use serde_json::json;

    let embedder = ProviderEmbedder::new(Endpoint::openai(key), "text-embedding-3-small")?;
    let memory =
        Memory::by_state("patient", Namespace::parse("patients")?).semantic(embedder.clone());
    let agent = Agent::builder(model)
        .storage(storage.clone())
        .memory(memory)
        .build()?;

    // Facts your app saves itself need a vector too.
    let patient = Namespace::parse("patients/p1")?;
    let fact = json!("Severe penicillin reaction");
    put_embedded(
        storage.memory().as_ref(),
        &embedder,
        &patient,
        "allergy",
        fact,
        None,
    )
    .await?;
    // "Which antibiotics should I avoid?" now finds the allergy.
    // [/show semantic]
    Ok(())
}

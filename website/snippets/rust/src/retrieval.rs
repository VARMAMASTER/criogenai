type Error = Box<dyn std::error::Error>;

pub async fn retrieval() -> Result<(), Error> {
    // [show]
    use std::sync::Arc;

    use criogen_agent::Agent;
    use criogen_agent::retrieval::{Collection, Document};
    use criogen_providers::{Endpoint, ProviderEmbedder, ProviderModel};
    use criogen_storage::Storage;

    let key = std::env::var("OPENAI_API_KEY")?;
    let storage = Storage::connect("sqlite://handbook.db").await?;
    let embedder = Arc::new(ProviderEmbedder::new(
        Endpoint::openai(key.clone()),
        "text-embedding-3-small",
    )?);
    let handbook = Collection::open(storage.memory().clone(), "handbook", embedder).await?;
    handbook
        .add(vec![
            Document::new("leave", "Annual leave is 25 days a year.").meta("section", "hr"),
            Document::new("expenses", "Expenses are paid within 30 days.")
                .meta("section", "finance"),
        ])
        .await?;

    let hits = handbook.search("How many vacation days?", 3).await?;
    println!("{}", hits[0].text);

    let model = ProviderModel::new(Endpoint::openai(key), "gpt-5-mini")?;
    let agent = Agent::builder(model)
        .tool(handbook.as_tool("search_handbook", "Search the staff handbook."))
        .build()?;
    agent.run("How much leave do I get?").await?;
    // [/show]
    Ok(())
}

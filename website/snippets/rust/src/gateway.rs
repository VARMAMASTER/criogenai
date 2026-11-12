type Error = Box<dyn std::error::Error>;

pub async fn gateway() -> Result<(), Error> {
    // [show]
    use std::sync::Arc;

    use criogenai_gateway::Gateway;
    use criogenai_providers::{Endpoint, ProviderModel};

    let openai = ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?;
    let local = ProviderModel::new(Endpoint::ollama(), "llama3.2")?;
    let team_key = std::env::var("GATEWAY_KEY_TEAM_A")?;

    let gateway = Gateway::builder()
        .route_with_fallbacks("support", [Arc::new(openai) as _, Arc::new(local) as _])
        .key("team-a", team_key, ["support"])
        .build()?;

    // An axum Router: serve it, or nest it in your own server.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4000").await?;
    axum::serve(listener, gateway.router()).await?;
    // [/show]
    Ok(())
}

type Error = Box<dyn std::error::Error>;

pub async fn mcp() -> Result<(), Error> {
    // [show]
    use criogen_agent::{Agent, mcp::McpServer};
    use criogen_providers::{Endpoint, ProviderModel};

    // A server this app starts (stdio): any MCP server, in any language.
    let files = McpServer::stdio(
        "npx",
        ["-y", "@modelcontextprotocol/server-filesystem", "./notes"],
    )
    .prefix("files")
    .connect()
    .await?;

    // A server at a URL (Streamable HTTP), with a token in a header.
    let token = std::env::var("DOCS_MCP_TOKEN")?;
    let docs = McpServer::http("https://mcp.example.com/mcp")
        .header("Authorization", format!("Bearer {token}"))
        .prefix("docs")
        .only(["search"])
        .connect()
        .await?;

    let model = ProviderModel::new(
        Endpoint::openai(std::env::var("OPENAI_API_KEY")?),
        "gpt-5-mini",
    )?;
    let agent = Agent::builder(model)
        .tools(files.tools())
        .tools(docs.tools())
        .build()?;
    let result = agent
        .run("Summarize what ./notes says about the Q3 launch.")
        .await?;
    println!("{}", result.text().unwrap_or_default());

    files.close().await;
    // [/show]
    Ok(())
}

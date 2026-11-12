//! MCP servers (design 0022): connected here, named to TypeScript by a
//! handle; agents take their tools by handle (`config.mcp`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use criogenai_agent::mcp::{McpClient, McpError, McpServer};
use criogenai_agent::{AgentBuilder, Tool, ToolOutput};
use serde_json::{Map, Value, json};

use crate::{Failure, Result, bad, failure, lock};

fn clients() -> &'static Mutex<HashMap<u32, McpClient>> {
    static CLIENTS: OnceLock<Mutex<HashMap<u32, McpClient>>> = OnceLock::new();
    CLIENTS.get_or_init(Mutex::default)
}

fn client(handle: u32) -> Result<McpClient> {
    lock(clients())
        .get(&handle)
        .cloned()
        .ok_or_else(|| failure("mcp_closed", "the MCP server was closed", None))
}

fn from_mcp(error: McpError) -> Failure {
    failure(error.code(), error.message, None)
}

/// Starts or reaches a server from its spec (`McpServer::from_spec`):
/// `{handle, info, tools}`.
pub async fn mcp_connect(spec: String) -> Result<String> {
    let spec: Value = serde_json::from_str(&spec).map_err(|e| bad(e.to_string()))?;
    if spec["transport"] == "stdio" && !cfg!(feature = "mcp-stdio") {
        return Err(failure(
            "unsupported",
            "MCP servers started as a process (stdio) need a server; in the browser use mcp.http",
            None,
        ));
    }
    let server = McpServer::from_spec(&spec).map_err(from_mcp)?;
    let client = server.connect().await.map_err(from_mcp)?;
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let handle = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut described = client.describe();
    lock(clients()).insert(handle, client);
    described["handle"] = json!(handle);
    Ok(described.to_string())
}

/// Lists the server's tools again: `{handle, info, tools}`.
pub async fn mcp_refresh(handle: u32) -> Result<String> {
    let client = client(handle)?;
    client.refresh().await.map_err(from_mcp)?;
    let mut described = client.describe();
    described["handle"] = json!(handle);
    Ok(described.to_string())
}

/// Calls a tool: `{"ok": true, "text" | "value" | "content"}` or
/// `{"ok": false, "error"}`.
pub async fn mcp_call(handle: u32, name: String, args: String) -> Result<String> {
    let args: Map<String, Value> = serde_json::from_str(&args).map_err(|e| bad(e.to_string()))?;
    let client = client(handle)?;
    Ok(match client.call(&name, args).await {
        Ok(output) => {
            let mut value = match &output {
                ToolOutput::Text(text) => json!({ "text": text }),
                ToolOutput::Json(value) => json!({ "value": value }),
                ToolOutput::Blocks(blocks) => json!({ "content": blocks }),
                ToolOutput::Rich { content, .. } => json!({ "content": content }),
                _ => json!({}),
            };
            value["ok"] = json!(true);
            value
        }
        Err(error) => json!({ "ok": false, "error": error.message }),
    }
    .to_string())
}

/// Stops the server and forgets the handle (agents built with its tools
/// get `mcp_closed` errors from them).
pub async fn mcp_close(handle: u32) {
    let taken = lock(clients()).remove(&handle);
    if let Some(client) = taken {
        client.close().await;
    }
}

/// Adds the tools `config.mcp` names: `[{handle, names}]`, `names` null for
/// all of a server's tools.
pub(crate) fn add_mcp_tools(mut builder: AgentBuilder, config: &Value) -> Result<AgentBuilder> {
    for spec in config["mcp"].as_array().cloned().unwrap_or_default() {
        let handle = spec["handle"].as_u64().unwrap_or_default() as u32;
        let client = client(handle)?;
        let names: Option<Vec<String>> = spec["names"].as_array().map(|n| {
            n.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
        for tool in client.tools() {
            let tool: Arc<dyn Tool> = tool;
            if names.as_ref().is_none_or(|n| n.contains(&tool.spec().name)) {
                builder = builder.shared_tool(tool);
            }
        }
    }
    Ok(builder)
}

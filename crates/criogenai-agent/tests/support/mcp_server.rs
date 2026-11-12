//! A scripted MCP server for the tests (design 0022): the same logic behind
//! stdio (tests/mcp.rs) and HTTP (tests/mcp_http.rs). The mode string picks
//! its behaviour: `modern` or `legacy`, plus flags (`exit-on-discover`,
//! `dupe`, `deep`, `leak`, `sse`, `auth`, `oldver`).

#![allow(dead_code)]

use serde_json::{Value, json};

pub const VERSION: &str = "2026-07-28";

pub enum Act {
    Reply(Value),
    /// No answer (a notification, or a call that hangs).
    Silent,
    /// The process exits (stdio).
    Exit,
    /// A text result of this many bytes.
    Huge(usize),
    /// Forget the HTTP session after answering.
    Expire(Value),
}

pub struct Fake {
    pub mode: String,
    pub initialized: bool,
    pub cancelled: Vec<Value>,
}

fn ok(id: &Value, result: Value) -> Act {
    Act::Reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn err(id: &Value, code: i64, message: &str) -> Act {
    Act::Reply(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn text(t: &str) -> Value {
    json!({"content": [{"type": "text", "text": t}]})
}

impl Fake {
    pub fn new(mode: &str) -> Self {
        Self {
            mode: mode.to_string(),
            initialized: false,
            cancelled: Vec::new(),
        }
    }

    fn has(&self, flag: &str) -> bool {
        self.mode.split(',').any(|m| m == flag)
    }

    pub fn legacy(&self) -> bool {
        self.has("legacy")
    }

    fn tools(&self) -> Vec<Value> {
        let object = |props: Value| json!({"type": "object", "properties": props});
        if self.has("dupe") {
            return vec![
                json!({"name": "a.b", "inputSchema": object(json!({}))}),
                json!({"name": "a_b", "inputSchema": object(json!({}))}),
            ];
        }
        let mut tools = vec![
            json!({"name": "echo", "description": "Says it back.",
                   "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                   "annotations": {"readOnlyHint": true, "idempotentHint": true}}),
            json!({"name": "files.read", "title": "Read a file", "inputSchema": object(json!({"path": {"type": "string"}}))}),
            json!({"name": "fail", "description": "Always fails.", "inputSchema": object(json!({})),
                   "annotations": {"destructiveHint": true}}),
            json!({"name": "picture", "description": "A picture.", "inputSchema": object(json!({}))}),
            json!({"name": "slow", "description": "Never answers.", "inputSchema": object(json!({}))}),
            json!({"name": "crash", "description": "Exits.", "inputSchema": object(json!({}))}),
            json!({"name": "region", "description": "Header params.",
                   "inputSchema": object(json!({"region": {"type": "string", "x-mcp-header": "Region"}}))}),
            json!({"name": "stats", "description": "What the server saw.", "inputSchema": object(json!({}))}),
            json!({"name": "huge", "description": "A big answer.", "inputSchema": object(json!({"bytes": {"type": "integer"}}))}),
            json!({"name": "expire", "description": "Ends the HTTP session.", "inputSchema": object(json!({}))}),
        ];
        if self.has("deep") {
            let mut schema = json!({"type": "object"});
            for _ in 0..40 {
                schema = json!({"type": "object", "properties": {"x": schema}});
            }
            tools.push(json!({"name": "deep", "inputSchema": schema}));
        }
        tools
    }

    pub fn handle(&mut self, message: &Value) -> Act {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = message.get("id").cloned() else {
            match method {
                "notifications/initialized" => self.initialized = true,
                "notifications/cancelled" => {
                    self.cancelled.push(message["params"]["requestId"].clone())
                }
                _ => {}
            }
            return Act::Silent;
        };
        let params = message.get("params").cloned().unwrap_or(json!({}));
        match method {
            "server/discover" if self.legacy() => {
                if self.has("exit-on-discover") {
                    Act::Exit
                } else {
                    err(&id, -32601, "Method not found")
                }
            }
            "server/discover" => {
                let meta = &params["_meta"];
                if meta["io.modelcontextprotocol/protocolVersion"] != VERSION
                    || !meta["io.modelcontextprotocol/clientCapabilities"].is_object()
                    || meta["io.modelcontextprotocol/clientInfo"]["name"] != "criogenai"
                {
                    return err(&id, -32600, "missing _meta");
                }
                ok(
                    &id,
                    json!({
                        "resultType": "complete",
                        "supportedVersions": [VERSION],
                        "capabilities": {"tools": {}},
                        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "fake", "version": "1.0"}},
                        "instructions": "Be nice.",
                    }),
                )
            }
            "initialize" if self.legacy() => {
                let version = if self.has("oldver") {
                    json!("2024-11-05")
                } else if self.has("future") {
                    json!("2099-01-01")
                } else {
                    params["protocolVersion"].clone()
                };
                ok(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fake-legacy", "version": "0.9"},
                    }),
                )
            }
            _ if !self.legacy()
                && params["_meta"]["io.modelcontextprotocol/protocolVersion"] != VERSION =>
            {
                err(&id, -32600, "missing _meta")
            }
            _ if self.legacy() && !self.initialized && method != "ping" => {
                err(&id, -32002, "not initialized")
            }
            "ping" => ok(&id, json!({})),
            "tools/list" => {
                let all = self.tools();
                let start: usize = params["cursor"]
                    .as_str()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                let page: Vec<Value> = all.iter().skip(start).take(3).cloned().collect();
                let mut result = json!({"tools": page});
                if start + 3 < all.len() {
                    result["nextCursor"] = json!((start + 3).to_string());
                }
                ok(&id, result)
            }
            "tools/call" => {
                let args = &params["arguments"];
                match params["name"].as_str().unwrap_or_default() {
                    "echo" => ok(&id, text(args["text"].as_str().unwrap_or_default())),
                    "files.read" => ok(
                        &id,
                        json!({"content": [], "structuredContent": {"path": args["path"], "size": 12}}),
                    ),
                    "fail" => ok(
                        &id,
                        json!({"content": [{"type": "text", "text": "disk full"}], "isError": true}),
                    ),
                    "picture" => ok(
                        &id,
                        json!({"content": [
                            {"type": "text", "text": "a dot"},
                            {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"},
                            {"type": "resource_link", "uri": "file:///dot.png", "name": "dot"},
                        ]}),
                    ),
                    "slow" => Act::Silent,
                    "crash" => Act::Exit,
                    "region" => ok(&id, text("region ok")),
                    "stats" => ok(
                        &id,
                        json!({"content": [], "structuredContent": {
                            "initialized": self.initialized, "cancelled": self.cancelled,
                        }}),
                    ),
                    "huge" => Act::Huge(args["bytes"].as_u64().unwrap_or(10) as usize),
                    "expire" => {
                        Act::Expire(json!({"jsonrpc": "2.0", "id": id, "result": text("expired")}))
                    }
                    other => err(&id, -32602, &format!("unknown tool {other}")),
                }
            }
            _ => err(&id, -32601, "Method not found"),
        }
    }

    /// The answer for `Huge`.
    pub fn huge(id: &Value, bytes: usize) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "result": text(&"x".repeat(bytes))})
    }
}

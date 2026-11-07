//! MCP over Streamable HTTP (design 0022), against the scripted server
//! (tests/support/mcp_server.rs) on a local socket. Never the network.

#[path = "support/mcp_server.rs"]
mod fake;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogen_agent::ToolOutput;
use criogen_agent::mcp::{LEGACY_PROTOCOL_VERSION, McpErrorKind, McpServer, PROTOCOL_VERSION};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "Bearer mcp-secret-token-4411";

type Seen = Arc<Mutex<Vec<HashMap<String, String>>>>;

struct Server {
    url: String,
    seen: Seen,
}

struct State {
    fake: fake::Fake,
    session: Option<String>,
    sessions: u32,
}

fn response(status: u16, kind: &str, body: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

async fn start(mode: &str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let state = Arc::new(Mutex::new(State {
        fake: fake::Fake::new(mode),
        session: None,
        sessions: 0,
    }));
    let record = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            let record = record.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                let (head, body) = loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&data[..at]).to_string();
                        let length: usize = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        while data.len() < at + 4 + length {
                            let n = socket.read(&mut buf).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            data.extend_from_slice(&buf[..n]);
                        }
                        break (head, data[at + 4..].to_vec());
                    }
                };
                let headers: HashMap<String, String> = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                record.lock().unwrap().push(headers.clone());
                let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let reply = {
                    let mut st = state.lock().unwrap();
                    let method = message["method"].as_str().unwrap_or_default().to_string();
                    let legacy = st.fake.legacy();
                    if st.fake.mode.contains("auth") {
                        Some(response(401, "text/plain", b"unauthorized", ""))
                    } else if legacy && method == "server/discover" {
                        // What servers from before `server/discover` say.
                        Some(response(
                            400,
                            "text/plain",
                            b"Bad Request: No valid session ID provided",
                            "",
                        ))
                    } else if legacy
                        && method != "initialize"
                        && headers.get("mcp-session-id") != st.session.as_ref()
                    {
                        Some(response(404, "text/plain", b"session not found", ""))
                    } else {
                        let act = st.fake.handle(&message);
                        let mut extra = String::new();
                        if legacy && method == "initialize" {
                            st.sessions += 1;
                            let id = format!("sess-{}", st.sessions);
                            extra = format!("Mcp-Session-Id: {id}\r\n");
                            st.session = Some(id);
                        }
                        let value = match act {
                            fake::Act::Reply(v) => Some(v),
                            fake::Act::Expire(v) => {
                                st.session = None;
                                Some(v)
                            }
                            fake::Act::Huge(n) => Some(fake::Fake::huge(&message["id"], n)),
                            fake::Act::Silent | fake::Act::Exit => None,
                        };
                        match value {
                            None if message.get("id").is_none() => {
                                Some(response(202, "text/plain", b"", ""))
                            }
                            // A call that never answers.
                            None => None,
                            Some(v) if st.fake.mode.contains("sse") => {
                                let body = format!(
                                    "event: message\ndata: {}\n\ndata: {v}\n\n",
                                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}})
                                );
                                Some(response(200, "text/event-stream", body.as_bytes(), &extra))
                            }
                            Some(v) => Some(response(
                                200,
                                "application/json",
                                v.to_string().as_bytes(),
                                &extra,
                            )),
                        }
                    }
                };
                let Some(reply) = reply else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    return;
                };
                let _ = socket.write_all(&reply).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Server { url, seen }
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn last_call(seen: &Seen) -> HashMap<String, String> {
    seen.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|h| h.get("mcp-method").map(String::as_str) == Some("tools/call"))
        .cloned()
        .unwrap_or_else(|| seen.lock().unwrap().last().cloned().unwrap())
}

#[tokio::test]
async fn modern_http_sends_the_protocol_headers() {
    let server = start("modern").await;
    let client = McpServer::http(&server.url)
        .header("Authorization", TOKEN)
        .connect()
        .await
        .unwrap();
    assert_eq!(client.info().protocol_version, PROTOCOL_VERSION);
    assert_eq!(client.tool_names().len(), 10);

    let out = client
        .call("echo", args(json!({"text": "hi"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("hi".into()));
    let h = last_call(&server.seen);
    assert_eq!(h["mcp-protocol-version"], PROTOCOL_VERSION);
    assert_eq!(h["mcp-method"], "tools/call");
    assert_eq!(h["mcp-name"], "echo");
    assert_eq!(h["authorization"], TOKEN);
    assert!(h["accept"].contains("text/event-stream"));

    client
        .call("region", args(json!({"region": "eu-west"})))
        .await
        .unwrap();
    assert_eq!(last_call(&server.seen)["mcp-param-region"], "eu-west");
    client
        .call("region", args(json!({"region": "zürich"})))
        .await
        .unwrap();
    assert_eq!(
        last_call(&server.seen)["mcp-param-region"],
        "=?base64?esO8cmljaA==?="
    );

    let err = client.call("fail", Map::new()).await.unwrap_err();
    assert_eq!(err.message, "disk full");
}

#[tokio::test]
async fn answers_can_come_as_an_sse_stream() {
    let server = start("modern,sse").await;
    let client = McpServer::http(&server.url).connect().await.unwrap();
    let out = client
        .call("files_read", args(json!({"path": "/x"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Json(json!({"path": "/x", "size": 12})));
}

#[tokio::test]
async fn legacy_http_uses_a_session_and_renews_it() {
    let server = start("legacy").await;
    let client = McpServer::http(&server.url).connect().await.unwrap();
    assert_eq!(client.info().protocol_version, LEGACY_PROTOCOL_VERSION);
    let out = client
        .call("echo", args(json!({"text": "s"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("s".into()));
    let h = server.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(h["mcp-session-id"], "sess-1");
    assert_eq!(h["mcp-protocol-version"], LEGACY_PROTOCOL_VERSION);
    assert!(!h.contains_key("mcp-method"));

    // The server forgets the session: the next call starts a new one.
    client.call("expire", Map::new()).await.unwrap();
    let out = client
        .call("echo", args(json!({"text": "again"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("again".into()));
    let h = server.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(h["mcp-session-id"], "sess-2");
}

#[tokio::test]
async fn refused_credentials_fail_without_falling_back_or_leaking() {
    let server = start("modern,auth").await;
    let config = McpServer::http(&server.url).header("Authorization", TOKEN);
    assert!(!format!("{config:?}").contains("secret"));
    let err = config.connect().await.unwrap_err();
    assert_eq!(err.kind, McpErrorKind::Connect);
    assert!(err.message.contains("401"), "{err}");
    assert!(!err.message.contains("secret"), "{err}");
    // One request: no `initialize` after a 401.
    assert_eq!(server.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn credentials_never_go_over_plain_http_to_another_machine() {
    let err = McpServer::http("http://example.com/mcp")
        .header("Authorization", TOKEN)
        .connect()
        .await
        .unwrap_err();
    assert_eq!(err.code(), "mcp_connect");
    assert!(!err.message.contains("secret"));
}

#[tokio::test]
async fn slow_calls_time_out() {
    let server = start("modern").await;
    let client = McpServer::http(&server.url)
        .timeout(Duration::from_millis(300))
        .connect()
        .await
        .unwrap();
    let err = client.call("slow", Map::new()).await.unwrap_err();
    assert!(err.message.contains("no answer in time"), "{err:?}");
    let out = client
        .call("echo", args(json!({"text": "still here"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("still here".into()));
}

#[tokio::test]
async fn oversized_answers_are_refused() {
    let server = start("modern").await;
    let client = McpServer::http(&server.url)
        .max_message_bytes(4096)
        .connect()
        .await
        .unwrap();
    let err = client
        .call("huge", args(json!({"bytes": 20_000})))
        .await
        .unwrap_err();
    assert!(err.message.contains("larger than"), "{err:?}");
}

#[tokio::test]
async fn an_unreachable_server_is_a_connect_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);
    let err = McpServer::http(url).connect().await.unwrap_err();
    assert_eq!(err.code(), "mcp_connect", "{err}");
}

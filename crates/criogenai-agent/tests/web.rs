//! Web tools (design 0019): search and page reading, against a local server only.
#![cfg(feature = "web")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogenai_agent::{Agent, Tool, web};
use criogenai_providers::{ContentBlock, FakeModel, FakeReply, Message, Role, ToolStatus};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SECRET: &str = "tvly-secret-key-123";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn reply(status: u16, kind: &str, body: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

async fn start() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, length) = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buffer).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while buffer.len() < head_end + length {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or("").split(' ');
                let (method, path) = (
                    first.next().unwrap_or("").to_string(),
                    first.next().unwrap_or("").to_string(),
                );
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                    .collect();
                let body = serde_json::from_slice(&buffer[head_end..]).unwrap_or(Value::Null);
                log.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    headers: headers.clone(),
                    body: body.clone(),
                });
                let json_kind = "application/json";
                let response = match (method.as_str(), path.as_str()) {
                    ("POST", "/search") => {
                        let auth = headers.get("authorization").cloned().unwrap_or_default();
                        if auth != format!("Bearer {SECRET}") {
                            reply(401, json_kind, json!({"error": format!("bad key {auth}")}).to_string().as_bytes(), "")
                        } else {
                            let results: Vec<Value> = (0..20)
                                .map(|i| json!({"title": format!("Result {i}"), "url": format!("https://example.com/{i}"), "content": "x".repeat(2000)}))
                                .collect();
                            reply(200, json_kind, json!({"answer": "Solar is growing.", "results": results}).to_string().as_bytes(), "")
                        }
                    }
                    ("GET", p) if p.starts_with("/res/v1/web/search") => reply(
                        200,
                        json_kind,
                        json!({"web": {"results": [{"title": "Brave hit", "url": "https://example.org", "description": "d"}]}}).to_string().as_bytes(),
                        "",
                    ),
                    ("GET", "/page") => reply(
                        200,
                        "text/html",
                        b"<html><head><title>t</title><script>evil()</script></head><body><h1>Hello</h1><p>World &amp; more</p></body></html>",
                        "",
                    ),
                    ("GET", "/plain") => reply(200, "text/plain", b"a < b\n\n  and c  \n", ""),
                    ("GET", "/redirect") => reply(302, "text/plain", b"", "location: http://127.0.0.1:1/elsewhere\r\n"),
                    ("GET", "/big") => reply(200, "text/plain", &vec![b'a'; 600_000], ""),
                    ("GET", "/long") => reply(200, "text/plain", "word ".repeat(5_000).as_bytes(), ""),
                    _ => reply(404, json_kind, b"{}", ""),
                };
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Server { base, seen }
}

/// Calls `tool` once through an agent, the way a model would, and returns the tool message.
async fn call(tool: impl Tool, name: &str, args: Value) -> Message {
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call(name, args))
        .then(FakeReply::text("done"));
    let agent = Agent::builder(model).tool(tool).build().unwrap();
    let result = agent.run("go").await.unwrap();
    result
        .messages
        .into_iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool message")
}

fn value_of(message: &Message) -> Value {
    message
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Json { value, .. } => Some(value.clone()),
            _ => None,
        })
        .expect("a JSON result")
}

fn everything(message: &Message) -> String {
    serde_json::to_string(message).unwrap()
}

#[tokio::test]
async fn tavily_returns_bounded_results_and_sends_the_key_only_as_a_header() {
    let server = start().await;
    let tool = web::tavily_search()
        .api_key(SECRET)
        .max_results(3)
        .base_url(&server.base)
        .unwrap();
    let message = call(tool, "web_search", json!({"query": "solar panels"})).await;
    assert_eq!(message.status, Some(ToolStatus::Success));
    let value = value_of(&message);
    assert_eq!(value["answer"], "Solar is growing.");
    assert_eq!(
        value["results"].as_array().unwrap().len(),
        3,
        "asked for 3 of 20"
    );
    assert!(
        value["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["snippet"].as_str().unwrap().chars().count() <= 600)
    );
    let seen = server.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.headers["authorization"], format!("Bearer {SECRET}"));
    assert_eq!(seen.body["query"], "solar panels");
    assert!(
        !everything(&message).contains(SECRET),
        "never in what the model sees"
    );
}

#[tokio::test]
async fn a_wrong_key_gives_a_status_not_the_response_body() {
    let server = start().await;
    let tool = web::tavily_search()
        .api_key("wrong-key")
        .base_url(&server.base)
        .unwrap();
    let message = call(tool, "web_search", json!({"query": "x"})).await;
    assert_eq!(message.status, Some(ToolStatus::Error));
    let text = everything(&message);
    assert!(
        text.contains("401") && !text.contains("wrong-key") && !text.contains("bad key"),
        "{text}"
    );
}

#[tokio::test]
async fn search_without_any_key_says_what_to_set() {
    // Environment variables cannot be changed here (no unsafe), so this checks
    // the unset case and is a no-op on a machine that has the keys.
    if std::env::var("TAVILY_API_KEY").is_ok() || std::env::var("BRAVE_API_KEY").is_ok() {
        return;
    }
    assert!(
        web::search()
            .unwrap_err()
            .to_string()
            .contains("TAVILY_API_KEY")
    );
    let message = call(web::tavily_search(), "web_search", json!({"query": "x"})).await;
    assert_eq!(message.status, Some(ToolStatus::Error));
    assert!(everything(&message).contains("TAVILY_API_KEY"));
    assert!(
        web::default_tools().is_empty(),
        "a researcher still works, just without the internet"
    );
}

#[tokio::test]
async fn brave_sends_its_key_in_its_own_header() {
    let server = start().await;
    let brave = web::brave_search()
        .api_key("brave-key")
        .base_url(&server.base)
        .unwrap();
    let message = call(brave, "web_search", json!({"query": "rain & wind"})).await;
    assert_eq!(value_of(&message)["results"][0]["title"], "Brave hit");
    let seen = server.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.headers["x-subscription-token"], "brave-key");
    assert!(
        seen.path.contains("q=rain+%26+wind") || seen.path.contains("q=rain%20%26%20wind"),
        "{}",
        seen.path
    );
    assert!(
        !seen.path.contains("brave-key"),
        "the key is never in a URL"
    );
}

#[test]
fn credentials_do_not_go_over_plain_http_to_another_machine() {
    let err = web::tavily_search()
        .base_url("http://search.example.com")
        .unwrap_err();
    assert_eq!(err, web::WebError::InsecureUrl);
    assert!(
        web::brave_search()
            .base_url("https://proxy.example.com")
            .is_ok()
    );
    assert!(web::brave_search().base_url("not a url").is_err());
}

#[tokio::test]
async fn fetch_reads_text_skips_scripts_and_refuses_private_addresses_by_default() {
    let server = start().await;
    let refused = call(
        web::fetch(),
        "read_page",
        json!({"url": format!("{}/page", server.base)}),
    )
    .await;
    assert_eq!(refused.status, Some(ToolStatus::Error));
    assert!(everything(&refused).contains("private network"));
    assert!(
        server.seen.lock().unwrap().is_empty(),
        "no request left this machine"
    );

    // By name too: localhost resolves to a loopback address.
    let port = server.base.rsplit(':').next().unwrap();
    let named = call(
        web::fetch(),
        "read_page",
        json!({"url": format!("http://localhost:{port}/page")}),
    )
    .await;
    assert!(everything(&named).contains("private network"));

    let ok = call(
        web::fetch().allow_private(true),
        "read_page",
        json!({"url": format!("{}/page", server.base)}),
    )
    .await;
    let text = value_of(&ok)["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("Hello") && text.contains("World & more") && !text.contains("evil"),
        "{text}"
    );

    let by_name = call(
        web::fetch().allow_private(true),
        "read_page",
        json!({"url": format!("http://localhost:{port}/page")}),
    )
    .await;
    assert_eq!(
        by_name.status,
        Some(ToolStatus::Success),
        "{}",
        everything(&by_name)
    );

    let plain = call(
        web::fetch().allow_private(true),
        "read_page",
        json!({"url": format!("{}/plain", server.base)}),
    )
    .await;
    assert_eq!(value_of(&plain)["text"], "a < b\nand c");

    for bad in ["file:///etc/passwd", "ftp://example.com/x", "not a url"] {
        let message = call(
            web::fetch().allow_private(true),
            "read_page",
            json!({"url": bad}),
        )
        .await;
        assert_eq!(message.status, Some(ToolStatus::Error), "{bad}");
    }
}

#[tokio::test]
async fn fetch_does_not_follow_redirects_and_bounds_what_it_reads() {
    let server = start().await;
    let fetch = || web::fetch().allow_private(true);
    let redirected = call(
        fetch(),
        "read_page",
        json!({"url": format!("{}/redirect", server.base)}),
    )
    .await;
    assert_eq!(redirected.status, Some(ToolStatus::Error));
    assert!(everything(&redirected).contains("redirect"));

    let big = call(
        fetch(),
        "read_page",
        json!({"url": format!("{}/big", server.base)}),
    )
    .await;
    assert_eq!(big.status, Some(ToolStatus::Error));
    assert!(everything(&big).contains("too large"));

    let long = call(
        fetch().max_chars(1_000),
        "read_page",
        json!({"url": format!("{}/long", server.base)}),
    )
    .await;
    let value = value_of(&long);
    assert_eq!(value["text"].as_str().unwrap().chars().count(), 1_000);
    assert_eq!(value["truncated"], true);

    let missing = call(
        fetch(),
        "read_page",
        json!({"url": format!("{}/nope", server.base)}),
    )
    .await;
    assert!(everything(&missing).contains("404"));
}

#[tokio::test(start_paused = true)]
async fn the_researcher_builds_with_or_without_web_tools() {
    let model: Arc<dyn criogenai_providers::Model> =
        Arc::new(FakeModel::with_delay(Duration::ZERO, Duration::ZERO).then(FakeReply::text("ok")));
    let built = criogenai_agent::specialists::researcher()
        .build(model)
        .unwrap();
    assert_eq!(built.run("x").await.unwrap().text(), Some("ok"));
    // Explicit tools replace the default; extend() keeps the default.
    assert!(
        criogenai_agent::specialists::researcher()
            .extend("r2")
            .tool_names()
            .is_empty()
    );
}

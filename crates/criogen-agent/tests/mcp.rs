//! MCP over stdio (design 0022). `harness = false`: started with
//! `CRIOGEN_MCP_FAKE=<mode>`, this binary is the scripted MCP server
//! (tests/support/mcp_server.rs) on its stdin and stdout; otherwise it runs
//! the tests below, which start it again as the server.

#[path = "support/mcp_server.rs"]
mod fake;

use std::future::Future;
use std::io::{BufRead, Write};
use std::pin::Pin;
use std::time::Duration;

use criogen_agent::mcp::{LEGACY_PROTOCOL_VERSION, McpErrorKind, McpServer, PROTOCOL_VERSION};
use criogen_agent::{Agent, ToolOutput};
use criogen_providers::{ContentBlock, FakeModel, FakeReply, Role, ToolKind};
use serde_json::{Map, Value, json};

const SECRET: &str = "super-secret-token-9731";

fn serve(mode: &str) {
    let mut server = fake::Fake::new(mode);
    if server.mode.contains("leak") {
        eprintln!(
            "starting with token {}",
            std::env::var("MCP_TOKEN").unwrap_or_default()
        );
        std::process::exit(3);
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    // A stray line a server should not print: the client skips it.
    writeln!(out, "fake server ready").unwrap();
    out.flush().unwrap();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let reply = match server.handle(&message) {
            fake::Act::Reply(v) | fake::Act::Expire(v) => v,
            fake::Act::Silent => continue,
            fake::Act::Exit => std::process::exit(1),
            fake::Act::Huge(n) => fake::Fake::huge(&message["id"], n),
        };
        writeln!(out, "{reply}").unwrap();
        out.flush().unwrap();
    }
}

fn server(mode: &str) -> McpServer {
    let exe = std::env::current_exe().unwrap();
    McpServer::stdio(exe.to_string_lossy(), Vec::<String>::new())
        .env("CRIOGEN_MCP_FAKE", mode)
        .name("fake")
        .connect_timeout(Duration::from_secs(20))
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

async fn modern_server_tools_and_info() {
    let client = server("modern").connect().await.unwrap();
    let info = client.info();
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.name.as_deref(), Some("fake"));
    assert_eq!(info.instructions.as_deref(), Some("Be nice."));
    // Three pages of tools, `.` mapped to `_`.
    let names = client.tool_names();
    assert_eq!(names.len(), 10, "{names:?}");
    assert!(names.contains(&"files_read".to_string()));
    let tools = client.tools();
    let echo = tools.iter().find(|t| t.spec().name == "echo").unwrap();
    assert_eq!(echo.spec().kind, ToolKind::Mcp);
    // Hints are not trusted by default; destructive is kept.
    assert!(!echo.spec().annotations.read_only);
    let fail = tools.iter().find(|t| t.spec().name == "fail").unwrap();
    assert!(fail.spec().annotations.destructive);
    let read = tools
        .iter()
        .find(|t| t.spec().name == "files_read")
        .unwrap();
    assert_eq!(read.spec().description, "Read a file");
    client.close().await;

    let trusted = server("modern")
        .trust_annotations(true)
        .connect()
        .await
        .unwrap();
    let echo = trusted
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "echo")
        .unwrap();
    assert!(echo.spec().annotations.read_only && echo.spec().annotations.idempotent);
}

async fn an_agent_calls_an_mcp_tool() {
    let client = server("modern").connect().await.unwrap();
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call(
            "echo",
            json!({"text": "hello from MCP"}),
        ))
        .then(FakeReply::text("done"));
    let agent = Agent::builder(model).tools(client.tools()).build().unwrap();
    let result = agent.run("go").await.unwrap();
    let message = result
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert!(
        message
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text, .. } if text == "hello from MCP")),
        "{message:?}"
    );
}

async fn results_of_every_kind() {
    let client = server("modern").connect().await.unwrap();
    let out = client
        .call("files_read", args(json!({"path": "/a"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Json(json!({"path": "/a", "size": 12})));
    let err = client.call("fail", Map::new()).await.unwrap_err();
    assert_eq!(err.message, "disk full");
    let ToolOutput::Blocks(blocks) = client.call("picture", Map::new()).await.unwrap() else {
        panic!("blocks")
    };
    assert_eq!(blocks.len(), 3);
    assert!(matches!(&blocks[1], ContentBlock::Image { .. }));
    assert!(
        matches!(&blocks[2], ContentBlock::Text { text, .. } if text == "resource: file:///dot.png (dot)")
    );
}

async fn legacy_servers_get_the_initialize_handshake() {
    let client = server("legacy").connect().await.unwrap();
    assert_eq!(client.info().protocol_version, LEGACY_PROTOCOL_VERSION);
    assert_eq!(client.info().name.as_deref(), Some("fake-legacy"));
    let out = client.call("stats", Map::new()).await.unwrap();
    assert_eq!(
        out,
        ToolOutput::Json(json!({"initialized": true, "cancelled": []}))
    );

    let old = server("legacy,oldver").connect().await.unwrap();
    assert_eq!(old.info().protocol_version, "2024-11-05");

    let err = server("legacy,future").connect().await.unwrap_err();
    assert_eq!(err.kind, McpErrorKind::Protocol);
    assert!(err.message.contains("2099-01-01"), "{err}");

    // A legacy server that exits on the unknown method is started again.
    let client = server("legacy,exit-on-discover").connect().await.unwrap();
    let out = client
        .call("echo", args(json!({"text": "back"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("back".into()));
}

async fn a_slow_call_times_out_and_is_cancelled() {
    let client = server("modern")
        .timeout(Duration::from_millis(300))
        .connect()
        .await
        .unwrap();
    let err = client.call("slow", Map::new()).await.unwrap_err();
    assert!(err.message.contains("no answer in time"), "{err:?}");
    assert!(err.retryable);
    let ToolOutput::Json(stats) = client.call("stats", Map::new()).await.unwrap() else {
        panic!("json")
    };
    assert_eq!(stats["cancelled"].as_array().unwrap().len(), 1, "{stats}");
}

async fn a_crashed_server_is_started_again() {
    let client = server("legacy").connect().await.unwrap();
    let err = client.call("crash", Map::new()).await.unwrap_err();
    assert!(err.message.contains("the server stopped"), "{err:?}");
    assert!(err.retryable);
    // Legacy: the handshake is done again.
    let out = client
        .call("echo", args(json!({"text": "alive"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("alive".into()));
}

async fn oversized_messages_are_refused() {
    let client = server("modern")
        .max_message_bytes(4096)
        .connect()
        .await
        .unwrap();
    let err = client
        .call("huge", args(json!({"bytes": 20_000})))
        .await
        .unwrap_err();
    assert!(err.message.contains("the server stopped"), "{err:?}");
    let out = client
        .call("huge", args(json!({"bytes": 10})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("x".repeat(10)));
}

async fn names_prefixes_and_filters() {
    let err = server("modern,dupe").connect().await.unwrap_err();
    assert_eq!(err.kind, McpErrorKind::ToolName);
    assert_eq!(err.code(), "mcp_tool_name");
    let client = server("modern,dupe")
        .except(["a_b"])
        .connect()
        .await
        .unwrap();
    assert_eq!(client.tool_names(), ["a_b"]);

    let err = server("modern,deep").connect().await.unwrap_err();
    assert_eq!(err.code(), "mcp_tool_schema");

    let client = server("modern")
        .prefix("fs")
        .only(["echo", "files.read"])
        .connect()
        .await
        .unwrap();
    assert_eq!(client.tool_names(), ["fs_echo", "fs_files_read"]);
    let out = client
        .call("fs_echo", args(json!({"text": "p"})))
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::Text("p".into()));
}

async fn failures_to_start_and_closing() {
    let err = McpServer::stdio("criogen-no-such-command-xyz", ["a"])
        .connect()
        .await
        .unwrap_err();
    assert_eq!(err.code(), "mcp_connect");

    let client = server("modern").connect().await.unwrap();
    client.close().await;
    let err = client
        .call("echo", args(json!({"text": "x"})))
        .await
        .unwrap_err();
    assert!(err.message.contains("stopped"), "{err:?}");
}

async fn secrets_stay_out_of_errors_and_debug() {
    let config = server("modern,leak").env("MCP_TOKEN", SECRET);
    assert!(!format!("{config:?}").contains(SECRET));
    let err = config.connect().await.unwrap_err();
    assert!(!err.message.contains(SECRET), "{err}");
    assert!(err.message.contains("[redacted]"), "{err}");
}

type Test = fn() -> Pin<Box<dyn Future<Output = ()>>>;

macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        [$((stringify!($name), (|| Box::pin($name()) as Pin<Box<dyn Future<Output = ()>>>) as Test)),*]
    };
}

fn main() {
    if let Ok(mode) = std::env::var("CRIOGEN_MCP_FAKE") {
        serve(&mode);
        return;
    }
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let all = tests![
        modern_server_tools_and_info,
        an_agent_calls_an_mcp_tool,
        results_of_every_kind,
        legacy_servers_get_the_initialize_handshake,
        a_slow_call_times_out_and_is_cancelled,
        a_crashed_server_is_started_again,
        oversized_messages_are_refused,
        names_prefixes_and_filters,
        failures_to_start_and_closing,
        secrets_stay_out_of_errors_and_debug,
    ];
    let mut failed = 0;
    let mut ran = 0;
    for (name, test) in all {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(60), test())
                    .await
                    .expect("the test took over 60 s")
            })
        }));
        drop(runtime);
        match outcome {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                failed += 1;
                println!("test {name} ... FAILED");
            }
        }
    }
    println!("\nmcp (stdio): {} passed, {failed} failed", ran - failed);
    if failed > 0 {
        std::process::exit(1);
    }
}

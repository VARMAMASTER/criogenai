//! `criogenai-trace [--listen 127.0.0.1:4318] [--db <file> | --memory]
//! [--keep-runs N] [--keep-days N] [--prices prices.json]
//! [--allow-remote --token-env NAME]` (design 0023).

use std::net::SocketAddr;
use std::process::ExitCode;

use criogenai_trace::{DEFAULT_ADDRESS, Prices, Viewer, default_db};

const USAGE: &str = "usage: criogenai-trace [--listen <addr>] [--db <file> | --memory] [--keep-runs N] [--keep-days N]
                     [--prices <file.json>] [--allow-remote --token-env <NAME>]

  --listen        address to serve on (default 127.0.0.1:4318, OTLP/HTTP's port)
  --db            SQLite file for runs (default ~/.criogen/traces.db)
  --memory        keep runs in memory only
  --keep-runs     newest runs kept (default 10000)
  --keep-days     days a run is kept (default 14)
  --prices        JSON of USD per million tokens: {\"openai:gpt-5-mini\": {\"input\": 0.25, \"output\": 2}}
  --allow-remote  listen on another address than this computer's (needs --token-env)
  --token-env     environment variable holding the token clients must send";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("criogenai-trace: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let mut listen: SocketAddr = DEFAULT_ADDRESS.parse().expect("valid default");
    let mut builder = Viewer::builder();
    let mut db = default_db();
    let mut allow_remote = false;
    let mut args = std::env::args().skip(1);
    let number = |value: Option<String>, what: &str| -> Result<u64, String> {
        value
            .ok_or(USAGE)?
            .parse()
            .map_err(|_| format!("{what} needs a number"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                let value = args.next().ok_or(USAGE)?;
                listen = value
                    .parse()
                    .map_err(|_| format!("not an address: {value}"))?;
            }
            "--db" => db = Some(args.next().ok_or(USAGE)?.into()),
            "--memory" => db = None,
            "--keep-runs" => {
                builder = builder.keep_runs(number(args.next(), "--keep-runs")? as usize)
            }
            "--keep-days" => builder = builder.keep_days(number(args.next(), "--keep-days")?),
            "--prices" => {
                let path = args.next().ok_or(USAGE)?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                let prices: Prices =
                    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
                builder = builder.prices(prices);
            }
            "--allow-remote" => allow_remote = true,
            "--token-env" => {
                let name = args.next().ok_or(USAGE)?;
                let token = std::env::var(&name)
                    .map_err(|_| format!("the environment variable {name} is not set"))?;
                builder = builder.token(token);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    if !listen.ip().is_loopback() && !allow_remote {
        return Err(format!(
            "refusing to listen on {listen}: pass --allow-remote with --token-env, or use 127.0.0.1"
        ));
    }
    if let Some(path) = &db {
        builder = builder.sqlite(path);
    }
    let viewer = builder.build().await.map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    match &db {
        Some(path) => eprintln!("criogenai-trace: runs in {}", path.display()),
        None => eprintln!("criogenai-trace: runs in memory"),
    }
    eprintln!("criogenai-trace: open http://{listen} (traces: http://{listen}/v1/traces)");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let result = criogenai_trace::serve(viewer.clone(), listener, shutdown).await;
    viewer.close().await;
    result.map_err(|e| e.to_string())
}

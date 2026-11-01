//! `criogen-gateway --config gateway.toml [--listen 127.0.0.1:4000]
//! [--allow-open] [--log-usage]` (design 0006 §12).

use std::net::SocketAddr;
use std::process::ExitCode;

use criogen_gateway::Config;

const USAGE: &str =
    "usage: criogen-gateway --config <file.toml> [--listen <addr>] [--allow-open] [--log-usage]

  --config      routes and keys (secrets come from the environment variables it names)
  --listen      address to serve on (default 127.0.0.1:4000)
  --allow-open  serve a gateway without keys on a non-loopback address
  --log-usage   print a JSON usage record (no content) per request to stderr";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("criogen-gateway: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let mut config_path = None;
    let mut listen: SocketAddr = "127.0.0.1:4000".parse().expect("valid default");
    let mut allow_open = false;
    let mut log_usage = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = args.next(),
            "--listen" => {
                let value = args.next().ok_or(USAGE)?;
                listen = value
                    .parse()
                    .map_err(|_| format!("not an address: {value}"))?;
            }
            "--allow-open" => allow_open = true,
            "--log-usage" => log_usage = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    let path = config_path.ok_or(USAGE)?;
    let text = std::fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?;
    let mut builder = Config::parse(&text)
        .and_then(Config::into_builder)
        .map_err(|err| err.to_string())?;
    if log_usage {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(10_000);
        builder = builder.usage(sender);
        tokio::spawn(async move {
            while let Some(record) = receiver.recv().await {
                if let Ok(line) = serde_json::to_string(&record) {
                    eprintln!("{line}");
                }
            }
        });
    }
    let gateway = builder.build().map_err(|err| err.to_string())?;
    if !gateway.has_keys() && !listen.ip().is_loopback() && !allow_open {
        return Err(format!(
            "refusing to serve a gateway without keys on {listen}: add [[keys]], listen on \
             127.0.0.1, or pass --allow-open"
        ));
    }
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|err| format!("cannot listen on {listen}: {err}"))?;
    eprintln!("criogen-gateway listening on http://{listen}");
    axum::serve(listener, gateway.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|err| err.to_string())
}

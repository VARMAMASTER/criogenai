//! Streams one answer from a real model server.
//!
//! Defaults to Ollama on this machine. Settings come from the environment:
//!
//! - `AGENTBUILD_PROTOCOL`: `openai` (default, any OpenAI-compatible server)
//!   or `anthropic`
//! - `AGENTBUILD_BASE_URL` (default `http://localhost:11434/v1`, or
//!   `https://api.anthropic.com/v1` for `anthropic`)
//! - `AGENTBUILD_MODEL` (default `llama3.2`; set it for `anthropic`)
//! - `AGENTBUILD_API_KEY` (optional; for `anthropic` also read from
//!   `ANTHROPIC_API_KEY`)
//!
//! Run with: `cargo run -p agentbuild-providers --example chat -- "Why is the sky blue?"`
//!
//! Not run in CI (it needs a model server). Tests use `FakeModel` and a
//! scripted local server instead.

use std::io::Write;

use agentbuild_providers::{
    BlockDelta, Endpoint, Message, MessageDelta, Model, ModelRequest, ProviderModel,
};

#[tokio::main]
async fn main() {
    let anthropic = std::env::var("AGENTBUILD_PROTOCOL").is_ok_and(|p| p == "anthropic");
    let default_url = if anthropic {
        "https://api.anthropic.com/v1"
    } else {
        "http://localhost:11434/v1"
    };
    let base_url = std::env::var("AGENTBUILD_BASE_URL").unwrap_or_else(|_| default_url.into());
    let model_name = std::env::var("AGENTBUILD_MODEL").unwrap_or_else(|_| "llama3.2".into());
    let mut api_key = std::env::var("AGENTBUILD_API_KEY").ok();
    if anthropic && api_key.is_none() {
        api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    }
    let question = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let question = if question.is_empty() {
        "Say hello in one sentence.".to_string()
    } else {
        question
    };

    let endpoint = if anthropic {
        Endpoint::anthropic_compatible("anthropic", base_url, api_key)
    } else {
        Endpoint::openai_compatible("local", base_url, api_key)
    };
    let model = ProviderModel::new(endpoint, model_name).expect("HTTP client");
    let request = ModelRequest::new(vec![Message::user(question)]);

    let mut print = |delta: MessageDelta| {
        if let BlockDelta::Text { text } = delta.delta {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
    };
    match model.stream(&request, &mut print).await {
        Ok(response) => {
            println!();
            eprintln!(
                "[{} {} | {} in, {} out | {:?}]",
                model.provider(),
                response.message.metadata.model.as_deref().unwrap_or("?"),
                response.usage.input_tokens,
                response.usage.output_tokens,
                response.finish_reason
            );
            for warning in &response.warnings {
                eprintln!("warning: {} ({})", warning.message, warning.code);
            }
        }
        Err(err) => {
            eprintln!(
                "error: {err} [{}, dispatch {:?}]",
                err.category(),
                err.dispatch
            );
            std::process::exit(1);
        }
    }
}

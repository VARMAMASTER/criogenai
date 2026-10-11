//! Streams one answer from a real OpenAI-compatible server.
//!
//! Defaults to Ollama on this machine. Settings come from the environment:
//!
//! - `AGENTBUILD_BASE_URL` (default `http://localhost:11434/v1`)
//! - `AGENTBUILD_MODEL` (default `llama3.2`)
//! - `AGENTBUILD_API_KEY` (optional)
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
    let base_url =
        std::env::var("AGENTBUILD_BASE_URL").unwrap_or_else(|_| "http://localhost:11434/v1".into());
    let model_name = std::env::var("AGENTBUILD_MODEL").unwrap_or_else(|_| "llama3.2".into());
    let api_key = std::env::var("AGENTBUILD_API_KEY").ok();
    let question = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let question = if question.is_empty() {
        "Say hello in one sentence.".to_string()
    } else {
        question
    };

    let endpoint = Endpoint::openai_compatible("local", base_url, api_key);
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

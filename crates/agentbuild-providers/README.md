# agentbuild-providers

The model-provider layer of [AgentBuild](https://github.com/VARMAMASTER/agentbuild).
It defines the language-neutral data contracts that agents, tools and model
providers share, the same JSON in Rust, Python and TypeScript:

- `Message` with ordered `ContentBlock`s (text, reasoning, images, files,
  JSON, tool calls, invalid tool calls, provider-specific blocks)
- `ToolSpec` with registration-time validation
- `ModelRequest` / `ModelResponse` / `Capabilities`
- `ModelError`, classified into stable kinds (`rate_limited`, `overloaded`,
  `context_overflow`, ...) from any provider's HTTP error, with `Retry-After`
  parsing and credential redaction
- transcript repair: every tool call gets exactly one result
- the `Model` trait, and `FakeModel` for tests (simulated 2–3 s replies,
  scripted tool calls and failures)
- `ProviderModel`: a model reached over HTTP (feature `http`, on by
  default) with streaming, through one of two protocols:
  - OpenAI chat completions: OpenAI, OpenRouter, Ollama, LM Studio, vLLM
    and other compatible servers (`Endpoint::openai`, `Endpoint::ollama`,
    ...)
  - Anthropic Messages: `Endpoint::anthropic`, `Endpoint::anthropic_compatible`
- embedding models for semantic search: the `Embedder` trait,
  `ProviderEmbedder` (the OpenAI-compatible `/embeddings` API: OpenAI,
  Gemini, Ollama, LM Studio, vLLM) and `FakeEmbedder` for tests

> **Early development.** The API may change. Not yet published to
> crates.io. The gateway (design 0006 §7) is planned, not built.

## Example

```rust
use agentbuild_providers::{FakeModel, FakeReply, Message, Model, ModelRequest};
use serde_json::json;

#[tokio::main(flavor = "current_thread", start_paused = true)]
async fn main() {
    let model = FakeModel::new()
        .then(FakeReply::tool_call("weather", json!({"city": "Paris"})));
    let request = ModelRequest::new(vec![Message::user("Weather in Paris?")]);

    let response = model.invoke(&request).await.unwrap();
    let call = response.message.tool_calls().next().unwrap();
    assert_eq!(call.name, "weather");
    assert_eq!(call.args["city"], "Paris");
}
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

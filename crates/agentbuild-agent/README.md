# agentbuild-agent

Agents for [AgentBuild](https://github.com/VARMAMASTER/agentbuild): a model
with tools, run as a loop on the AgentBuild workflow engine.

- the agent compiles into an ordinary workflow (`model` ⇄ `tools`), so
  streaming, deadlines, cancellation and snapshots work as for any workflow
- tool arguments are validated against the tool's JSON Schema; unknown
  tools, bad arguments, tool errors and timeouts go back to the model
- tool calls run concurrently (bounded) and their results keep call order
- every finished call is saved at once: retrying a failed run never repeats
  a call that already ran (for example, sending an email)
- limits on model calls and tool calls; transient model errors are retried

> **Early development.** Human approval, structured output and hooks are
> designed but not built.

## Example

```rust
use agentbuild_agent::{Agent, ToolError, tool_fn};
use agentbuild_providers::{FakeModel, FakeReply, ToolSpec};
use serde_json::json;

#[tokio::main(flavor = "current_thread", start_paused = true)]
async fn main() {
    // A simulated model; use `ProviderModel` for a real one.
    let model = FakeModel::new()
        .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
        .then(FakeReply::text("It is 21 °C in Paris."));
    let weather = tool_fn(
        ToolSpec::new("weather", "Current temperature", json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        })),
        |args, _ctx| async move {
            Ok::<_, ToolError>(json!({"city": args["city"], "celsius": 21}))
        },
    );

    let agent = Agent::builder(model).tool(weather).build().unwrap();
    let result = agent.run("Weather in Paris?").await.unwrap();
    assert_eq!(result.text(), Some("It is 21 °C in Paris."));
}
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

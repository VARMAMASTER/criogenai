# agentbuild-core

The execution engine of [AgentBuild](https://github.com/VARMAMASTER/agentbuild),
an AI agent orchestration framework. It runs workflow graphs of async nodes in
*supersteps*: the nodes scheduled for a step run concurrently against the same
JSON state, their updates are merged atomically, and edges decide what runs
next.

> **Early development.** The API will change. Python and TypeScript SDKs, LLM
> providers and persistence are planned, not built.

## Features

- static edges, fan-out, joins, async routers, cycles with a step limit
- per-key reducers (`replace`, `append`, `upsert_by_id`)
- nodes that choose their next step (`Command`) or pause the run (`Suspend`)
- JSON snapshots: resume a paused run, or retry a failed one, from any process
- saved task writes and `ctx.memo()`: a retried or resumed step does not
  repeat work that already finished (for example, sending an email)
- live events (`Workflow::stream`)
- per-node timeouts and retries, run deadlines, graceful cancellation
- validation that reports every problem at once; stable error codes

## Example

```rust
use agentbuild_core::{END, State, Update, WorkflowBuilder, node_fn};

#[tokio::main]
async fn main() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("greet", node_fn(|state| async move {
            let name = state.get("name").and_then(|v| v.as_str()).unwrap_or("world");
            Ok(Update::new().set("greeting", format!("hello, {name}")))
        }))
        .set_entry("greet")
        .add_edge("greet", END);
    let workflow = builder.build().expect("valid graph");

    let output = workflow.run(State::new().with("name", "ada")).await.unwrap();
    assert_eq!(output.state.get("greeting").unwrap(), "hello, ada");
}
```

More in [`examples/`](https://github.com/VARMAMASTER/agentbuild/tree/main/crates/agentbuild-core/examples)
and the design documents in
[`docs/design/`](https://github.com/VARMAMASTER/agentbuild/tree/main/docs/design).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

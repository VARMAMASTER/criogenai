# Glossary and naming conventions

The words AgentBuild uses, what each one means, and the naming rules for
code, JSON and docs. Terms marked *(planned)* are designed but not built.

## 1. Workflow engine (`agentbuild-core`)

| Term | Meaning | In code |
|---|---|---|
| **Workflow** | A validated, immutable graph of nodes, ready to run. Cheap to clone; one workflow serves many runs. | `Workflow` |
| **Builder** | Collects nodes, edges and settings; `build()` validates and reports *every* problem at once. | `WorkflowBuilder` |
| **Node** | A named unit of work: reads the state, returns an update. | `Node` trait, `node_fn`, `node_with_context` |
| **Entry** | The node scheduled for the first step. | `set_entry` |
| **Edge** | A fixed link `a → b`: after `a`, schedule `b`. Several edges from one node fan out. | `add_edge` |
| **END** | The special target that ends a path. | `END` (`"__end__"`) |
| **Router** | A function that looks at the merged state and returns a **label**; the label picks the next node. | `Router`, `router_fn`, `add_router` |
| **Join** | Waits until all its source nodes have completed, then schedules its target (fan-in). | `add_join` |
| **Goto** | A node choosing its next node at run time; the targets must be declared up front. | `NodeOutput::Command`, `may_goto` |
| **State** | The run's shared data: one JSON object. Nodes never change it directly. | `State` |
| **Update** | The keys a node wants to change. | `Update` |
| **Reducer** | How writes to one state key combine: `replace` (default), `append`, `upsert_by_id`. | `Reducer` |
| **Run** | One execution of a workflow, from input to completion. Has a `run_id` (time-ordered UUID). | `workflow.run(...)` |
| **Segment** | The part of a run between two pauses. A run that suspends once has two segments; they share the `run_id`. | |
| **Step** (superstep) | One round: every scheduled node runs concurrently on the same state, then all updates merge **atomically**. A failed step commits nothing. | `step` index |
| **Frontier** | The nodes scheduled for the next step. | `Snapshot::frontier` |
| **Task** | One node's execution within one step. | `task_id` = `"{run_id}:{step}:{node}"` |
| **Attempt** | One try of a task. With a retry policy a task can have several attempts. | `ctx.attempt()` |
| **Node context** | What a node receives besides the state: ids, attempt, resume value, events, cancel token, deadline, memo. | `NodeContext` (`ctx`) |
| **Suspend / interrupt** | A node pausing the run to ask for outside input (e.g. a human approval). The run returns the **interrupt** payloads and a snapshot. | `NodeOutput::Suspend`, `Interrupt` |
| **Resume** | Continuing a suspended run with a value for each suspended node. | `workflow.resume(snapshot, values)` |
| **Snapshot** | Everything needed to continue a run later, as plain JSON (`agentbuild.snapshot/1`). Returned on suspend and attached to every failure. | `Snapshot` |
| **Pending writes** | Work already done in the snapshot's step: saved outputs, memo entries, resume values. | `Snapshot::pending_writes` |
| **Task memo** | Results a node saves while running so they are not repeated on retry or resume (e.g. "email already sent"). | `ctx.memo()`, `TaskMemo::once` |
| **Retry (a failed run)** | Continuing from a failure snapshot after fixing the cause; finished nodes are not run again. | `workflow.retry(snapshot)` |
| **Blame rule** | An output that itself caused the failure (conflict, invalid value, bad goto) is not saved, so that node runs again after the fix. | |
| **Fingerprint** | Hash of the graph's structure; a snapshot only resumes on the same structure. | `workflow.fingerprint()` |
| **Policy** | Per-node reliability settings: per-attempt **timeout** and **retry policy** (exponential backoff with jitter, only for errors marked retryable). | `NodePolicy`, `RetryPolicy` |
| **Deadline** | A time limit for a whole `run`/`resume` call. | `RunOptions::deadline` |
| **Cancel token / grace** | Cooperative cancellation; nodes get `cancel_grace` (5 s) to return. | `CancelToken`, `RunOptions::cancel_grace` |
| **Event** | A live progress message (`run_started`, `node_finished`, ...), numbered by `seq`, from `internal` (engine) or `user` (node) origin. | `Event`, `EventKind`, `workflow.stream(...)` |

**Three kinds of "retry"** — keep them apart:

1. **Attempt retry**: a node's `RetryPolicy` re-runs a task after a retryable
   error, inside one step.
2. **Run retry**: `workflow.retry(snapshot)` continues a failed run.
3. **Model retry** *(planned, agent layer)*: re-sending a model request after
   a transient `ModelError`, per 0003 §5.

## 2. Messages, tools and models (`agentbuild-providers`)

| Term | Meaning | In code |
|---|---|---|
| **Message** | One turn of a conversation, the same JSON in every language. | `Message` |
| **Role** | `system`, `user`, `assistant` or `tool`. | `Role` |
| **Content block** | One ordered piece of a message: `text`, `reasoning`, `image`, `audio`, `file`, `json`, `tool_call`, `invalid_tool_call`, `provider`. | `ContentBlock` |
| **Tool** | A function the model may call, described by a name, description and JSON Schema. | `ToolSpec` |
| **Tool call** | The model asking to run a tool, inside an assistant message. Has a `call_id`. | `ContentBlock::ToolCall` |
| **Invalid tool call** | A tool call whose arguments could not be parsed (or were cut off: `truncated`). | `ContentBlock::InvalidToolCall` |
| **Tool result** (tool message) | A `role: tool` message answering one `call_id`, with a **status**: `success`, `error`, `rejected`, `skipped`, `cancelled`. | `Message::tool_result`, `ToolStatus` |
| **Transcript** | The list of messages sent to a model. **Valid** means every tool call has exactly one result before the next model call. | `transcript::check`, `transcript::repair` |
| **Model** | Anything that answers a model request: a real provider, the fake, or a Python/TS model. | `Model` trait |
| **Model request / response** | The provider contract: messages, tools, tool choice, settings → assistant message, usage, finish reason, warnings. | `ModelRequest`, `ModelResponse` |
| **Capabilities** | What a model supports (tools, vision, streaming...). `None` = unknown, never guessed from the model name. | `Capabilities` |
| **Usage** | Token counts; `input_tokens` includes cached tokens. | `Usage` |
| **Finish reason** | Why the model stopped: `stop`, `tool_calls`, `length`, `content_filter`, `error`, `other`. | `FinishReason` |
| **Provider** | The company or server behind a model (`openai`, `ollama`). | `ModelError::provider` |
| **Protocol** | A wire format (`openai_chat`; `anthropic_messages` planned). One protocol serves many providers. | `Protocol` |
| **Endpoint** | Where and how to reach a provider: protocol, base URL, key, headers, quirks. Presets exist. | `Endpoint::ollama()` etc. |
| **Quirks** | The small differences between servers that speak the same protocol. | `OpenAiQuirks` |
| **Provider model** | Endpoint + model name + HTTP transport, as a `Model`. | `ProviderModel` |
| **Transport** | The HTTP layer: timeouts (connect, first byte, idle, total). Never retries. | `Transport` |
| **Model error** | A classified failure with a **kind** (`rate_limited`, `context_overflow`, ...) and a **category**: `transient` (time may fix it) or `provider_fatal`. | `ModelError`, `ModelErrorKind` |
| **Dispatch** | How far a failed request got: `not_sent` (always safe to retry), `sent` (may be billed), `streaming`. | `Dispatch` |
| **Delta** | A piece of a streamed answer (`message_delta`). | `MessageDelta`, `BlockDelta` |
| **Assembler** | Builds the final message from stream items; parses tool arguments once. | `Assembler` |
| **Warning** | Something dropped or repaired, never silently. | `Warning` |
| **Fake model / fake LLM** | Simulated models for tests: 2–3 s replies, scripted tool calls, failures. | `FakeModel`, `FakeLlm` (core tests) |

## 3. Agents (`agentbuild-agent`; design 0004)

| Term | Meaning | In code |
|---|---|---|
| **Agent** | A model plus tools plus instructions, run as a fixed workflow: **model node** ⇄ **tools node** until the model answers without tool calls. | `Agent`, `Agent::builder(model)` |
| **Agent loop** | That cycle. Each model call and each tools round is a step. | |
| **Model node** | Repairs the transcript, checks limits, calls the model, routes. | node `model` |
| **Tools node** | Runs the tool calls of one assistant message concurrently and appends the results in call order. | node `tools` |
| **Tool (Rust)** | A function the agent can call; arguments are validated first. | `Tool`, `tool_fn`, `ToolContext` |
| **Tool output / tool error** | What a tool returns; `ToolError::new` is shown to the model, `ToolError::fatal` fails the run. | `ToolOutput`, `ToolError` |
| **Limits** | Bounds on model calls, tokens, tool calls (per run and per turn), parallel calls, tool timeout, structured-output retries; `on_limit` = fail (`limit_exceeded`) or end gracefully (stop reason `limit`). | `Limits`, `OnLimit`, `AgentResult::limit` |
| **Fallback model** | A model tried when the previous one keeps failing (transient errors, or kinds listed in `fallback_on`); rate limits and overload switch at once. | `AgentBuilder::fallback`, `fallback_on`, event `model_fallback` |
| **Retry-After** | The wait a provider asks for after a rate limit; honored up to `max_retry_after`, longer waits fail or fall back at once. | `ModelError::retry_after`, `AgentBuilder::max_retry_after` |
| **Agent node / agent tool** | An agent inside a workflow (`agent.as_node()`: reads and writes a messages key, can pause the workflow for approval) or used by another agent as a tool (`agent.as_tool(name, description)`, argument `input`); bounded by `Limits::nesting`. | `AgentNode`, `AgentTool`, `ToolContext::depth` |
| **Stop reason** | Why a run ended normally: `final_answer`, `return_direct`, `content_filter`. | `StopReason` |
| **Tool annotations** | `read_only`, `idempotent`, `destructive`; only read-only or idempotent tools are retried. | `ToolAnnotations`, `AgentBuilder::tool_retry` |
| **Artifact / truncation** | App-only data kept in a tool message (never sent to the model); oversized results are cut and kept whole in the artifact. | `ToolOutput::with_artifact`, `max_result_chars`, `metadata.truncated` |
| **Tool state update** | A tool changing agent state; parallel writes are merged in call order (lists joined). | `ToolOutput::with_state` |
| **App context** | Shared services (database, clients) for tools. | `AgentBuilder::context`, `ToolContext::context::<T>()` |
| **Agent stream** | A run in the background with typed events for a UI (text pieces, tool calls, messages, approval needed). | `Agent::stream`, `AgentStream`, `AgentEvent` |
| **Return direct** | A tool whose successful result ends the run and becomes the output. | `ToolSpec::return_direct` |
| **Structured output** | The run must end with JSON matching a schema: through the model's JSON mode (`native`) or the reserved `final_output` tool (`tool`); invalid answers go back to the model, up to `output_retries`. | `AgentBuilder::output`, `FINAL_OUTPUT_TOOL`, stop reason `output`, error `output_invalid` |
| **Agent result / error** | The transcript, output, usage and counts; or a coded failure (`model_failed`, `tool_failed`, `limit_exceeded`) with a snapshot to retry. | `AgentResult`, `AgentError` |
| **Hook / middleware** | Code that runs around the loop: `on_start`, `before_model`, `wrap_model`, `after_model`, `wrap_tool`, `on_end`; onion order (first hook outermost). | `Hook`, `HookAction`, `HookContext`, `ModelNext`, `ToolNext`, stop reason `hook_end`, error `hook_failed` |
| **Approval / review** | A person approving tool calls before they run; the `review` node suspends the run with the requests. | `Approval`, `ApprovalRule`, node `review` |
| **Approval request** | One gated call waiting for a decision: call id, tool, arguments, description, allowed decisions. | `ApprovalRequest`, `AgentResult::approval_requests` |
| **Decision** | `approve`, `edit` (run changed arguments), `reject` (not run; model told why), `respond` (not run; reviewer's answer is the result). | `Decision`, `DecisionKind`, `Agent::resume` |
| **Thread** | A persisted conversation that runs continue (with a **checkpointer**, design 0008). |
| **Gateway** | The HTTP server that exposes the provider layer to other apps (design 0006 §7). |

## 4. Observability (`agentbuild-telemetry`; design 0009)

| Term | Meaning | In code |
|---|---|---|
| **Observer** | Receives every event of a run as it happens, without blocking it. | `EventObserver`, `RunOptions::observe` |
| **Run metadata** | JSON attached to a run (`user_id`, `session_id`, `tags`, `name`, ...), reported in `run_started`. | `RunOptions::metadata` |
| **Exporter** | Turns events into spans and sends them in the background. | `OtlpExporter` |
| **Trace / span** | OpenTelemetry terms: a trace is one run (trace id = run id); spans are its parts (nodes, model calls, tool calls). | |
| **Observation** | Langfuse's word for a span; types used: `agent`/`chain` (root), `span` (node), `generation` (model call), `tool`, `event`. | |
| **Content capture / mask** | Whether messages, arguments and results are exported, and a function that rewrites them first. | `OtlpConfig::capture_content`, `OtlpConfig::mask` |

## 5. Naming conventions

### Code (Rust)

- Crates: `agentbuild-<area>` (`agentbuild-core`, `agentbuild-providers`;
  later `agentbuild-gateway`, `agentbuild-py`, `agentbuild-node`).
- Types are nouns in `UpperCamelCase` (`RunOptions`, `ToolSpec`); functions
  and methods are verbs in `snake_case` (`run`, `resume`, `classify_http`).
- Constructors: `new(...)` for the plain case, named constructors for
  variants (`Message::user`, `Endpoint::ollama`, `ContentBlock::tool_call`).
- Builders: `with_<field>(value)` returns `Self` (`with_tools`,
  `with_dispatch`); option structs use the field name directly
  (`RunOptions::default().deadline(..).max_steps(..)`).
- `<verb>_with(…, options)` is the explicit-options form of `<verb>(…)`:
  `run`/`run_with`, `resume`/`resume_with`, `retry`/`retry_with`,
  `stream`/`stream_with`.
- Booleans read as questions: `is_suspended`, `is_transient`,
  `emitted_deltas`.
- Public enums and structs that may grow are `#[non_exhaustive]`; everything
  public has docs and implements `Debug`.

### JSON, events and errors

- All JSON field names are `snake_case` (`call_id`, `finish_reason`,
  `input_tokens`). Enum values are `snake_case` strings (`tool_calls`,
  `not_sent`).
- Tagged unions use `type` (`{"type": "text", ...}`); media sources use
  `kind`; transcript changes use `change`.
- **Error codes** are stable `snake_case` identifiers: run errors
  (`node_failed`, `deadline_exceeded`), model error kinds (`rate_limited`),
  tool-result error codes (`invalid_arguments`), warning codes
  (`reasoning_dropped`). Codes are added, never renamed.
- **Event types** are `snake_case`, `<subject>_<past-tense verb>`:
  `run_started`, `node_finished`, `step_committed`, `attempt_failed`.
- **Ids:** `run_id` is a UUIDv7; message ids `msg_<uuid>`; generated call ids
  `call_<uuid>`; repair results `msg_repair_<call_id>`; task ids
  `"{run_id}:{step}:{node}"`.
- Reserved names: node names starting with `__` (`__end__`); the tool name
  `final_output`.
- Formats are versioned strings: `agentbuild.snapshot/1`.

### Repository and process

- Design docs: `docs/design/NNNN-title.md`, numbered in order; behaviour
  changes update a design doc first.
- Tests are named as sentences that state the behaviour
  (`finished_nodes_are_not_rerun_when_a_failed_step_is_retried`).
- Commits: Conventional Commits (`feat(core): …`, `fix(providers): …`),
  `!` for breaking changes.

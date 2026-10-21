# 0017: The rest of the Rust API in the SDKs

Status: **Accepted** (the user asked to build every feature the status
page lists as planned for Python and TypeScript: roadmap tasks 13–17).
Builds on [0011](0011-python-sdk.md) and [0012](0012-typescript-sdk.md);
workflows are [0015](0015-sdk-workflows.md). Engine semantics stay in
Rust (AGENTS.md rule 3): every feature here is configuration for, or a
callback from, something the Rust agent already does.

## 1. Agents as tools

```python
researcher = Agent(model, instructions="Find facts.", tools=[search])
writer = Agent(model, tools=[researcher.as_tool("research", "Looks things up")])
```

```ts
const writer = new Agent({ model, tools: [researcher.asTool("research", "Looks things up")] });
```

`as_tool` / `asTool` wraps Rust's `Agent::as_tool` (0004): the tool takes
`{"input": string}`, runs the sub-agent, returns its answer; nesting is
bounded by `Limits::nesting`. Agents as workflow steps are 0015
(`wf.add_node(name, agent)`).

## 2. Retries and fallback models

```python
Agent(model,
      fallbacks=[Anthropic("claude-haiku-5-5"), Ollama("llama3.2")],
      fallback_on=["context_overflow"],
      model_retry=Retry(max_attempts=4, initial_backoff=0.5, factor=2, max_backoff=20, jitter=True),
      tool_retry=Retry(max_attempts=2),
      max_retry_after=30)
```

TypeScript: `fallbacks`, `fallbackOn`, `modelRetry`, `toolRetry`,
`maxRetryAfterSecs` (a `Retry` is a plain object `{maxAttempts,
initialBackoffSecs, factor, maxBackoffSecs, jitter}`). They map one to one
onto `AgentBuilder::fallback`, `fallback_on`, `model_retry`, `tool_retry`
and `max_retry_after`; the defaults are Rust's.

## 3. Long-term memory by meaning

```python
memory = Memory.by_state("user_id", "users").semantic(Embedder.openai("text-embedding-3-small"))
```

`Embedder` mirrors the model constructors (`openai`, `openai_compatible`
with a base URL, `ollama`, `fake()` for tests) and builds a
`ProviderEmbedder` (or `FakeEmbedder`) in Rust; `Memory.semantic(...)`
calls `Memory::semantic`. Search falls back to keywords when the embedder
fails (0008 §16). Keys come from the environment, as for models.

## 4. Tracing

```python
Agent(model, telemetry=Telemetry.langfuse())                        # LANGFUSE_* variables
Agent(model, telemetry=Telemetry.otlp("http://localhost:4318", headers={...}, capture_content=False))
```

Builds an `OtlpConfig` (`Langfuse::from_env` / `OtlpConfig::new`) and an
exporter once per agent; every run of the agent (and its threads) gets it
through `RunOptions::observe`. `capture_content` (default off) and masks
follow 0009. TypeScript: `Telemetry.langfuse()`, `Telemetry.otlp(url,
{ headers, captureContent })`.

## 5. Hooks

A hook is an object with any of these methods (sync or async):

| Method | When | It may return |
|---|---|---|
| `on_start(ctx)` | once per run segment | `HookAction(state=…)` |
| `before_model(ctx)` | before each model call | `HookAction(messages=[…], state=…, end=…)` |
| `after_model(ctx)` | after each model reply | `HookAction(message={…}, state=…, end=…)` |
| `before_tool(call)` | before each tool call | a changed `ToolCall` (name, args), or `ToolResult(value)` to skip the tool, or raise `ToolError` |
| `after_tool(call, result)` | after each tool call | a replacement result |
| `on_end(ctx, outcome)` | always, once per segment | nothing |

`ctx` has `run_id`, `messages`, `model_calls`, `hook_state` and `state`.
`before_tool` / `after_tool` are the SDKs' form of Rust's `wrap_tool`
(the bridge calls them around `next`); Rust's `wrap_model` has no SDK form
in this slice (retries and fallbacks cover its main uses). Order and
semantics are Rust's (0004 §3): `on_start`/`before_model`/`before_tool` run
in list order, the others in reverse. An exception in a hook fails the
run with `hook_failed`, the exception as its cause.

TypeScript: the same names in camelCase (`onStart`, `beforeModel`, …).

## 6. TypeScript tool input from Zod and other Standard Schema libraries

`tool({ input })` accepts a JSON Schema (as today) or a Standard Schema
object (`~standard`, version 1: Zod 4, Valibot, ArkType). Its JSON Schema
comes from `~standard.jsonSchema.input()` when the library provides it
(Standard JSON Schema), else from `z.toJSONSchema` for Zod 4 (found on the
schema object); without one, `tool` throws asking for an explicit
`jsonSchema`. The engine validates arguments with that JSON Schema (rule 3);
the library's own `~standard.validate` then runs too, for what JSON Schema
cannot say (refinements, transforms), and its issues go back to the model
as an `invalid_arguments` error; the tool receives the library's output
(e.g. transformed values). Types: `tool<z.infer<typeof schema>>` is
inferred.

## 7. Tests

Each feature in both SDKs, with simulated models only: a sub-agent tool
answers; a failing model falls back (and retries with `fail_first`); a
semantic memory recalls by meaning with the fake embedder; telemetry
exports spans to a local test endpoint; each hook method changes what it
may; a Zod schema validates, transforms and reports issues to the model.
Package tests and consumers extended; the status page updated per row.

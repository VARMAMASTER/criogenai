# Research: LangChain 1.x agent layer

Status: research note (input to Phase 4 agent-layer design). Not a decision record.

Studied: `langchain 1.4.4`, `langgraph 1.2.14`, `langgraph-prebuilt 1.1.0`,
`langchain-core 1.6.9` (installed under `research/.venv/Lib/site-packages`).
Everything below was read from that source, not from docs or memory. Mechanisms
are paraphrased; no code is reproduced.

**Citation legend** (paths relative to `site-packages/`):

| short form        | file                                                  |
|-------------------|-------------------------------------------------------|
| `factory.py`      | `langchain/agents/factory.py`                         |
| `types.py`        | `langchain/agents/middleware/types.py`                |
| `so.py`           | `langchain/agents/structured_output.py`               |
| `mw/<name>.py`    | `langchain/agents/middleware/<name>.py`               |
| `chat_base.py`    | `langchain/chat_models/base.py`                       |
| `tool_node.py`    | `langgraph/prebuilt/tool_node.py`                     |
| `react.py`        | `langgraph/prebuilt/chat_agent_executor.py`           |
| `lg_types.py`     | `langgraph/types.py`                                  |

---

## 0. What is in the 1.x `langchain` package

The 1.x package is small (16.7k lines, 40 files) and is now mainly an agent package.
The classic chains, retrievers and community integrations moved to
`langchain-classic` (`langchain/embeddings/__init__.py:1-9` points users there).

| module | contents |
|---|---|
| `agents/` | `create_agent` (`factory.py`, 2.2k lines), `AgentState`, structured output strategies (`so.py`), `SubagentTransformer` (streaming handle for nested agents) |
| `agents/middleware/` | the middleware base class and decorators (`types.py`, 2.2k lines) plus 15 built-in middlewares (§2.6) |
| `chat_models/` | `init_chat_model` and the `_ConfigurableModel` runtime-swappable wrapper (`chat_base.py`) |
| `embeddings/` | `init_embeddings` (mirror of `init_chat_model`) |
| `tools/` | re-exports `tool`, `BaseTool`, `InjectedToolArg`, `InjectedToolCallId`, `ToolException` from core; `InjectedState`, `InjectedStore`, `ToolRuntime` from langgraph-prebuilt (`langchain/tools/tool_node.py:1-20`) |
| `messages/` | re-exports core message and content-block types |
| `rate_limiters/` | re-exports `InMemoryRateLimiter` from core |
| `mcp/` | **beta** MCP adapter (`MCPAdapter`, `as_langchain_tool`) plus interrupt-driven elicitation types. It warns when imported (`langchain/mcp/__init__.py`) |

Runtime dependencies are `langchain-core`, `langgraph` (<1.3) and `pydantic`. Provider
packages are optional extras (`langchain-1.4.4.dist-info/METADATA`).
`langsmith` gets pulled in anyway: `factory.py:37` imports `traceable` unconditionally.

Size of the stack an agent sits on: langchain 16.7k lines, langgraph 38k,
langchain-core 70k, langsmith 93k, langgraph-sdk 18.8k.

---

## 1. `create_agent`

### 1.1 Signature (`factory.py:892-911`)

`create_agent(model, tools=None, *, system_prompt, middleware=(), response_format,
state_schema, context_schema, checkpointer, store, interrupt_before, interrupt_after,
debug, name, cache, transformers)` returns a **`CompiledStateGraph`**. Three
`@overload`s (`factory.py:823-889`) only refine the type of `structured_response`.

- `model`: a `"provider:model"` string goes through `init_chat_model` (`factory.py:1047-1048`), or pass a `BaseChatModel`. A per-call model choice can only be made through middleware (`request.override(model=...)`). The deprecated `create_react_agent` accepted a callable for this (`react.py:278-289`); `create_agent` dropped it.
- `tools`: a mix of `BaseTool`, plain callables and **dicts**. A dict is treated as a provider-side "built-in" tool. It is bound to the model and never executed locally (`factory.py:1131-1136`).
- `system_prompt`: a `str` or `SystemMessage`. It is prepended at call time and never stored in state (`factory.py:1502-1504`).
- `state_schema`: a `TypedDict` extending `AgentState`. `context_schema`: the type of the immutable per-run `runtime.context` object, used for dependency injection.
- `checkpointer` / `store`: thread-scoped persistence and cross-thread KV storage. Both are passed straight to LangGraph.
- `interrupt_before/after`: LangGraph static breakpoints. They take **generated node names** such as `"HumanInTheLoopMiddleware.after_model"`.
- `name`: becomes `AIMessage.name` (`factory.py:1507-1508`) and the `lc_agent_name` metadata used to detect subagents (`factory.py:1901-1902`).
- `transformers`: stream-event transformer factories (§5, §7).
- **There is no `max_iterations` option.** See §1.5.

### 1.2 Graph construction

The build runs in this order (`factory.py:1046-1925`):

1. **Normalise response_format.** A raw schema is wrapped in `AutoStrategy`. A `ToolStrategy` is precomputed so that the synthetic output tools exist at build time (`factory.py:1062-1088`).
2. **Collect tools.** Middleware-declared `m.tools` plus user tools go into a single `ToolNode`. The node is created only if there is at least one client-side tool or some middleware overrides `wrap_tool_call` (`factory.py:1089-1148`).
3. **Detect hooks by override.** For each hook, the factory checks whether the subclass's method differs from the one on the `AgentMiddleware` base (`factory.py:1094-1204`). Middleware names must be unique (`factory.py:1160-1162`). Duplicate classes therefore need a custom `name` property, e.g. `mw/tool_call_limit.py:258`.
4. **Compose wrappers.** All `wrap_model_call` handlers and all `wrap_tool_call` handlers are folded into one onion each. Every layer is wrapped in `langsmith.traceable` (`factory.py:1101-1127, 1206-1224`).
5. **Merge state schemas.** The middleware `state_schema`s are combined with the base `AgentState` into one dynamically created `TypedDict`. The base is merged last, so it wins on conflicts. Separate input and output schemas are derived by dropping fields annotated `OmitFromInput`, `OmitFromOutput` or `PrivateStateAttr` (`factory.py:1226-1234, 470-523`; annotations at `types.py:328-346`).
6. **Add nodes.** `"model"`, `"tools"`, and **one graph node per overridden `before_agent` / `before_model` / `after_model` / `after_agent` hook**, named `"{middleware.name}.{hook}"` (`factory.py:1611-1713`).
7. **Wire edges.** Four anchors are computed (`factory.py:1715-1741`):
   - `entry`: the first `before_agent`, else the first `before_model`, else `model`
   - `loop_entry`: the first `before_model`, else `model`. Tools loop back here.
   - `loop_exit`: the **first** middleware's `after_model`. The after-hooks run in reverse order, so this one runs last.
   - `exit`: the last-registered `after_agent`, else `END`.

   Middleware nodes are chained with plain edges, or with a conditional edge when the hook declared `can_jump_to` (`factory.py:2114-2159`).
8. **Compile.** The graph is compiled with `recursion_limit` **hard-coded to 9 999** and `with_config` metadata attached (`factory.py:1897-1925`).

Running a probe with a fake model (`create_agent(model, [echo])`) gives 4 nodes:
`__start__ → model ⇄ tools`, plus `model → __end__`. Adding five middlewares
(call-limit, todo, tool-retry, HITL, summarization) gives **9 nodes and 11 edges**:

```
__start__ → ModelCallLimit.before_model → Summarization.before_model → model
model → HITL.after_model → TodoList.after_model → ModelCallLimit.after_model
ModelCallLimit.after_model ⇢ {tools, ModelCallLimit.before_model, __end__}
tools ⇢ ModelCallLimit.before_model
```

`ToolRetryMiddleware` adds no node because it only implements `wrap_tool_call`.

### 1.3 The model↔tools loop

**Model node** (`factory.py:1520-1549`, async twin at `1579-1608`):

1. It repairs the history first. Any `invalid_tool_calls` (unparseable arguments) without a matching `ToolMessage` get a synthetic error `ToolMessage` (`factory.py:676-707`).
2. It builds a `ModelRequest` from the model, tools, system message, response_format, messages, state and runtime (`types.py:87-156`).
3. It runs the composed `wrap_model_call` onion around `_execute_model_sync` (`factory.py:1493-1518`). That function binds the tools (`_get_bound_model`, `factory.py:1350-1491`), calls `invoke`, and post-processes structured output.
4. It returns a **list of `Command`s** (`_build_commands`, `factory.py:222-278`). If the history was repaired, the update is `RemoveMessage(REMOVE_ALL_MESSAGES)` followed by the full rewritten history (`factory.py:249-254`).

**Router after the model** (`_make_model_to_tools_edge`, `factory.py:1991-2042`). Checks run in this order:

1. If state contains `jump_to`, go there.
2. If there is no `AIMessage`, exit.
3. If the last AI message has no `tool_calls`, exit. This is the classic termination condition.
4. Pending calls (no `ToolMessage` yet, not a structured-output tool) produce **one `Send("tools", [tool_call])` per call** (`factory.py:2031-2032`).
5. If a `structured_response` was set, exit.
6. Otherwise go back to the model. This covers the case where middleware injected synthetic ToolMessages for every call.

**Router after tools** (`_make_tools_to_model_edge`, `factory.py:2072-2111`). It exits if every executed tool has `return_direct=True` (this respects HITL redirects via the `ToolMessage.name`), or if a structured-output tool succeeded. Otherwise it returns to `loop_entry`.

Subtlety: an AI message whose calls are **all** invalid has empty `tool_calls`, so the
run **ends** with that malformed message (`factory.py:2018-2019`). The repair from
step 1 only happens on the next model call, i.e. the next user turn on that thread.

### 1.4 Parallel tool calls

Parallelism comes from LangGraph rather than from ToolNode. Each tool call is its own `Send`, so
each call becomes a separate Pregel task in the same superstep. Each task invokes `ToolNode` with a
one-element list. The tool then gets the state by reading the channels through Pregel's
private `CONFIG_KEY_READ` partial (`tool_node.py:1298-1312`). An older design inlined the
full state into every `Send`; the comment at `factory.py:2027-2030` says that was O(N²).
On its own, `ToolNode` would also parallelise: a thread pool `executor.map` on the sync
path (`tool_node.py:821-824`) and `asyncio.gather` on the async path (`tool_node.py:858`).

### 1.5 Iteration limits

- `create_agent` has no step or iteration cap of its own. It sets `recursion_limit=9_999`, citing langgraph issue #7313 (`factory.py:1897-1899`). LangGraph's default is 10 007 (`langgraph/_internal/_config.py:32`).
- Every loop costs at least 2 supersteps, plus one per before_model/after_model middleware node. With the default limit, runaway loops are effectively unbounded.
- Bounding the loop is done with middleware: `ModelCallLimitMiddleware` / `ToolCallLimitMiddleware` (§2.6).
- The deprecated `create_react_agent` used a `remaining_steps` managed value and returned "Sorry, need more steps" (`react.py:62, 74, 432-439, 627`). That is gone in 1.x.

### 1.6 Structured output

`response_format` accepts `ToolStrategy`, `ProviderStrategy`, a raw schema, or a JSON-schema
dict (`factory.py:898`).

- **ToolStrategy** (`so.py:196-267`). Each schema becomes a synthetic tool. A union, or a JSON `oneOf`, becomes several tools. The model is bound with `tool_choice="any"` (`factory.py:1474-1481`), so it **must** call some tool on every turn. Calling an output tool ends the loop. Parse failures and calls to several output tools are handled by `handle_errors`: `True`, a string, an exception type or tuple, or a callable (`so.py:210-231`, `factory.py:646-673`). A handled failure appends an error `ToolMessage` and the model is called again (`factory.py:1280-1346`). **Raw dict schemas are not validated at all** (`so.py:223-230`).
- **ProviderStrategy** (`so.py:270-314`). It binds a provider-native `response_format`. The kwargs are always built in **OpenAI's `json_schema` shape**, and each provider integration has to translate them (`so.py:295-314`). For OpenAI-compatible models it also sets `strict=True` as a backward-compatibility workaround (`factory.py:1443-1456`). A parse failure raises `StructuredOutputValidationError` and is **never retried** (`factory.py:1262-1269`).
- **AutoStrategy** (raw schema) is resolved on every model call. ProviderStrategy is chosen if the model's `profile["structured_output"]` is true, or if the model name matches a **hard-coded regex list of model names** (`factory.py:182-203, 578-623`). That list includes a special case for Gemini < 3 together with tools.
- The result is stored in `state["structured_response"]`. When no result was produced, it is explicitly cleared so that a value from an earlier checkpointed turn does not leak through (`factory.py:257-260`).

### 1.7 Error handling

| failure | default behaviour | where |
|---|---|---|
| model raises | the exception propagates and the run fails. Recovery is opt-in (`ModelRetryMiddleware`, `ModelFallbackMiddleware`) | `factory.py:1506` |
| tool argument validation (pydantic) | becomes an error `ToolMessage`, with errors on injected args filtered out | `tool_node.py:956-966, 383-391` |
| model calls an unknown tool | error `ToolMessage` listing the available tools | `tool_node.py:1268-1279` |
| tool raises any other exception | **re-raised**. The default handler converts only `ToolInvocationError` | `tool_node.py:383-391, 984-1012` |
| `interrupt()` / `GraphBubbleUp` | always re-raised. Every catch-all in the stack has to remember this | `tool_node.py:982-983`, `mw/model_fallback.py`, `mw/model_retry.py:244`, `mw/tool_error.py:154` |
| middleware adds a tool that ToolNode does not know about | `ValueError` with a long how-to, unless some `wrap_tool_call` exists | `factory.py:127-151, 1369-1397` |
| `ModelRetryMiddleware` runs out of retries | default `on_failure="continue"` **turns the exception into an `AIMessage`** and the run continues | `mw/model_retry.py:116-125, 196-219` |

`ToolNode(handle_tool_errors=...)` takes a bool, a string, an exception type or tuple, or a
callable. For a callable, the handled exception types are inferred from its type
annotation (`tool_node.py:444-509`). `create_agent` does not expose this parameter. To
change the policy you add `ToolErrorMiddleware` or `ToolRetryMiddleware`.

---

## 2. Middleware

### 2.1 Hook points (verified names, `types.py:385-823`)

| hook | kind | signature (paraphrased) | graph node? |
|---|---|---|---|
| `before_agent` / `abefore_agent` | node | (state, runtime) → update dict or None | yes, runs once per invocation |
| `before_model` / `abefore_model` | node | (state, runtime) → update | yes, runs every iteration |
| `after_model` / `aafter_model` | node | (state, runtime) → update | yes, runs every iteration |
| `after_agent` / `aafter_agent` | node | (state, runtime) → update | yes, runs once |
| `wrap_model_call` / `awrap_model_call` | interceptor | (ModelRequest, handler) → ModelResponse, AIMessage or ExtendedModelResponse | no, runs inside the `model` node |
| `wrap_tool_call` / `awrap_tool_call` | interceptor | (ToolCallRequest, handler) → ToolMessage or Command | no, runs inside the `tools` task |

Class attributes: `state_schema` (`types.py:397`), `tools` (`400`), `trace_policy` (`403`),
`transformers` (`413`), and `name` (`424-429`, defaults to the class name).

**Function decorators** (`types.py:934-2200`): `@before_agent`, `@before_model`,
`@after_model`, `@after_agent`, `@wrap_model_call`, `@wrap_tool_call`, and
`@dynamic_prompt`. Each decorator builds a one-off subclass with `type(...)`
(e.g. `types.py:1104-1115`). `@dynamic_prompt` is a `wrap_model_call` that swaps in a
new `system_message` (`types.py:1761-1838`). `@hook_config(can_jump_to=[...])` attaches
jump metadata to methods (`types.py:879-931`).

Every sync/async hook pair is duplicated. If a user implements only one side and the
agent is run through the other entry point, the base class raises `NotImplementedError` at
runtime (`types.py:586-596, 638-648, 744-754, 813-823`). Node hooks get some protection
from `RunnableCallable`, which falls back to running a sync function in an executor.

### 2.2 Ordering and composition

- **`before_*` hooks run in list order.** **`after_*` hooks run in reverse list order.** The model connects to the *last* middleware's `after_model`, which chains back to the first (`factory.py:1855-1868`). The same holds for `after_agent` (`factory.py:1872-1895`).
- **`wrap_*` hooks form an onion with the first middleware outermost.** The model onion is composed right to left (`factory.py:281-370`), and so is the tool onion (`factory.py:710-755`). A handler may call `handler()` zero times (short-circuit, cache), once, or several times (retry, fallback).
- A `wrap_model_call` can also return `ExtendedModelResponse(model_response, command)`. Commands from every layer are collected inner-first and applied as **extra state updates** after the model output. `goto`, `resume` and `graph` on those commands raise `NotImplementedError` (`types.py:290-325`, `factory.py:262-274`). The accumulator is cleared each time the inner handler is called, so retries do not duplicate commands (`factory.py:344-347`).
- `ModelRequest` and `ToolCallRequest` are treated as immutable through `.override(**changes)` (`types.py:203-269`, `tool_node.py:170-199`). Assigning attributes directly still works but issues a deprecation warning (`types.py:169-201`, `tool_node.py:151-168`). A request can override the model, messages, system_message, tools, tool_choice, response_format, model_settings and state. A tool request can override tool_call, tool and state.

### 2.3 Adding state

A middleware declares `state_schema = MyState(AgentState)`. Its fields are merged into the
graph schema (§1.2 step 5). LangGraph channel annotations select the semantics:

- the `add_messages` reducer on `messages` (`types.py:352`)
- `EphemeralValue` for `jump_to`, which is cleared after one step (`types.py:353`)
- `UntrackedValue` for state that must not be checkpointed, e.g. per-run counters in `mw/model_call_limit.py:33-34` or **live shell process handles** in `mw/shell_tool.py:107-109`
- `PrivateStateAttr` to hide a field from the input and output schemas

Because thread-scoped and run-scoped state are separated only by the channel type, "run
limit" vs "thread limit" in the call-limit middlewares is just `UntrackedValue` vs a normal
channel.

### 2.4 Adding tools

- **Static:** set `self.tools = [...]`. These go into the ToolNode at build time (`factory.py:1089, 1136`).
- **Dynamic:** in `wrap_model_call`, change `request.tools` with `override(tools=...)`, which affects only the binding for that call. Adding a tool that ToolNode does not know about also requires a `wrap_tool_call` that executes it, for example with `request.override(tool=...)`. Otherwise the factory raises (`factory.py:1369-1397`). `ToolNode` computes injection metadata for such tools on the fly (`tool_node.py:1354-1359`).

### 2.5 Jumping (`jump_to`)

`JumpTo` is the literal type `"tools" | "model" | "end"` (`types.py:68`). A node hook
returns `{"jump_to": ...}`. The router that follows it resolves `"model"` to **`loop_entry`**,
which is the first `before_model` and not necessarily the model node, and resolves `"end"`
to the exit anchor (`factory.py:1955-1967`).

The allowed destinations have to be declared **statically** with `can_jump_to`, because
LangGraph needs the conditional edge's possible targets at compile time
(`factory.py:541-575, 2135-2156`). A jump that was not declared is silently ignored,
because `_add_middleware_edge` builds a plain edge when there is no `can_jump_to`. The
one exception is the loop-exit `after_model`: its router always reads `jump_to`
(`factory.py:2001`). Interceptor hooks cannot jump (§2.2).

### 2.6 Built-in middleware (`langchain/agents/middleware/__init__.py`)

| middleware | hooks | what it does |
|---|---|---|
| `SummarizationMiddleware` (`mw/summarization.py:240`) | before_model | Counts tokens over the **whole history on every step** (`:425`). When an OR-of-AND trigger fires (messages, tokens or fraction of the context window), it summarises the older messages with a second model. It then **rewrites persisted state** to `RemoveMessage(ALL)` + summary + the most recent messages, keeping AI/tool pairs together (`:441`). Internal calls retry 3 times and are tagged so they stay out of the stream (§7). |
| `ContextEditingMiddleware` (`mw/context_editing.py:187`) | wrap_model_call | Changes only the *request*, never the state. It clears old tool results above a token threshold (`ClearToolUsesEdit`, modelled on Anthropic's `clear_tool_uses`). It **deep-copies the full message list on every call** (`:270`). |
| `HumanInTheLoopMiddleware` (`mw/human_in_the_loop.py:240`) | after_model + wrap_tool_call | Pauses in a single batched `interrupt` for the tool calls that match its rules, then applies approve, edit, reject or respond decisions. See §6. |
| `ModelCallLimitMiddleware` (`mw/model_call_limit.py:94`) | before_model (jump end) + after_model | Thread and run counters. On the limit it either jumps to end with an AI message or raises (`:166-208`). |
| `ToolCallLimitMiddleware` (`mw/tool_call_limit.py:141`) | after_model (jump end) | Counts per tool or globally. `continue` returns error ToolMessages for the blocked calls; `end` also writes "skipped" messages for the other pending calls so the history stays valid; `error` raises (`:325-471`). |
| `ModelFallbackMiddleware` (`mw/model_fallback.py:342`) | wrap_model_call | Tries the primary model, then each fallback, stripping provider-specific cache settings before each fallback attempt. Re-raises the last error. |
| `ModelRetryMiddleware` (`mw/model_retry.py:33`) | wrap_model_call | Exponential backoff with ±25% jitter (`mw/_retry.py:98-136`). Respects `ModelError.is_retryable`. On exhaustion it returns an AI error message by default. Its sync path calls **blocking `time.sleep`** (`:265`). |
| `ToolRetryMiddleware` (`mw/tool_retry.py:32`) | wrap_tool_call | Same backoff policy, per tool. Still carries deprecated `on_failure` aliases (`:197-211`). |
| `ToolErrorMiddleware` (`mw/tool_error.py:37`) | wrap_tool_call | Opt-in conversion of execution exceptions to error ToolMessages. The callback returns `None` to re-raise. Never sees validation errors or interrupts. |
| `TodoListMiddleware` (`mw/todo.py:174`) | wrap_model_call + after_model + tool | Adds a `write_todos` tool that returns a `Command` updating `state["todos"]` (`:144-157`), appends planning guidance to the system prompt, and rejects parallel `write_todos` calls. |
| `PIIMiddleware` (`mw/pii.py:492`) | before_model + after_model + stream transformer | Regex/Luhn detection (email, card, IP, MAC, URL, custom). Strategies are block, redact, mask or hash, applied to input, output and/or tool results. It needs a **stream transformer** that redacts deltas in flight with a sliding buffer so that the stream matches state (`:50-75`). |
| `LLMToolSelectorMiddleware` (`mw/tool_selection.py:123`) | wrap_model_call | An extra LLM call picks up to N relevant tools before the main call. |
| `LLMToolEmulator` (`mw/tool_emulator.py:29`) | wrap_tool_call | An LLM fakes the results of selected tools (for testing). |
| `ShellToolMiddleware` (`mw/shell_tool.py:518`) | before_agent/after_agent + tool | A persistent shell session with Host, Codex-sandbox or Docker policies. The session is stored in an `UntrackedValue` and cleaned up in `after_agent`, which does **not** run on error or interrupt; a `weakref.finalize` backstop covers that (`:88-91, 657-696`). |
| `FilesystemFileSearchMiddleware` (`mw/file_search.py:108`) | tools | Glob and grep tools (ripgrep, with a Python fallback). |
| `ProviderToolSearchMiddleware` (`mw/provider_tool_search.py:57`) | wrap_model_call | Marks tools `defer_loading` and injects the provider's server-side tool-search tool (Anthropic and OpenAI only). |

---

## 3. ToolNode and tool execution (`tool_node.py`)

- **Construction** (`:743-786`). Callables are converted to `BaseTool`. Injection metadata (`_InjectedArgs`) is computed once per tool.
- **Accepted inputs** (`:1224-1266`):
  - a message list
  - a state dict or object containing `messages` (the tool calls come from the last `AIMessage`)
  - a bare list of tool calls (the new `Send` payload)
  - the legacy `ToolCallWithContext` dict (`__type="tool_call_with_context"`, carries inlined state)

  This polymorphism exists for backward compatibility. It is why the outputs are shaped as a list, a dict, or Commands (`:862-920`).
- **Per call** (`_run_one`, `:1014-1067`). A `ToolRuntime` is built with: state, context, config, store, stream_writer, `tool_call_id`, the list of tools, execution_info and server_info (`:1662-1731`). The `wrap_tool_call` onion is run around `_execute_tool_sync` (`:922-1012`). That function **injects the arguments right before invoking the tool** and normalises the result.
- **Injection** (`:1315-1430`):
  - `Annotated[T, InjectedState]` gets the whole state; `InjectedState("field")` gets one field (`:1753-1827`).
  - `InjectedStore()` gets the store; it raises if the graph was compiled without one (`:1407-1415`).
  - A parameter typed `ToolRuntime` is injected without needing `Annotated`.
  - `InjectedToolCallId` and `InjectedToolArg` come from core.

  Injected parameters are left out of the schema the model sees. **Values the model supplies for injected keys are stripped**, which prevents an LLM from forging hidden arguments (`:1421-1429`).
- **Command returns** (`:1432-1579`). A tool may return:
  - a `ToolMessage`
  - a `Command`, whose `update` must contain a `ToolMessage` with the matching `tool_call_id` unless it targets the parent graph (`:1557-1578`)
  - a list of Commands and ToolMessages with **exactly one** terminating ToolMessage (`:1455-1501`)

  Any other return type raises `TypeError`. `Command(graph=PARENT, goto=[Send...])` from several tools are **merged into one parent command** (`:895-919`). That merge is the mechanism behind handoffs.
- **Streaming from tools.** `runtime.stream_writer` emits custom events. `ToolRuntime.emit_output_delta` streams partial tool output on the `tools` channel (`:1732-1750`).
- **`tools_condition`** (`:1582-1659`) is still exported for hand-built graphs: it routes to `"tools"` when the last message has tool calls, otherwise to `"__end__"`. `create_agent` uses its own richer router (§1.3).
- `ValidationNode` (`langgraph/prebuilt/tool_validator.py`) is also exported. `create_react_agent` is still present, but it is **deprecated** in favour of `create_agent` (`react.py:274-277`).

---

## 4. `init_chat_model` and the provider abstraction (`chat_base.py`)

- **Registry.** `_BUILTIN_PROVIDERS` maps 30 provider keys to (module, class name, constructor). Examples: `anthropic`, `openai`, `azure_openai`, `bedrock_converse`, `google_genai`, `ollama`, `groq`, `xai`, `openrouter`, `litellm`, `langsmith` (`:56-99`). Each one points to a **separate pip package** such as `langchain-openai`. The module is imported lazily with an "install X" error message (`:125-149`), and the constructor is cached (`:152-193`).
- **Parsing `"provider:model"`** (`:623-651`). The prefix is only split off if it is a known provider key. Without a prefix, the provider is **guessed from the model-name prefix** (`gpt-` → openai, `claude` → anthropic, `gemini` → vertexai, and so on). The Gemini guess carries a deprecation warning that its default will change (`:547-620`).
- **Doc/code mismatch.** The registry docstring says providers missing from the dict still work if their package is installed (`:111-118`). The code instead raises `ValueError("Unsupported provider")` for any provider not in the dict (`:173-176`).
- **Configurable models.** When `configurable_fields` is set, or `model` is omitted, `init_chat_model` returns a `_ConfigurableModel` (`:508-533`). It reads `config["configurable"]["{prefix}_{field}"]` at call time. Declarative calls (`bind_tools`, `with_structured_output`) are *queued* and replayed (`:685-706`). **A new chat-model instance is constructed on every call** (`_model`, `:715-720`).
- **LangSmith gateway.** `"langsmith:..."` routes through `ChatOpenAI` pointed at LangSmith's gateway with the Responses API forced on (`:24-38`), so the vendor's own gateway is a first-class provider.
- **Capability data** comes from `BaseChatModel.profile` (e.g. `structured_output`, `max_input_tokens`), which summarization and AutoStrategy both use. When `profile` is missing, the code falls back to name regexes (§1.6).

---

## 5. Multi-agent patterns

**In the package:**

- **Subagent as a tool.** A tool calls a compiled `create_agent(name=...)`. `SubagentTransformer` (`langchain/agents/_subagent_transformer.py:1-20`) detects the nested run by a change in `lc_agent_name`. It exposes the nested run as a typed `run.subagents` stream handle and records which `tool_call_id` caused it. ToolNode deliberately lets subgraph interrupts bubble up through tools (`tool_node.py:973-983`, which cites the "supervisor with tools" case).
- **Handoffs.** A tool returns `Command(graph=Command.PARENT, goto=..., update=...)` to move control in the parent graph. ToolNode merges parallel parent `Send`s (`tool_node.py:895-919`). `create_agent(name=...)` can be added as a node in a parent `StateGraph`.
- **Agent as subgraph.** The compiled agent is an ordinary LangGraph graph, so it composes like any subgraph.

**Not in the package:** no `create_supervisor`, no swarm, no handoff-tool factory, no
"deep agent" (grep for `supervisor|handoff|swarm` finds only a comment in
`tool_node.py`). Those live in separate packages (`langgraph-supervisor`,
`langgraph-swarm`, `deepagents`) or only in the docs. In 1.x, multi-agent support is
"compose graphs yourself, plus a streaming affordance".

---

## 6. Human-in-the-loop, end to end

1. **Configure.** Pass `HumanInTheLoopMiddleware(interrupt_on={tool_name: True | False | InterruptOnConfig})`. `True` allows all four decisions. Each `InterruptOnConfig` has `allowed_decisions`, an optional `description` (a string or `(tool_call, state, runtime) → str`), `args_schema`, and a `when(ToolCallRequest) → bool` predicate. An empty `allowed_decisions` is rejected at construction so that a typo cannot silently disable the gate (`mw/human_in_the_loop.py:161-306`). A checkpointer is required for resume; this is a LangGraph requirement.
2. **Detect.** The middleware's `after_model` node finds the last AI message's tool calls that match the rules (`:427-472`).
3. **Interrupt.** It calls `interrupt(HITLRequest)` **once for the whole batch**. The payload is `{action_requests: [{name, args, description}], review_configs: [{action_name, allowed_decisions, args_schema?}]}` (`:81-89, 474-481`). The run stops and the client receives the interrupt.
4. **Resume.** The client resumes with `Command(resume={"decisions": [...]})`. There must be one decision per interrupted call, in order, or the middleware raises `ValueError` (`:483-489`). Per LangGraph semantics, **the after_model node is re-executed from the start**, and the second time `interrupt()` returns the resume value instead of raising (`lg_types.py:900-912`). Everything before the `interrupt()` call runs twice, including the `when` predicates and `description` callables.
5. **Apply the decisions** (`:344-392, 491-525`):
   - `approve`: the call is left unchanged.
   - `edit` (`edited_action: {name, args}`): the *original* call stays in the AI message, and the replacement is recorded in a private state key `hitl_edited_tool_calls`. Later, `wrap_tool_call` swaps in the edited call, possibly redirecting to a different tool, and **prepends a notice to the tool result** telling the model what actually ran (`:541-661`).
   - `reject` (optional `message`): a synthetic error `ToolMessage` is added. The model is told not to retry.
   - `respond` (`message`): a synthetic *success* `ToolMessage` is added and the tool is skipped. This is used for "ask the user" tools.

   The AI message's `tool_calls` list is **mutated in place** and re-emitted with the same id, so `add_messages` replaces it (`:518-525`).
6. **Continue.** The router sees that the rejected or responded calls already have ToolMessages, so it sends only the remaining calls to `tools` (§1.3 step 4).
7. Not to be confused with older APIs: `langgraph.prebuilt.interrupt.HumanInterrupt` (accept / ignore / response / edit) is deprecated. Its deprecation message points to `langchain.agents.interrupt`, **a module that does not exist in 1.4.4** (`langgraph/prebuilt/interrupt.py:7-10`). MCP elicitation reuses the same interrupt mechanism with its own payload types (`langchain/mcp/elicitation.py:56-160`).

---

## 7. Pain points and design debt

### 7.1 Layering

To understand one agent turn, a reader has to go through **five layers**:

1. `langchain.agents` builds the graph.
2. `langgraph.graph.StateGraph` compiles it to Pregel channels.
3. `langgraph.prebuilt.ToolNode` executes tools.
4. `langchain_core` provides Runnables, callbacks, messages, `BaseTool` and `BaseChatModel`.
5. The provider package (`langchain_openai` and others) translates to the wire format.

Tracing runs through `langsmith`, imported unconditionally at `factory.py:37`.

The abstractions leak across these boundaries:

- ToolNode reads Pregel internals (`config[CONF][CONFIG_KEY_READ].args[1]`, `tool_node.py:1304-1312`).
- The factory relies on knowledge of `langchain_openai` internals (`_is_openai_compatible_model`, `factory.py:626-643`).
- `ProviderStrategy` sends OpenAI-shaped kwargs to every provider (`so.py:295-314`).
- The public tool-injection API (`langchain.tools.ToolRuntime`, `InjectedState`) is re-exported from langgraph-prebuilt, while `ToolNode` itself is only imported privately as `_ToolNode` (`langchain/tools/tool_node.py:1-20`). Users see one package, but the code lives in another.

### 7.2 Concept count

A user who wants an agent with a custom guardrail plus HITL plus structured output meets
about 35 distinct concepts:

- `create_agent` and its 15 parameters
- model strings, provider inference and `BaseChatModel`
- three kinds of tools (`BaseTool`, callable, provider dict)
- 6 hooks × sync/async = 12 methods, 7 decorators, `hook_config`
- `ModelRequest`, `ModelResponse`, `ExtendedModelResponse`, `ModelCallResult`, `ToolCallRequest`
- `AgentState`, `InputAgentState`, `OutputAgentState`
- `OmitFromInput`, `OmitFromOutput`, `PrivateStateAttr`
- channel semantics: `add_messages`, `RemoveMessage(REMOVE_ALL_MESSAGES)`, `EphemeralValue`, `UntrackedValue`
- `Runtime`, `ToolRuntime` and `RunnableConfig`; the docstring itself has to explain that `ToolRuntime` ≠ `Runtime` (`tool_node.py:1666-1671`)
- `context_schema`, `store`, `checkpointer`
- `Command` (update / goto / graph / resume), `Send`, `interrupt`
- `jump_to` / `can_jump_to`
- Tool, Provider and Auto strategies with `handle_errors`
- `StreamTransformer` / `transformers`, `TracePolicy`

Many of these are LangGraph concepts that surface directly through the agent API.

### 7.3 Hidden behaviour

- **The middleware list is compiled into graph topology.** Each node-style hook becomes a node with a generated name, and jumps must be pre-declared (§2.5). Adding a logging `before_model` changes the graph, the checkpoints, the stream events and the recursion budget.
- **Ordering asymmetry.** `before_*` hooks run forward and `after_*` hooks run in reverse (§2.2). This is correct onion semantics, but it is spread over separate nodes, so it is easy to get wrong.
- **Exceptions as control flow.** Interrupts are `GraphBubbleUp` exceptions. Every `except Exception` in a middleware must re-raise them first, and fallback, retry, tool-error and ToolNode each have to remember to do so.
- **Interrupt replay.** A resumed node re-executes from the top (§6.4). Side effects before `interrupt()` happen twice.
- **Inconsistent defaults.** Tool exceptions crash the run (`tool_node.py:383-391`). Exhausted model retries quietly become an assistant message (`mw/model_retry.py:121, 210-219`).
- **Capability detection by regex** over model names inside the framework (`factory.py:182-203`).
- **In-place mutation of messages** that are part of state: HITL `tool_calls` (`mw/human_in_the_loop.py:518`) and `output.name` (`factory.py:1508`).
- **Rewrites of the whole history.** Repairing invalid tool calls emits remove-all plus the full history (`factory.py:249-254`). Summarization does the same (`mw/summarization.py:441`).
- **Internal LLM calls leak into the stream.** Streaming is callback-based, so summarization's private calls would appear in `run.messages`. They have to be tagged with a random process token and filtered by `InternalCallTransformer` (`mw/internal_call_transformer.py:1-60`). PII redaction likewise has to be done twice, once in state hooks and once as a stream transformer (`mw/pii.py:50-75`).
- **Resource lifetimes tied to `after_agent`**, which is skipped on error or interrupt (`mw/shell_tool.py:687-696`).

### 7.4 Performance overhead per agent step

Measurement: a fake model with zero latency and an echo tool, 100 loops, no checkpointer,
CPython 3.13 on Windows, best of 3. Script in the session scratchpad (`probe_timing.py`).

| configuration | 1 tool call/loop | 4 parallel calls/loop |
|---|---|---|
| hand-written loop (`model.invoke` + `tool.invoke`, core only) | 0.64 ms | 1.23 ms |
| `create_agent`, no middleware | 3.0 ms | 9.7 ms |
| `create_agent` + 5 middleware | 5.5 ms | 13.8 ms |

So the framework adds about **2.4–8.5 ms per iteration before any checkpointing**. That is
roughly 5–8× the bare loop, and it grows with each middleware node and with fan-out,
because every `Send` is a full Pregel task. This is small next to LLM latency, but it is
real for high-throughput or local-model workloads. A checkpointer would add one write per
superstep, and every middleware node is another superstep.

Other O(history) work done on every step:

- summarization counts tokens over the whole history (`:425`)
- context editing deep-copies the history (`:270`)
- the model node compares the repaired message list against the state with `!=` (`factory.py:1540`)

### 7.5 Debugging difficulty

- Stack traces pass through nested `compose_two` closures (`factory.py:331-368`), `traceable` wrappers, `RunnableCallable`, the Pregel runner, and the executor or `gather`.
- Routing logic lives in closures created by `_make_*_edge` factories, so it cannot be inspected on the compiled graph.
- `jump_to: "model"` resolves to whichever node happens to be the first `before_model` (`factory.py:1961-1962`).
- An undeclared `jump_to` is silently ignored on most nodes, but honoured on the loop-exit node (§2.5).
- `debug=True` prints Pregel steps, not middleware semantics.

### 7.6 Version churn (visible in this one snapshot)

- `create_react_agent` is deprecated (it had its own `version="v1"|"v2"` switch, `react.py:274-307, 844-859`). It was replaced by `create_agent` in a *different package*.
- The `HumanInterrupt` family is deprecated with a pointer to a non-existent module (§6.7). The decision vocabulary changed from accept/ignore/response/edit to approve/edit/reject/respond.
- Several APIs are mid-transition:
  - `ModelRequest.system_prompt` → `system_message`; attribute assignment → `override()` (`types.py:121-201`)
  - `ToolRetryMiddleware(on_failure="raise"|"return_message")` aliases (`mw/tool_retry.py:197-211`)
  - legacy `ToolCallWithContext` Send payload alongside the new list payload (`tool_node.py:1288-1297`)
  - `_InputAgentState` aliases (`types.py:370-373`)
  - `**deprecated_kwargs` on summarization
  - `config_schema` → `context_schema` (`react.py:518-526`)
  - a pending change to the Gemini provider default (`chat_base.py:584-594`)
- Embeddings and chains moved to `langchain-classic`. `langchain.mcp` is beta.
- `langchain` 1.4.4 pins `langgraph<1.3`, so the two packages version in lock-step but ship separately.

---

## 8. Implications for Criogen

Criogen's core today (0001) provides:

- supersteps, static edges, async routers that return declared labels, joins, cycles
- atomic steps and fail-fast errors
- JSON state with whole-key replacement and `update_conflict`
- `max_steps` (default 64)
- no reducers, no persistence, no streaming, and no dynamic fan-out (`Send`) yet

### 8.1 The shape of a minimal agent layer

Keep the agent graph **fixed and tiny**: `model ⇄ tools`, with one router on `model` that
returns `tools | end | model` and one router on `tools` that returns `model | end`. All
behaviour customisation runs **in process, inside those two nodes**, as an ordered
interceptor list. Middleware never becomes graph topology.

```
agent(model, tools, *, instructions, hooks=[...], output=Schema, limits=...)
  model node:  run before_model hooks → onion(wrap_model) → provider call → after_model hooks
               → emits {messages+, control?: "end"|"tools"|"model", structured?}
  router:      control ?? (pending tool calls ? "tools" : "end")
  tools node:  run all pending calls concurrently in-node, each through onion(wrap_tool)
               → emits {messages+} (exactly one result per call_id, enforced)
  router:      return_direct / output-tool done ? "end" : "model"
```

This gives LangChain's expressiveness (the same six hook points) without its costs:

- the graph is stable, so checkpoints, stream events and step budgets do not change when hooks are added
- one superstep per phase instead of one per hook; LangChain's per-hook nodes would eat through our `max_steps=64` quickly
- hooks cannot jump outside the agent, so no static `can_jump_to` declarations are needed

### 8.2 Adopt / adapt / avoid

| concept | verdict | reason |
|---|---|---|
| `wrap_model_call` / `wrap_tool_call` onion with a re-callable `handler` | **adopt** | One primitive covers retry, fallback, caching, prompt rewriting, tool filtering and HITL edit substitution. |
| `before_model` / `after_model` state hooks | **adapt** | Keep them, but run them inside the model node rather than as graph nodes, which keeps the graph stable and cheap. |
| `before_agent` / `after_agent` | **adapt** | Make them `on_start` / `on_end` with **finally semantics**: they also run on error, interrupt and cancel, so resource cleanup is reliable. |
| immutable `Request.override(...)` | **adopt** | Clear data flow, and it is easy to mirror in TS. Skip the deprecated mutable path entirely. |
| middleware-contributed state keys and tools | **adopt** | Good for encapsulation. Namespace the keys (`mw.<name>.*`) instead of merging TypedDicts, to avoid silent schema conflicts. |
| run-scoped vs thread-scoped state | **adapt** | Make it explicit (a `scope: run|thread` flag on the key) rather than implied by channel type. |
| `jump_to` in state with static `can_jump_to` | **avoid** | Hooks return a typed control value. The agent's own router owns the label set. |
| Tool, Provider and Auto structured-output strategies | **adapt** | Keep tool strategy as the fallback and provider strategy as the preferred path. Detect support from **declared adapter capabilities**, never from model-name regexes, and validate dict schemas too. |
| injected tool args (`ToolRuntime`, `InjectedState`) with stripping of forged args | **adopt** | One `ctx` parameter (state view, context, store, call_id, emit) is simpler than three annotation kinds. Keep the anti-forgery strip. |
| tools returning `Command` (state update + goto) | **adapt** | Allow `{result, state_update}` but enforce exactly one result per call_id, as ToolNode does. Leave parent-graph `goto` for the subgraph phase. |
| batched HITL payload with approve/edit/reject/respond | **adopt** | It is the most mature part of the design. `respond` (human answers for the tool) and the edit notice to the model are both useful ideas. |
| interrupts via exceptions plus node re-execution | **avoid** | Use an explicit `Suspend(payload)` node outcome. On resume, call a continuation (`node.resume(decision)`) or re-enter with the decision in input. Never silently replay side effects. |
| unbounded recursion limit plus opt-in call-limit middleware | **avoid** | Ship a default `max_model_calls`, separate from the core `max_steps`, with a clear error code. |
| default "exhausted retry becomes a fake assistant message" | **avoid** | Errors stay errors unless policy says otherwise. Use the same default for model and tool failures. |
| `init_chat_model("provider:model")` | **adapt** | Keep the string form, but resolve through an explicit registry where adapters register themselves (an entry point or import), and never by guessing from name prefixes. Cache client instances. |
| callback-based streaming, with transformers to filter internal calls | **avoid** | Emit structured events from the runtime with an `origin` tag (agent, hook, internal). Filtering is then a query, not a mutation. |
| `langsmith` hard import / tracing wrappers on every hook | **avoid** | Use OpenTelemetry-style spans from the core with zero cost when disabled. |
| subagent-as-tool with a typed nested stream handle | **adopt (later)** | Cheap to support once subgraphs exist. Record `cause = tool_call_id` on the nested run. |
| supervisor/swarm factories | **avoid in core** | LangChain itself keeps these out of the main package. Document them as patterns. |

### 8.3 What needs core-runtime support and what can live in the SDK

| feature | core (Rust) | SDK (Py/TS) |
|---|---|---|
| model⇄tools loop | already expressible: cycle plus async router. **Needs a canonical message/tool-call JSON schema** shared by all three languages. | node implementations, provider adapters |
| `messages` append | **needed: an `append` reducer** (or a per-key merge policy). Otherwise every step ships the whole history across the FFI boundary and back, so each step costs O(history). | — |
| parallel tool calls | none needed if tools run concurrently **inside** the tools node. A `Send`-style dynamic fan-out is only required for per-call checkpoints or per-call HITL. Defer it. | `asyncio.gather` / `Promise.all` inside the tools node |
| hooks / middleware onion | none | entirely SDK. Hooks must be written in the agent's own language. |
| language-neutral built-ins (call limits, retry/backoff, timeouts, tool-call caps) | **should be core**, as declarative config, so Py and TS agents get identical behaviour and fewer FFI hops | thin config wrappers |
| per-node retry and timeout, cancellation | **core** (already in 0001 "not yet specified") | expose as options |
| structured output | none | SDK: tool strategy vs provider strategy, validation via Pydantic or Zod |
| HITL / interrupts | **core**: a node outcome `Suspended{payload}` plus persistence of the step boundary plus a `resume(run_id, value)` API. Atomic supersteps are already the right checkpoint boundary. | the HITL policy, payload shape and decision handling |
| streaming (tokens, tool deltas, custom events) | **core**: an event channel with node/run/step ids and an `origin` tag. Node code gets an `emit()` handle. | token streaming from provider adapters into `emit()` |
| step budget | already present (`max_steps`). The agent loop costs 2 steps per iteration, so the agent should set `max_steps` from `max_model_calls`. | the default iteration cap |
| subagents / handoffs | subgraphs (unspecified in 0001), a parent-goto equivalent, and a nested event namespace | subagent-as-tool helper |
| tracing | spans per step/node in core | span attributes for model and tool calls |

### 8.4 Concrete recommendations for the Phase 4 design doc

1. Specify the **message and tool-call JSON schema** first (roles, content blocks, `tool_calls[{id,name,args}]`, `tool_result{call_id,status,content}`, `invalid_tool_calls`). Every other feature depends on it.
2. Add a minimal **reducer** to the core: `append` for lists. It is the smallest change that fixes O(history) FFI traffic, and it keeps `update_conflict` for every other key.
3. Define **`Suspend` as a third node outcome** (besides update and error), alongside the persistence work. Do not emulate it with exceptions.
4. Keep middleware an SDK concept with **six hooks** (`on_start`, `before_model`, `wrap_model`, `after_model`, `wrap_tool`, `on_end`). Specify ordering explicitly: forward for `before_*`, reverse for `after_*`, first is outermost for `wrap_*`. Provide async only in TS, and both sync and async in Python with automatic adaptation, never a runtime `NotImplementedError`.
5. Make every agent-level failure an **Criogen error code** (`model_failed`, `tool_failed`, `output_invalid`, `model_call_limit`), consistent with the 0001 `RunError` table. Make conversion of a tool error into a message an explicit, uniform policy.
6. Budget the per-step overhead: LangChain adds about 2.4–8.5 ms per iteration in-process. Benchmark against the same fake-model loop (§7.4) before optimising.

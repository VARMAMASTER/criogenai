# Research: langchain-core 1.6.9 design study

Status: research note (input to design docs, not a decision).
Studied: `langchain-core 1.6.9`, `langchain-protocol 0.0.19` (both MIT, installed in
`research/.venv`). `langgraph 1.2.14` is also installed and is referenced where core
depends on it implicitly.

**Method.** I read the installed source and ran small measurements in the research venv.
Mechanisms are described in my own words. No code is copied beyond identifiers.
Citations are `path:line` relative to `site-packages/langchain_core/` unless prefixed
(`langchain_protocol/…`, `langgraph/…`). Line numbers are for these exact versions.

**Measurement environment.** Windows 11, CPython 3.13.14, pydantic 2.14.0, tracing
env vars unset. Numbers are order-of-magnitude only.

---

## 0. TL;DR

1. **Runnable** is a single generic interface (`invoke/ainvoke/batch/abatch/stream/astream/transform/astream_events`).
   Most of it has default implementations derived from `invoke`. Composition (`|`, dicts, lambdas)
   produces a tree of Runnable objects, and each node opens a traced "run" with its own UUID.
2. Config (`tags`, `metadata`, `callbacks`, `run_name`, `max_concurrency`, `recursion_limit`,
   `configurable`, `run_id`) moves in two ways. It is passed explicitly, and a `ContextVar` fills
   in what is missing. Parent/child run linkage therefore works even when user code forgets to
   pass `config`.
3. **Messages in 1.x have two representations.** The legacy `content: str | list[str|dict]` is
   kept for compatibility. A new, typed, provider-neutral **content-block** union
   (`text`, `reasoning`, `tool_call`, `image`, `server_tool_call`, `non_standard`, …) is derived
   lazily through per-provider translators. Streaming merges chunks with a type-driven heuristic
   dict/list merger keyed by `index`.
4. **A new language-neutral wire protocol has appeared.** `langchain_protocol` is generated from
   a CDDL file. It defines message-lifecycle events (`message-start → content-block-start/delta/finish →
   message-finish`), tool events, lifecycle, checkpoint and HITL commands. Core now drives chat-model
   streaming "v3" through it, with a compat bridge that turns old chunks into events. This is the
   part most worth studying for AgentBuild.
5. **Chat-model provider contract.** The minimum is `_generate` plus `_llm_type`. Optional:
   `_stream`, `_agenerate`, `_astream`, and the new `_stream_chat_model_events`. The base class
   adds caching, rate limiting, callbacks, ID assignment, the output-format version, and the
   decision between streaming and non-streaming. That decision depends partly on *which callback
   handlers are attached*.
6. **Tools.** Pydantic schemas are inferred from signatures and docstrings. Injected args are
   hidden from the model schema. `ToolException` is turned into an error `ToolMessage` only when
   the tool opts in. Provider schema conversion is OpenAI-format-centric: other formats are
   *sniffed* from dict shape.
7. **Callbacks are the backbone of tracing and of `astream_events`.** There are about 20 event
   methods × sync/async, plus per-run-type managers. The run tree is LangSmith's `RunTree`
   (`tracers/schemas.py:10`). LangSmith is a hard dependency.
8. **Major debt.** Deep pydantic-based hierarchies. Sync/async code is written twice
   (`BaseTool.run`/`arun`, the callback managers). Many heuristics are type-driven (`+`
   aggregation, dict-shape sniffing). Overhead is about 130 µs per trivial `RunnableLambda.invoke`.
   Versioned streaming APIs are layered on each other (`astream_log`, events v1/v2/v3,
   `output_version` v0/v1).
9. **For AgentBuild:** adopt the content-block schema and the message-event lifecycle as
   **JSON-schema contracts in the Rust core**. Adopt the run-tree/event model, but as plain data
   events, not a callback-handler class hierarchy. Avoid a universal Runnable algebra, the implicit
   `+` aggregation, pydantic-as-schema-source, and tracer-vendor coupling.

---

## 1. Runnable protocol (`runnables/`)

### 1.1 Interface

- `Runnable(ABC, Generic[Input, Output])` is at `runnables/base.py:134`. Only `invoke` is abstract
  (`base.py:887`). Everything else has a default:
  - `ainvoke`: runs the sync `invoke` in a thread executor (`base.py:909-930`).
  - `batch`: maps `invoke` over a `ContextThreadPoolExecutor` and skips the pool when there is
    one input (`base.py:932-981`). `abatch` uses `asyncio.gather` with a semaphore
    (`base.py:1067-1114`, `runnables/utils.py:66`). `batch_as_completed` and
    `abatch_as_completed` yield `(index, output)` (`base.py:983-1190`).
  - `stream`/`astream` yield the single `invoke`/`ainvoke` result (`base.py:1195-1236`).
    Every Runnable is "streamable", but only overriding classes actually stream incrementally.
  - `transform`/`atransform` (stream in → stream out) buffer the whole input by summing chunks
    with `+`, then call `stream` (`base.py:1761-1850`). See §1.6.
  - `astream_log` (JSON-Patch based, `tracers/log_stream.py:19,115`) and `astream_events`
    (`base.py:1369`) are observability streams built on callbacks (§6).
- Introspection: `input_schema`/`output_schema`/`get_input_jsonschema` build pydantic models
  dynamically (`base.py:376-530`). `config_specs` and `config_schema` describe the runtime-configurable
  fields (`base.py:531-592`). `get_graph` renders the composition as a graph (`base.py:594`, plus
  `graph*.py` for ASCII, Mermaid and PNG).
- Declarative modifiers return wrappers: `bind` (pre-set kwargs, `base.py:1852`), `with_config`
  (`1886`), `with_listeners`/`with_alisteners` (`1911`, `1983`), `with_types` (`2080`), `with_retry`
  (`2102`), `map` (`2169`), `with_fallbacks` (`2192`), and `as_tool` (`2712`, which turns any
  Runnable into a tool).
- `RunnableSerializable` (`base.py:2831`) mixes in `Serializable` (a pydantic `BaseModel`) and adds
  `configurable_fields`/`configurable_alternatives` (`base.py:2859`, `2917`). Almost every concrete
  class in the ecosystem (chat models, tools, prompts, parsers) is a pydantic model *and* a Runnable.

### 1.2 Composition

- `a | b` and `b.__ror__` coerce the operand (`base.py:629-723`). `coerce_to_runnable`
  (`base.py:6644-6669`) turns generator functions into `RunnableGenerator`, other callables into
  `RunnableLambda`, and dicts into `RunnableParallel`. Plain Python values therefore become graph
  nodes implicitly.
- **RunnableSequence** (`base.py:3079`) flattens nested sequences into first/middle/last. `invoke`
  opens one parent run and calls each step with a child callback manager tagged `seq:step:N`
  (`base.py:3434-3468`). `batch` runs each step as a batch across all inputs, one step at a time
  (`base.py:3510`). Streaming chains each step's `transform` into the next (`base.py:3768-3789`).
- **RunnableParallel** (`base.py:3868`) runs every branch on the same input. Sync mode uses a
  thread pool **created per invoke** and tags children `map:key:K` (`base.py:4143-4201`). Async
  mode uses `gather`. Streaming tees the input to every branch and interleaves the branch outputs
  as one-key dicts, whichever finishes first (`base.py:4257-4302`).
- **RunnableLambda** (`base.py:4707`) wraps a sync and/or async function:
  - If a function returns a Runnable, that Runnable is invoked in turn (dynamic dispatch), with
    `recursion_limit` decremented (`base.py:5188-5203`). This limit is the only cycle guard.
  - Calling sync `invoke` on an async-only lambda raises an error (`base.py:5306-5335`).
  - Generator functions are consumed and their chunks summed with `+` (`base.py:5167-5182`).
  - It **reads the function's source with `inspect.getsource` + `ast`** to guess which dict keys
    it reads, for schema inference (`runnables/utils.py:369-386`, used at `base.py:5013`). It also
    uses the source for `repr` (`base.py:5151`).
- **RunnableBranch** (`runnables/branch.py:43`) evaluates `(condition, runnable)` pairs in order,
  each condition being itself a Runnable, with a default branch. Children are tagged `condition:N`
  and `branch:N` (`branch.py:185-240`).
- **RunnablePassthrough / assign / pick** (`runnables/passthrough.py:74`, `352`, `679`).
  `assign(k=runnable)` merges a `RunnableParallel` result into the input dict. While streaming, it
  forwards non-assigned keys right away and computes assigned keys in the background
  (`passthrough.py:542-590`).
- `RouterRunnable` (`runnables/router.py:46`) dispatches on a `{"key", "input"}` dict.
  `RunnableWithMessageHistory` (`runnables/history.py:40`) is legacy memory glue.
- `RunnableBinding` (`base.py:6382`) is the generic wrapper behind `bind`, `with_config` and
  `with_retry`. It proxies unknown attributes to the bound object with `__getattr__` and
  injects `config` into proxied methods when their signature accepts it (`base.py:6553`).

### 1.3 Config propagation

- `RunnableConfig` is a `TypedDict(total=False)` (`runnables/config.py:57-130`) with the keys
  listed in `CONFIG_KEYS` (`config.py:131-140`). `recursion_limit` defaults to 25 (`config.py:171`).
- **Implicit inheritance.** `var_child_runnable_config` is a `ContextVar` (`config.py:174`).
  `ensure_config` (`config.py:255-309`) starts from defaults, overlays the context-var config
  (copying `tags`, `metadata`, `callbacks` and `configurable`), then overlays the explicit config.
  It moves **any unknown top-level key into `configurable`** and copies `configurable.model` and
  `checkpoint_ns` into `metadata` for tracing.
- **Per-child config.** Every composite calls `patch_config(config, callbacks=run_manager.get_child(tag))`
  and runs the child inside `set_config_context` (`config.py:224-252`). That function copies the
  current `contextvars.Context`, sets the context var, and **also sets LangSmith's own tracing
  context** whenever a `LangChainTracer` is present (`config.py:180-221`). Async children run as
  tasks created with that context (`runnables/utils.py:143-159`).
- The `run_id` in config is *popped* when used (for example `base.py:2298`), so only the outermost
  run takes a caller-provided ID.
- Thread pools: `ContextThreadPoolExecutor` copies the context into worker threads
  (`config.py:607-657`). `get_executor_for_config` builds a **new pool per call** sized by
  `max_concurrency` (`config.py:660-676`).
- The standard wrapping pattern is `_call_with_config` (`base.py:2272-2319`, async `2321-2362`):
  ensure config → configure callback manager → `on_chain_start` → run the function in the child
  context → `on_chain_end` or `on_chain_error` → re-raise.

### 1.4 Retry, fallbacks, configurable

- **with_retry** returns `RunnableRetry` (`runnables/retry.py:50`), a binding that delegates to
  **tenacity** (`retry.py:11-19`). The default is 3 attempts with exponential jitter, retrying on
  `Exception` unless types or a predicate are given (`retry.py:116-163`). Each attempt after the
  first is traced as a child run tagged `retry:attempt:N` (`retry.py:166-173`). Batch retry only
  re-runs the failed indices.
- **with_fallbacks** gives `RunnableWithFallbacks` (`runnables/fallbacks.py:37`). It tries each
  runnable in order on `exceptions_to_handle` (default `Exception`, `fallbacks.py:93`). With
  `exception_key` set, it writes the previous error into the input dict for the next attempt
  (`fallbacks.py:98`, `188-189`). If everything fails it raises the **first** error
  (`fallbacks.py:211-212`). While streaming, fallback is possible **only until the first chunk**:
  it pulls one chunk inside the try block and commits after that (`fallbacks.py:465-510`).
- **configurable_fields / configurable_alternatives** produce a `DynamicRunnable`
  (`runnables/configurable.py:50`, `317`, `474`). On every call `_prepare` reads
  `config["configurable"]`. If any field is overridden, it **reconstructs the pydantic object**
  from the default's fields plus the overrides (`configurable.py:419-458`). Alternatives pick a
  whole different Runnable by key (`configurable.py:615`).

### 1.5 Sync↔async bridging

- **Async from sync (the default).** `ainvoke` calls `run_in_executor` (`config.py:678-712`),
  which runs the sync function on the loop's default executor with a copied context. It converts
  `StopIteration` to `RuntimeError` because the former cannot cross a Future.
- **Async stream from a sync stream.** `BaseChatModel._astream` default runs `_stream` in the
  executor, then calls `next()` through the executor **once per chunk** (`chat_models.py:2275-2311`).
- **Sync from async is not supported** for user code. An async-only lambda raises on `invoke`
  (§1.2). The callback system is the exception: an async handler fired from sync code is run on a
  **fresh event loop on a shared thread pool** (`callbacks/manager.py:355-400`). Errors from those
  coroutines are always swallowed, which the code itself flags as an asymmetry.
- **Sync handlers on the async path** are run in an executor per event unless `run_inline=True`
  (`callbacks/manager.py:404-450`).
- In practice every core class implements both paths by hand: `BaseTool.run` (`tools/base.py:1009-1135`)
  and `arun` (`1137-1267`) are about 130 lines each and nearly identical. The callback-manager classes
  are duplicated as `X`/`AsyncX` (`callbacks/manager.py:490-1377` vs `1859-2390`).

### 1.6 Streaming transform semantics

- Streams are iterators of **chunks of the output type**. Aggregation is defined by `__add__` on
  the chunk type: `AIMessageChunk`, `ChatGenerationChunk`, `AddableDict`, strings.
- `_transform_stream_with_config` (`base.py:2506-2602`) tees the input so it can be traced. It
  sums output chunks with `+` to build the final traced output, and falls back to "last chunk wins"
  as soon as `+` raises `TypeError`. The same "try `+`, else keep the last" rule appears in the
  default `transform` (`base.py:1785-1803`) and in `RunnableLambda` generators.
- A sequence streams end-to-end only if **every** step overrides `transform`. Otherwise the first
  non-streaming step buffers everything (`base.py:3779-3787`). Parsers such as
  `JsonOutputParser` are streaming-aware: they re-parse the accumulated text on every chunk (§5).
- `RunnableGenerator` (`base.py:4403`) is the escape hatch: a user generator that receives the
  input iterator.

---

## 2. Messages (`messages/`)

### 2.1 Classes

- `BaseMessage(Serializable)` (`messages/base.py:93-135`) has these fields: `content` (`str | list[str|dict]`),
  `additional_kwargs` (a provider dump), `response_metadata`, `type` (discriminator), `name`, `id`.
- Concrete classes: `HumanMessage` (`human.py:9`), `SystemMessage` (`system.py:9`), `AIMessage`
  (`ai.py:160`), `ToolMessage` (`tool.py:26`), `ChatMessage` (arbitrary role, `chat.py:15`),
  `FunctionMessage` (legacy, `function.py`), and `RemoveMessage` (a deletion marker for LangGraph
  reducers, `modifier.py:8`).
- Each has a `*Chunk` twin using **multiple inheritance**, for example
  `AIMessageChunk(AIMessage, BaseMessageChunk)` (`ai.py:418`). The chunk's `type` differs
  (`"AIMessageChunk"`).
- `MessageLikeRepresentation` accepts a message, a string, a `(role, content)` tuple, a dict, or a
  list (`messages/utils.py:596-602`). `convert_to_messages` coerces all of these, and
  `LanguageModelInput` (`language_models/base.py:140`) adds `PromptValue` and `str`.
- Utility surface: `trim_messages` (`utils.py:1146`), `filter_messages` (`870`),
  `merge_message_runs` (`1015`), `convert_to_openai_messages` (`1543`, the de facto interchange
  format), and `count_tokens_approximately` (`2257`).
- Back-compat shims are visible. `.text` is a `str` subclass that can also be *called* like the old
  `.text()` method, with a deprecation warning (`base.py:47-90`).

### 2.2 Standard content blocks (new in 1.0)

- These live in `messages/content.py` as `TypedDict`s. The module docstring explains why: providers
  are incompatible, a message is an ordered list of blocks, and adapters translate to and from
  provider formats (`content.py:1-60`).
- Block types (`content.py:126-855`, set at `856-878`):
  - text: `text`, with `annotations` (`citation`, `non_standard_annotation`)
  - `reasoning`
  - tools: `tool_call`, `tool_call_chunk`, `invalid_tool_call`
  - provider-executed tools: `server_tool_call`, `server_tool_call_chunk`, `server_tool_result`
  - data: `image`, `audio`, `video`, `file`, `text-plain`
  - the escape hatch: `non_standard` (`{value: {...}}`)
- Common fields: `type`, optional `id`, `index: int | str` (position while streaming), and
  `extras: dict` for provider-specific bits such as signatures and cache control. Data blocks
  carry one of `url`, `base64` (with `mime_type`) or `file_id` (`content.py:498-548`).
- **Derivation is lazy.** `BaseMessage.content_blocks` (`base.py:200-262`) runs several passes.
  Strings become text blocks. Known v1 dicts are kept, but anything with `source_type` is treated
  as a v0 block. Unknown dicts become `non_standard`. Then per-provider "input" converters (v0,
  OpenAI chat completions, Anthropic, Google GenAI, Bedrock Converse) are tried in turn.
- For `AIMessage`, if `response_metadata.model_provider` is set, a **registered translator** for
  that provider is used first (`ai.py:243-306`). The registry is `messages/block_translators/__init__.py`.
  Core ships translators for openai, anthropic, google_genai, bedrock, bedrock_converse and groq
  (about 3.2k lines in `block_translators/`). Partner packages can register more at import time.
- `output_version` on the chat model (`language_models/chat_models.py:355`, env `LC_OUTPUT_VERSION`).
  `"v0"` keeps provider-native content. `"v1"` overwrites `content` with standard blocks
  (`chat_models.py:2028-2031`). Both shapes therefore exist in the wild, and readers must call
  `.content_blocks` to get a stable view.

### 2.3 Tool calls

- `AIMessage.tool_calls: list[ToolCall]` and `invalid_tool_calls` are first-class fields
  (`ai.py:170-173`). `ToolCall` is `{name, args: dict, id, type?}` (`messages/tool.py:206`).
  `InvalidToolCall` keeps the raw `args: str` plus `error`, so the agent loop can report a parse
  failure back to the model instead of crashing.
- Streaming uses `tool_call_chunks: list[ToolCallChunk]` with `args` as a **partial JSON string**
  and an `index` (`ai.py:427`, `tool.py:261`). The validator `init_tool_calls` (`ai.py:508-600`)
  re-derives `tool_calls`/`invalid_tool_calls` from the chunks using `parse_partial_json` **every
  time a chunk object is constructed**, including every intermediate sum.
- `ToolCall` is defined **twice**: in `messages/tool.py:206` (legacy, `type` optional) and in
  `messages/content.py:247` (block form, with `index` and `extras`).
- `ToolMessage` (`tool.py:26-87`) has `tool_call_id`, `artifact: Any` (a payload for downstream
  code that is *not* sent to the model), and `status: "success"|"error"`. A before-validator
  stringifies non-string content and numeric IDs (`tool.py:92-135`).

### 2.4 Chunk merging

- `BaseMessageChunk.__add__` (`base.py:412-471`) merges `content` with `merge_content`
  (`base.py:366-406`). Two strings concatenate. A string meeting a list is prepended. Two lists
  are merged by index. Dicts are merged with `merge_dicts`.
- `merge_dicts`/`merge_lists` (`utils/_merge.py:6-178`) is a **generic, type-driven merger**:
  - strings are concatenated, except `id`, `output_version` and `model_provider` when the values
    are equal;
  - ints are **summed**, except `index`, `created` and `timestamp`, where the last value wins;
  - dicts are merged recursively;
  - list elements carrying the same `index` (and compatible `id`) are merged pairwise;
  - booleans or mismatched types raise an error.
  Merge semantics are therefore implicit in value types, not declared per field.
- `add_ai_message_chunks` (`ai.py:651-732`) also sums `usage_metadata` (`add_usage`, `ai.py:734`).
  It chooses the message `id` by preference: a provider ID first, then `lc_run-*`, then `lc_*`
  (`ai.py:697-716`). It propagates `chunk_position="last"`, the explicit end-of-stream marker that
  triggers final tool-call parsing (`ai.py:430`, `574`).
- Measured: summing 500 single-character tool-call-arg chunks one at a time takes **about 40 ms**,
  which is quadratic. The cause is the validator re-parse plus copies on every `+`.

### 2.5 Usage metadata

- `UsageMetadata` (`ai.py:104-155`): required `input_tokens`, `output_tokens` and `total_tokens`.
  Optional `input_token_details` (`audio`, `cache_creation`, `cache_read`) and
  `output_token_details` (`audio`, `reasoning`) (`ai.py:38-100`). `add_usage` and
  `subtract_usage` (`ai.py:734`, `793`) support aggregation and deltas.

### 2.6 IDs

- Message IDs are optional strings. When the provider supplies none, core assigns
  `lc_run-<run_uuid>` (or with a `-<idx>` suffix for multi-generation results)
  (`chat_models.py:2038`). The prefixes are at `utils/utils.py:495-498`. Run IDs are **UUIDv7**,
  so they sort by time (`callbacks/manager.py:33`, `1402`, …).
- Cache keys strip message IDs before hashing (`chat_models.py:1907-1916`).

### 2.7 Serialization

- Messages are `Serializable` pydantic models. `message_to_dict` gives `{type, data}`
  (`base.py:474`), and `messages_from_dict` reverses it (`utils.py:560`). They also round-trip
  through the general `dumpd`/`load` system (§7). `allowed_objects="messages"` is the documented
  safe subset.

---

## 3. Chat models (`language_models/`)

### 3.1 Provider contract

- `BaseChatModel(BaseLanguageModel[AIMessage], ABC)` (`chat_models.py:284`). Its docstring table
  (`chat_models.py:319-331`) lists what a provider must supply: **`_generate`** and **`_llm_type`**.
  Optional: `_identifying_params`, `_stream`, `_agenerate` and `_astream`. The undocumented-in-table
  native **`_stream_chat_model_events` / `_astream_chat_model_events`** hooks emit protocol events
  directly (`chat_models.py:631-632`, `677-686`).
- `_generate(messages, stop, run_manager, **kwargs) -> ChatResult` (`chat_models.py:2209`).
  `ChatResult.generations: list[ChatGeneration]` wraps the `AIMessage` (`outputs/chat_result.py:10`).
  `_stream` yields `ChatGenerationChunk` (`2255`). These wrapper types are vestiges of the
  completion-LLM era.
- Inherited fields, all pydantic: `cache`, `verbose`, `callbacks`, `tags`, `metadata`
  (`language_models/base.py:190-215`), `rate_limiter` (`chat_models.py:334`), `disable_streaming`
  (`337`), `output_version` (`355`) and `profile` (`378`).
- `bind_tools` **raises `NotImplementedError` in the base** (`chat_models.py:2366-2383`). Each
  provider converts tools itself, usually through `convert_to_openai_tool`.

### 3.2 Call path

`invoke` → `generate_prompt` → `generate` → `_generate_with_cache` → `_generate`
(`chat_models.py:475-500`, `1592`, `1892`).

- `invoke` wraps the input in a one-element batch and unwraps `generations[0][0].message`. The
  list-of-lists comes from the legacy batch shape of `LLMResult`.
- `generate` configures the callback manager, merging LangSmith-specific `ls_*` invocation
  params into the metadata (`_get_ls_params`, `chat_models.py:1502`). It fires
  `on_chat_model_start` once per prompt (`chat_models.py:1668`), then `on_llm_end`/`on_llm_error`.
- `_generate_with_cache` (`chat_models.py:1892-2049`) does the rest, in order:
  1. Cache lookup keyed by `(dumps(messages with ids stripped), llm_string)` (`1899-1925`). A hit
     *replays* synthetic protocol events for v3 observers (`1446`).
  2. `rate_limiter.acquire()`, **after** the cache check (`1935`).
  3. If a v2-protocol handler is attached → drive the event generator (`1942-1966`).
  4. Else, if streaming should be used → call `_stream`, fire `on_llm_new_token` per chunk, append
     a final empty `chunk_position="last"` chunk, and aggregate (`1968-2020`).
  5. Else → `_generate`. Whether to pass `run_manager` is decided by **inspecting the signature on
     every call** (`2021`).
  6. Apply v1 content conversion, assign IDs, merge `llm_output` into `response_metadata`, and
     update the cache (`2030-2048`).
- `stream()` (`chat_models.py:727-856`) has its own callback setup and **does not consult the
  cache**, unlike `invoke`. It applies the rate limiter (`786`).

### 3.3 How streaming is chosen

- `_should_stream` (`chat_models.py:549-585`) returns false if `_stream` is not overridden or if
  streaming is disabled: by `disable_streaming=True`, by `"tool_calling"` mode with `tools` in the
  kwargs, by `stream=False`, or by `streaming=False` (`525-547`). It returns true if `stream=True`
  or the instance has `streaming=True`. **Otherwise it returns true iff any attached callback
  handler implements `_StreamingCallbackHandler`** (`585`).
- The result: `invoke()` silently becomes a streaming call when the user wraps it in
  `astream_events` or `astream_log`. That is how token events surface from inside a non-streaming
  chain. It also means the observer changes the execution path.
- `_should_use_protocol_streaming` (`587-643`) is the parallel decision for the v3 event path. It is
  triggered by handlers that subclass `_V2StreamingCallbackHandler` (`tracers/_streaming.py:32`).
  Note the naming: a "v2 handler" drives `stream_events(version="v3")`.

### 3.4 Structured output

- The default `with_structured_output` (`chat_models.py:2385-2566`) calls
  `bind_tools([schema], tool_choice="any")` and pipes the result into `PydanticToolsParser` or
  `JsonOutputKeyToolsParser` (first tool only). The `method` and `strict` arguments are popped and
  **ignored** in the base (`2530-2531`). Providers override the method to support native JSON-schema
  modes. `include_raw=True` builds `{raw, parsed, parsing_error}` out of `RunnableMap`, `assign` and
  `with_fallbacks(exception_key="parsing_error")` (`2556-2565`), so parsing errors are captured as
  data rather than raised.
- Structured-output requests are tagged for LangSmith via the hidden kwarg `ls_structured_output_format`
  (`chat_models.py:263-281`, `2540-2547`).

### 3.5 Caching, rate limiting, profiles

- `BaseCache` (`caches.py:32`) is `lookup`/`update`/`clear` keyed by `(prompt_str, llm_string)`.
  `InMemoryCache` is at `caches.py:155`. It is a global or per-model setting: `cache: BaseCache | bool | None`.
- `BaseRateLimiter.acquire(blocking)` / `aacquire` (`rate_limiters.py:11-66`). `InMemoryRateLimiter`
  is a token bucket counting **requests only**, not LLM tokens, within one process
  (`rate_limiters.py:67-117`).
- `ModelProfile` (`language_models/model_profile.py:13-150`) is a `total=False` TypedDict of
  capabilities: context and output limits, input/output modalities, `tool_calling`, `tool_choice`,
  `tool_call_streaming`, `structured_output`, `reasoning_output`, effort levels. The data comes
  from models.dev through a separate generator package, and providers load it on init
  (`chat_models.py:398-445`). It is marked beta.

### 3.6 v3 streaming: typed per-message streams

- `stream_events(version="v3")` / `astream_events(version="v3")` on a chat model return a
  `ChatModelStream` / `AsyncChatModelStream` (`language_models/chat_model_stream.py:1-15`, `1125`, `1281`).
  It offers typed **projections** (`.text`, `.reasoning`, `.tool_calls`, `.usage`, `.output`), each
  iterable for deltas or awaitable for the final value. It also provides a raw event iterator with
  replay-buffer semantics, so several consumers can read it (`chat_model_stream.py:1208-1262`).
- The **compat bridge** (`language_models/_compat_bridge.py:1-31`) converts any legacy `_stream`
  output into protocol events. It treats each chunk's `content_blocks` as a delta slice and keeps
  per-index state, so that `content-block-finish` can carry a finalized block (with tool-call
  args parsed to a dict) (`chunks_to_events`, `590`). `message_to_events` (`776`) synthesizes the
  full lifecycle from a finished message (cache hits, checkpoint restores).
- Every event is also delivered to callbacks through the new `on_stream_event` hook
  (`callbacks/base.py:128-167`, `chat_models.py:688-693`).

---

## 4. Tools (`tools/`)

### 4.1 BaseTool

- `BaseTool(RunnableSerializable[str | dict | ToolCall, Any])` (`tools/base.py:433`). Fields:
  `name`, `description`, `args_schema` (a pydantic model **or** a raw JSON-schema dict), `return_direct`,
  `callbacks`, `tags`, `metadata`, `handle_tool_error`, `handle_validation_error`, `response_format`,
  and `extras` (provider-specific, e.g. cache control) (`tools/base.py:474-560`).
- Input forms: a string (single-arg tools), an args dict, or a **full `ToolCall`**. With a `ToolCall`,
  `_prep_run_args` extracts `args` and `tool_call_id` (`base.py:1352-1388`). The output is wrapped
  in a `ToolMessage` **only if a `tool_call_id` was present** (`_format_output`, `base.py:1390-1427`).
  Otherwise the raw return value is passed through. The return type of `invoke` therefore depends on
  the input shape.
- `run()` (`base.py:1009-1135`) works like this:
  1. Configure callbacks.
  2. Fire `on_tool_start` with a stringified input and the injected args filtered out.
  3. Validate and parse the input against `args_schema`.
  4. Inject `run_manager`/`config` if the `_run` signature asks for them, which is checked by
     introspection on every call.
  5. Call `_run` in the child context.
  6. Unpack `(content, artifact)` when `response_format == "content_and_artifact"`.
  7. Fire `on_tool_end` or `on_tool_error`.
- Subclasses: `StructuredTool` (`tools/structured.py`, with `func` and an optional `coroutine`, falling
  back to the executor when there is no coroutine, `structured.py:121-123`), `Tool` (a legacy
  single-string tool, `tools/simple.py`) and the retriever tool (`tools/retriever.py`).

### 4.2 Schema inference

- `@tool` (`tools/convert.py:77`) accepts a function, a Runnable, or a name plus options:
  `description`, `args_schema`, `infer_schema`, `response_format`, `parse_docstring`,
  `error_on_invalid_docstring` and `extras`. It builds a `StructuredTool.from_function`
  (`convert.py:317`).
- `create_schema_from_function` (`tools/base.py:263-370`) builds a pydantic model **by running
  pydantic's deprecated `validate_arguments` on the function**, with deprecation warnings
  suppressed. It filters `self`/`cls`, `run_manager`, `callbacks` and injected args, and adds
  descriptions from a Google-style docstring (`base.py:126-202`, `function_calling.py:735`) or from
  `Annotated[..., "desc"]`. Pydantic v1 annotations are still detected and handled
  (`base.py:221-250`).
- `tool_call_schema` (`base.py:677-723`) is the **model-facing** schema: the input schema minus
  injected args. It is memoized per instance. Pydantic does not cache `model_json_schema`, so core
  **monkey-patches** a cache onto the generated class (`base.py:404-431`).

### 4.3 Injected arguments

- `InjectedToolArg` (`base.py:1726`) is a marker used through `Annotated[T, InjectedToolArg]`. Such
  arguments are kept in the validation schema but removed from the model schema.
  `InjectedToolCallId` (`base.py:1756`) injects the current call ID.
- `_DirectlyInjectedToolArg` (`base.py:1734`) enables injection **by bare type annotation**. Its
  only user is `ToolRuntime`, which lives in **LangGraph**, not core
  (`langgraph/prebuilt/tool_node.py:1663`, fields `state, context, config, stream_writer,
  tool_call_id, store, tools, execution_info, server_info` at `1722-1730`). `InjectedState` is also
  in LangGraph (`tool_node.py:1753`). Core's docstring example imports `ToolRuntime` from
  `langchain_core.tools` (`base.py:1745`), which does not export it. The injection *protocol* sits
  in core while the injected *values* are produced by the graph runtime.

### 4.4 Errors

- `ToolException` (`base.py:371`) is the opt-in "recoverable" error. `handle_tool_error` defaults to
  **False** (re-raise). `True` returns the message as content, a string replaces it, and a callable
  maps it (`base.py:527-540`). Handled errors give a `ToolMessage(status="error")`.
  `handle_validation_error` does the same for pydantic `ValidationError` (`542`). Any other
  exception always propagates (`base.py:1127-1132`). The agent-level policy ("send all errors back
  to the model") lives in LangGraph's ToolNode, not here.

### 4.5 Provider tool schemas (`utils/function_calling.py`)

- `convert_to_openai_function` (`function_calling.py:377-496`) accepts many input forms:
  - pydantic classes, TypedDicts, Python callables, and `BaseTool`;
  - dicts in **Anthropic** format (`name` + `input_schema`), **Bedrock Converse** (`toolSpec`),
    OpenAI format (`name`), or a JSON schema with a top-level `title`.
  Dict formats are detected **by sniffing keys** (`412-446`).
- `strict=True` sets `additionalProperties: false` recursively (`825`). `convert_to_openai_tool`
  (`515`) wraps the result as `{"type":"function","function":…}` and passes OpenAI built-in tool
  dicts through untouched (`_WellKnownOpenAITools`, `499-512`). `convert_to_json_schema` (`577`) is
  the provider-neutral variant.
- So the **OpenAI function schema is the internal lingua franca**. Every provider package converts
  from it.

---

## 5. Output parsers, structured output, prompts (brief)

- **Parsers** (`output_parsers/`) are Runnables over `str | BaseMessage`.
  `BaseCumulativeTransformOutputParser` (`output_parsers/transform.py:99-150`) streams by
  re-accumulating all chunks and **re-parsing the whole buffer on every chunk**, yielding when the
  parse result changes, or a diff if `diff=True`. That is O(n²) in output length.
  `JsonOutputParser` (`json.py:31`) relies on `parse_partial_json` (`utils/json.py:58`).
- Parsers still essential in 1.x: `StrOutputParser`, `JsonOutputParser`, `PydanticOutputParser`, and
  the tool-call parsers (`openai_tools.py:139-330`) used by `with_structured_output`. The XML,
  list and "format instructions" parsers are prompt-era leftovers.
- **Prompts** (`prompts/`): `ChatPromptTemplate` (`chat.py:794`) with role templates and
  `MessagesPlaceholder` (`chat.py:53`). Template formats are f-string, mustache and jinja2
  (`string.py:30`); jinja2 runs in a sandbox and is blocked during deserialization (`load/load.py:220`).
  With chat models taking message lists and agents building context in code, prompts are mostly a
  convenience layer and are not needed for orchestration.

---

## 6. Callbacks and tracing (`callbacks/`, `tracers/`)

### 6.1 Handler and manager design

- `BaseCallbackHandler` (`callbacks/base.py:496`) is assembled from mixins: LLM, Chain, Tool,
  Retriever, CallbackManager and RunManager.
  - Events: `on_llm_start`, `on_chat_model_start`, `on_llm_new_token`, `on_llm_end`, `on_llm_error`,
    `on_stream_event`, `on_chain_start/end/error`, `on_tool_start/end/error`,
    `on_retriever_start/end/error`, `on_agent_action/finish`, `on_text`, `on_retry`, `on_custom_event`.
  - Flags: `raise_error` and `run_inline`, plus seven `ignore_*` properties (`base.py:506-546`).
  - `AsyncCallbackHandler` (`base.py:548`) re-declares all of them as `async`.
- A **CallbackManager** holds `handlers` and `inheritable_handlers`, `tags` and `inheritable_tags`,
  `metadata` and `inheritable_metadata`, and `parent_run_id` (`callbacks/base.py:1004-1040`).
  `on_*_start` creates a run-scoped manager per run type (`CallbackManagerForLLMRun`, `…ChainRun`,
  `…ToolRun`, `…RetrieverRun`, each with an async twin: `manager.py:705-1377`). It carries a new
  UUIDv7 `run_id`.
- **Parent/child linkage.** `ParentRunManager.get_child(tag)` (`manager.py:599-618`) builds a new
  manager whose `parent_run_id` is the current run, keeping only the *inheritable* handlers, tags
  and metadata, plus an optional non-inheritable tag such as `seq:step:2`. Composites pass that
  child manager down inside `config["callbacks"]`, and the context var (§1.3) recovers it when user
  code does not.
- `_configure` (`manager.py:2390-2610`) merges, in order:
  - the LangSmith ambient tracing context (`get_tracing_context`, `2425-2441`), with explicit
    tie-breaking rules between the LangSmith parent and the LangChain parent (`2456-2466`);
  - inherited and local callbacks, tags and metadata;
  - a `LangChainTracer`, auto-added when LangSmith tracing is enabled through env or context var
    (`2496-2550`);
  - console handlers for `verbose`/`debug`.
- **Dispatch.** `handle_event` (`manager.py:285-368`) calls each handler, honouring the `ignore_*`
  flags. Exceptions are **logged and swallowed** unless `raise_error`. If a handler does not
  implement `on_chat_model_start`, it falls back to `on_llm_start` with stringified messages. The
  async version `gather`s non-inline handlers (`manager.py:1951`, `2022`).
- **Custom events.** `dispatch_custom_event` / `adispatch_custom_event` (`manager.py:2739`,
  `2614`) let user code inside a run emit named events. They need a parent run in context.

### 6.2 Tracers and the run tree

- `tracers/schemas.py:5-10` aliases `Run` to **`langsmith.RunTree`**. The in-memory trace model
  *is* LangSmith's.
- `_TracerCore` (`tracers/core.py:66-175`) keeps `run_map` (id → Run) and `order_map`
  (id → trace_id, `dotted_order`). `dotted_order` is a sortable path made of start-timestamp + id
  segments, built by appending to the parent's (`core.py:150-175`).
- `LangChainTracer` (`tracers/langchain.py:134`) posts and patches runs to LangSmith
  (`langchain.py:328-350`). Other tracers: stdout, the run collector, root listeners (used by
  `with_listeners`), the log stream, and the event stream.
- Package facts: `langsmith` is a hard dependency (`langchain_core-1.6.9.dist-info/METADATA:30`),
  imported lazily in about 10 places (`runnables/config.py`, `callbacks/manager.py`, `tracers/*`).
  The lazy import is explicitly there to avoid about 130 ms of import time (`config.py:191`).

### 6.3 `astream_events` (v2) implementation

- `_astream_events_implementation_v2` (`tracers/event_stream.py:1004-1100`):
  1. Creates an `_AstreamEventsCallbackHandler` (`event_stream.py:101`) backed by an in-memory
     channel (`142-144`).
  2. **Injects it into `config["callbacks"]`** as an inheritable handler.
  3. Runs `runnable.astream()` in a background task.
  4. Yields events from the channel.
  The input is patched into the first event because the input is not known until it has been
  consumed (`1070-1078`).
- Event shape (`runnables/schema.py:56-185`): `{event: "on_<type>_<start|stream|end>", name, run_id,
  parent_ids (root→parent), tags, metadata, data: {input|output|chunk|error}}`, plus
  `on_custom_event`. Filtering is by name, type or tag.
- `*_stream` events come from **tapping** the output iterators: `tap_output_aiter` (`178-231`). The
  first tapper for a run ID emits the stream events. Chat models emit them from `on_llm_new_token`
  instead. The handler being a `_StreamingCallbackHandler` is what flips chat models into
  streaming mode (§3.3).
- Version history visible in one method:
  - `astream_log`, JSON-Patch based, needs `jsonpatch`;
  - `astream_events` v1, built on `astream_log`, deprecated since 1.3.3 (`base.py:1643-1650`);
  - v2, callback-based;
  - v3, protocol-based, chat models and compiled graphs only (`base.py:1586-1614`).
  The sync `stream_events` only supports v3 (`base.py:1700-1759`).

---

## 7. Serialization (`load/`)

- `Serializable(BaseModel, ABC)` (`load/serializable.py:106`) is opt-in: `is_lc_serializable()`
  defaults to False (`139`). Hooks: `get_lc_namespace` (`152`), `lc_id` (`196`, the class path),
  `lc_secrets` (constructor arg → secret ID, `178`) and `lc_attributes` (`186`).
- Wire format (`serializable.py:21-56`): `{lc: 1, type: "constructor", id: [...path], kwargs: {...}}`.
  Secrets serialize as `{type: "secret", id: [ENV_NAME]}`, and unserializable objects as
  `{type: "not_implemented", repr}`. Only kwargs that differ from the defaults are emitted.
- **Loading is instantiation.** `Reviver` (`load/load.py:336`) maps the `id` path to a class.
  It checks an allowlist (`allowed_objects`: an explicit list, `"messages"`, `"core"` (the
  default) or `"all"`), a trusted-namespace list (`load.py:134-149`) and a no-import-by-path list
  (`152`). It then calls `importlib.import_module` (`557`) and the class constructor with the
  payload's kwargs.
- The module docstring (`load.py:1-133`) is effectively a threat model. Constructors and
  validators run during load. A manifest can set `base_url` or headers (SSRF). `secrets_from_env`
  reads arbitrary env vars (`load.py:481`). jinja2 templates are blocked (`220`). Plain user dicts
  containing an `lc` key are escaped as `{"__lc_escaped__": …}` so they cannot be confused with
  objects (`load/_validation.py:28-60`).
- Cross-language detail: there is a mapping table from **LangChain.js class paths** to Python paths
  (`load/mapping.py:986`). JS and Python identities are kept in sync by a hand-maintained table of
  about 1,100 lines, not by a shared schema.

---

## 8. `langchain_protocol`: a language-neutral contract

`langchain_protocol/protocol.py` (618 lines) is **generated by `cddl2py` from `protocol.cddl`**
(line 1; package README). It contains only types: no runtime, no transport. This is LangChain's own
move toward a schema-first, cross-language contract.

- **Content blocks** (`protocol.py:31-138`) mirror core's, but they **drift** from them in three
  ways. The protocol blocks have no `extras` field. The protocol has no `text-plain` block. And it
  adds `FinalizedContentBlock` (blocks valid after streaming finishes, without `*_chunk` types,
  `136`).
- **Deltas** (`156-176`): `text-delta`, `reasoning-delta`, `data-delta` (base64) and a generic
  `block-delta {fields}`.
- **Message lifecycle** (`458-488`): `message-start {role, id, metadata}` →
  `content-block-start {index, content}` → `content-block-delta {index, delta}` →
  `content-block-finish {index, content: finalized}` → `message-finish {usage}`. There is also an
  `error` event. `MessageRole` covers only ai, human and system (`20`); tool results travel on the
  tools channel.
- **Tool events** (`506-528`): `tool-started {tool_call_id, tool_name, input}`,
  `tool-output-delta`, `tool-finished {output}` and `tool-error {message, code}`.
- **Lifecycle** (`435-456`): agent status `started|running|completed|failed|interrupted`, with a
  **causation edge** (`toolCall`, `send` or `edge` from a named node) and a checkpoint reference.
- **Session protocol**:
  - commands carry `{id, method, params}`: `run.start`, `subscription.subscribe/unsubscribe/reconnect`,
    `agent.getTree`, `input.respond/inject` (HITL), `state.get/listCheckpoints/fork`;
  - responses are success or error, with a closed `ErrorCode` enum (`251-262`, `368`);
  - events are `{type:"event", method: <channel>, params, event_id?, seq?}` for resumable streams
    (`303-335`);
  - channels: `values`, `updates`, `messages`, `tools`, `lifecycle`, `input`, `checkpoints`,
    `tasks`, `custom` and `custom:*` (`389`);
  - namespaces are string paths, so subgraphs can be subscribed by prefix and depth (`391-396`).
- `Checkpoint {id, parent_id, step, source: input|loop|update|fork}` (`603-610`) and
  `UpdatesData {node, values}` (`611`) are superstep-oriented, and match our execution model closely.

---

## 9. Pain points and design debt

1. **One abstraction for everything.**
   - Prompts, models, parsers, tools, retrievers, lambdas and dicts are all `Runnable`, and almost
     all are pydantic models too.
   - MROs are deep and generic. A fake chat model resolves through `GenericFakeChatModel →
     BaseChatModel → BaseLanguageModel[AIMessage] → BaseLanguageModel → RunnableSerializable[…] →
     RunnableSerializable → Serializable → BaseModel → Runnable → ABC → Generic`, which is 12
     classes. `StructuredTool` has 10 and `AIMessageChunk` has 8, with diamond inheritance.
   - The generic interface forces meaningless defaults: `stream` yields once, `transform` buffers,
     and `batch` makes a thread pool. It also forces escape hatches: `RunnableGenerator`,
     `RunnableLambda` returning a Runnable, and `__getattr__` proxying in bindings.
2. **Per-call overhead** (measured, warm, see the environment line at the top):

   | Operation | Time per call |
   |---|---|
   | Plain Python function | 0.12 µs |
   | `RunnableLambda.invoke` | ~135 µs |
   | 3-step `RunnableSequence.invoke` | ~560 µs |
   | `@tool` invoke | ~255 µs |
   | Fake chat-model `invoke` | ~150 µs |

   Importing runnables, chat models, tools and messages took about 0.9 s on this machine, with
   pydantic `fields` alone about 130 ms. The sources are per-run UUIDv7 and callback-manager
   construction, two-level context copies, signature introspection on each call
   (`chat_models.py:2021`, `tools/base.py:1094-1097`), pydantic model construction for every
   message and chunk, and new thread pools per batch or parallel call (`config.py:672`). This is
   negligible against LLM latency, but it adds up in fine-grained graphs and in tool-heavy loops.
3. **Sync/async duplication.**
   - Nearly every behaviour is written twice: `run`/`arun`, `_generate_with_cache`/`_agenerate_with_cache`
     (`1892`/`2051`), `stream`/`astream`, and every callback manager.
   - Bridging has to spin event loops in threads (`manager.py:355-400`) or pump iterators through
     executors one item at a time (`chat_models.py:2290-2311`).
   - The two paths diverge in subtle ways. Async-handler errors are swallowed on the sync path,
     which the code itself acknowledges at `manager.py:371-375`.
4. **Pydantic coupling.**
   - Schemas come from pydantic. Tool inference relies on a *deprecated* pydantic API under
     warning suppression (`tools/base.py:294-303`). `model_json_schema` is monkey-patched for
     caching (`404-431`). Pydantic v1 compatibility code remains in 9 modules.
   - `configurable_fields` re-instantiates models per call (`configurable.py:448-456`).
   - Message validators do real work (tool-call parsing) during construction (`ai.py:508`).
   - None of this ports: TypeScript and Rust have no pydantic, so LangChain.js re-implements
     everything and the two are bridged by the hand-written class-path table (`load/mapping.py:986`).
5. **Implicit, type-driven semantics.**
   - Stream aggregation is "whatever `+` does, else last chunk wins" (`base.py:2572-2590`).
   - Dict merge sums ints and concatenates strings, with special-cased keys (`utils/_merge.py:59-90`).
   - Tool formats are detected by dict-key sniffing (`function_calling.py:412-446`).
   - Tool return type depends on whether the input had an ID (`tools/base.py:1416-1427`).
   - These rules are easy to use and hard to specify. A cross-language port would have to
     reverse-engineer them.
6. **Observers change behaviour.** Attaching a streaming callback handler switches `invoke` to
   `_stream` (`chat_models.py:585`). `stream()` skips the cache that `invoke()` uses
   (`727-856` vs `1899`). Callbacks are both the tracing bus and the streaming mechanism, so the
   control plane and the observation plane are entangled.
7. **Callback surface area.** About 20 handler methods × 2, 7 `ignore_*` flags, inheritable vs local
   handlers, tags and metadata, 8 run-manager classes × 2, and the `on_chat_model_start` →
   `on_llm_start` fallback. Errors are swallowed by default, so a broken tracer fails silently.
8. **Version layering and churn.**
   - Streaming APIs stack up: `astream_log`, events v1, v2, v3, with a "v2 handler" feeding "v3".
   - `output_version` v0/v1 means `content` has two legal shapes.
   - There are two `ToolCall` definitions and two content-block definitions (core vs protocol)
     that already differ.
   - The protocol package is at 0.0.x and is still a hard dependency.
   - `TextAccessor` keeps `.text()` alive. There are 15 `@deprecated` decorators and 11
     `warn_deprecated` calls in core.
9. **Vendor coupling.** The run model *is* `langsmith.RunTree`. `_configure` reads LangSmith's ambient
   context and auto-adds its tracer. Chat models compute `ls_*` params on every call, and the
   wire protocol has a `langsmith_tracer` field (`protocol.py:378-388`).
10. **Layering inversions.** Core defines the injection marker that LangGraph's `ToolRuntime`
    implements, and its docs point at it (`tools/base.py:1734-1755`). `RemoveMessage` and
    `checkpoint_ns` handling (`config.py:297`) exist in core for LangGraph's benefit. The v3 hooks
    name `CompiledGraph` in core error messages (`base.py:1609-1611`).
11. **Source introspection.** `RunnableLambda` parses user source with `ast` (`runnables/utils.py:369-386`).
    It breaks for lambdas defined in REPLs or notebooks, and it does not translate to other languages.

---

## 10. Implications for AgentBuild

### 10.1 Placement principle

LangChain's own trajectory (§8) is toward **schema-first, generated, language-neutral types**,
with SDK classes as views over them. AgentBuild should start there. Data contracts live in the
Rust core as `serde` types, published as JSON Schema, with Python (TypedDict/pydantic) and
TypeScript (types/zod) generated from them. Behaviour that calls user code or provider SDKs lives
in the SDKs.

### 10.2 Adopt / adapt / avoid

| Piece | Verdict | Reason |
|---|---|---|
| Content-block union (`text`, `reasoning`, `tool_call`, `invalid_tool_call`, `server_tool_call/result`, `image/audio/video/file`, `non_standard`) with `id`, `index`, `extras` | **Adopt** (core contract) | This is the best-researched part of LangChain. It is provider-neutral, extensible through `extras` and `non_standard`, and already converging with a CDDL spec. |
| Single message shape `{role, id, name?, content: Block[], tool_call_id?, status?, usage?, metadata}` | **Adapt** | Use one representation. Drop the `str` content, `additional_kwargs` and v0/v1 duality. Make `tool_calls` derivable from blocks, so it is not a second source of truth. |
| `invalid_tool_call` as data | **Adopt** | The agent loop can return parse errors to the model without exceptions. |
| `UsageMetadata` with cache/reasoning/audio details | **Adopt** (core) | It is small, additive, and maps to every provider. |
| `ToolMessage.artifact` and `status` | **Adopt** | Separating "what the model sees" from "what code gets" is valuable for agents. |
| Message-lifecycle events (`message-start` → `block-start/delta/finish` → `message-finish`) | **Adopt** (core contract) | Explicit block boundaries remove the `+`-merge heuristics. `finish` carries the finalized block, so consumers do not need to accumulate. |
| Tool events `started/output-delta/finished/error` keyed by `tool_call_id` | **Adopt** (core) | They fit node events, and `tool_call_id` correlates them with the model's request. |
| Lifecycle causation edges (`edge`/`send`/`toolCall`) and namespaced subgraph paths | **Adopt** | This is cheap to emit from the superstep engine and makes traces explainable. |
| Event envelope `{seq, event_id, namespace, channel, data}` with replay from `seq` | **Adopt** (core) | It answers open decision #5. Resumable streams fall out of the superstep log. |
| Chunk aggregation by `__add__` / generic `merge_dicts` | **Avoid** | It is implicit and type-driven, O(n²) in places, and unportable. Aggregate from explicit deltas per block index in Rust. |
| Universal Runnable algebra (`\|`, dict → parallel, lambda returning runnable) | **Avoid** | The workflow graph is our composition model, and a second algebra duplicates it with ~100 µs per node. At most, offer SDK sugar that compiles to graph nodes. |
| `invoke/stream/batch` on *every* object with fake defaults | **Avoid** | Define capabilities explicitly: a model either streams events or it does not, and the core adapts it once. |
| `RunnableConfig` merging (tags, metadata, run name, concurrency) | **Adapt** | Keep a small, typed `RunContext` owned by Rust: run IDs, parent, tags, metadata, deadline, cancel token. Do not use free-form keys moved into `configurable`. |
| ContextVar-based implicit parent propagation | **Adapt** (SDK) | It is useful ergonomically in Python (contextvars) and TS (AsyncLocalStorage). The authoritative parent ID still comes from the Rust engine when it calls a node. |
| `with_retry` (tenacity) / `with_fallbacks` | **Adapt** (core policy) | Retries and timeouts per node or per model call should be declarative policy data executed by Rust: max attempts, backoff, retryable `NodeError.kind`s. Fallback after the first streamed byte must be explicitly forbidden, as LangChain does. |
| `configurable_fields` (re-instantiating per call) | **Avoid** | Pass model and parameters as data in the request instead. |
| Chat-model provider interface | **Adapt** | It needs one required method: `stream(request) -> AsyncIterator<MessageEvent>`. Non-streaming providers are adapted by a core helper, like `message_to_events`. `invoke` = drain the stream. No callback- or observer-dependent path switching, and caching/rate-limits are wrappers. |
| `ModelRequest` as data `{model, messages, tools, tool_choice, response_format, params, provider_extras}` | **Adopt** (core contract) | It makes requests loggable, cacheable (hash the canonical JSON) and replayable, and it decides where providers live (decision #2) without changing the contract. |
| `bind_tools` / `with_structured_output` as model-wrapper methods | **Avoid** in core; SDK sugar only | Tools and the response schema are fields of `ModelRequest`. Structured output is a request mode plus validation, not a parser chain. |
| `ModelProfile` capability descriptor | **Adopt** (as data) | `tool_calling`, `structured_output`, modalities and limits let the engine pick strategies, for example native JSON mode versus a forced tool. |
| Request-count rate limiter | **Adapt** | Implement it in Rust, shared across languages and concurrent nodes. Add token-based budgets. |
| Response cache keyed by canonical request JSON | **Adapt** | Apply the same semantics to `stream` and `invoke`. LangChain's `stream` bypasses its cache. |
| Tool schema = `{name, description, input_schema (JSON Schema), output_schema?, extras}` | **Adopt** (core contract) | JSON Schema is the neutral format. Do not make the OpenAI function format the internal form; provider adapters translate at the edge. |
| Schema inference from signatures + docstrings, `Annotated` descriptions | **Adapt** (SDK) | This is needed for ergonomics. Python can use pydantic `TypeAdapter`/`create_model` directly (not deprecated APIs), and TS can use zod. Both emit JSON Schema into the contract. |
| Injected args hidden from the model schema | **Adopt** | Do it through one explicit `ToolContext` parameter (call ID, run context, state view, stream writer) defined by our SDK. Avoid a marker in one package and an implementation in another. |
| `ToolException` opt-in + `handle_tool_error` | **Adapt** | Map tool failures to `NodeError {message, kind, details}`. The agent layer decides by policy which `kind`s go back to the model as `status:"error"` results. |
| `response_format="content_and_artifact"` | **Adopt** (as the result shape) | `ToolResult {content: Block[], artifact?: JSON, status}`. |
| Callback handler class hierarchy (~20 methods, managers ×2) | **Avoid** | Emit one typed event stream from Rust, and implement tracers and loggers as subscribers. SDK "callbacks" are filters over that stream. |
| Run tree (`run_id`, `parent_run_id`, `trace_id`, `dotted_order`, UUIDv7) | **Adopt** (core) | Time-sortable IDs and a dotted path make ordering and subtree queries trivial. Keep it vendor-neutral, with an OpenTelemetry exporter as the default sink. |
| `astream_events` via injected callback + output tapping | **Avoid** | Events should be native to the engine: node start/finish per superstep, state `updates`, model and tool events. They should not be reconstructed from callbacks or change execution. |
| Custom events from user code (`dispatch_custom_event`) | **Adopt** | Use a `custom` channel with a namespace, written through the node's context. |
| Serializable / `lc` constructor manifests + allowlisted import | **Avoid** | Serialize *data* (messages, requests, state, checkpoints), never object graphs. That removes the whole SSRF/env-secret risk class documented in `load.py:1-133`. |
| CDDL/JSON-Schema → generated bindings | **Adopt** (process) | One source of truth for Rust, Python and TS. A conformance test checks that each SDK round-trips fixtures. |
| Prompt templates, legacy output parsers | **Avoid** in core | Plain functions are enough. A tiny SDK helper for message templating is optional. |
| Message utilities (`trim_messages`, `count_tokens_approximately`, `convert_to_openai_messages`) | **Adapt** (SDK or core helpers) | Trimming and windowing are agent essentials. They could live in Rust as pure functions over the message contract, shared by both SDKs. |
| `RemoveMessage` + reducer semantics | **Adapt** | This informs open decision #1. An `add_messages` reducer (append, replace by `id`, remove by `id`) should be a core reducer over the message contract. |

### 10.3 What lives where

**Rust core: language-neutral contracts (JSON Schema published, bindings generated)**
- `ContentBlock` (tagged union on `type`), `Message`, `ToolCall`, `InvalidToolCall`, `ToolResult`, `Usage`.
- `ToolSpec {name, description, input_schema, output_schema?, extras}`.
- `ModelRequest`, `ModelProfile`, and `MessageEvent` (start/block-start/delta/finish/message-finish/error).
- The event envelope `{seq, ts, run_id, parent_run_id, namespace[], channel, data}` with channels
  `lifecycle | updates | values | messages | tools | custom:*`, plus `Checkpoint {id, parent_id, step, source}`.
- Policies as data: `RetryPolicy`, `Timeout`, `RateLimit`, `CachePolicy`.
- Errors: extend `NodeError {message, kind, details}` with the stable codes we already have, and add
  model and tool error kinds.

**Rust core: behaviour**
- Delta → finalized-block accumulation, including partial-JSON tool args. Write it once, in O(n).
- Retry, timeout, cancel and rate-limit execution. Event fan-out with sequence numbers. Message
  reducers. Request canonicalization and hashing for caching.

**SDKs (Python and TS): idiomatic surface only**
- Decorators and schema inference to JSON Schema. `ToolContext` injection.
- Provider adapters, if decision #2 keeps them in the SDKs. Each one is a pure
  `ModelRequest → stream of MessageEvent` translator, testable with recorded fixtures.
- Typed projections over the event stream, in the style of `ChatModelStream.text`, `.tool_calls`
  and `.output`. Iterator and async-iterator ergonomics. Context-local propagation of the current
  run context.

### 10.4 Concrete lessons to carry into design docs

1. **Do not let observation change execution.** Pick streaming vs non-streaming from the request or
   the provider capability, never from which listeners are attached (contrast `chat_models.py:585`).
2. **Make streaming explicit.** Use block boundaries plus a finalize step, not an algebra over
   chunks. LangChain had to add `chunk_position="last"` and a finalize validator to approximate this.
3. **One representation, no output versions.** Version the schema itself (`schema_version` in the
   envelope) and keep the content field single-shaped.
4. **Write async once.** Make the core async (Tokio) and expose sync wrappers in SDKs only at the
   outer edge. Do not hand-maintain `run`/`arun` pairs.
5. **Keep tracing vendor-neutral and on by construction.** Every run already has IDs and parentage
   from the engine, and exporters (OTel, LangSmith, files) are subscribers.
6. **Treat serialized config as untrusted.** Never deserialize into constructors. Load only data
   contracts.

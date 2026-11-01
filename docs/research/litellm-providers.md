# Research: LiteLLM provider core (Rust rewrite + Python reference)

Status: research note (input to design docs, not a decision).
Studied: LiteLLM sparse checkout at commit `2a01fe6` in `research/repos/litellm/`
(MIT, except `enterprise/`, which is not checked out and not used).

**Method.** I read the source. Mechanisms are described in my own words; no code is copied
beyond identifiers and a few short fragments. Citation conventions:

- `rs:<crate>/…:line` is relative to `litellm-rust/crates/`.
- `py:…:line` is relative to the repo root (`litellm/…`).
- `data:…` is a data file at the repo root.
- Line numbers are for commit `2a01fe6`.

In the blueprint (§10), **[L]** marks something learned from LiteLLM and **[D]** marks our own
decision (often a deliberate departure).

---

## 0. TL;DR

1. **The Rust rewrite is a partial port under Python, not a standalone library.** Its chat path
   is text-only and non-streaming and *declines* (`Unsupported`) everything else so Python
   serves it; the Anthropic→OpenAI stream iterator is `todo!()`. The fully native path is the
   Anthropic **Messages** route, which mostly relays bytes. Translation knowledge (tools, images,
   thinking, tool-call streaming) still lives in Python (§1.1, §2).
2. **No single IR.** Rust is format-centric: thin OpenAI-chat types plus a rich, exhaustive
   Anthropic-Messages model used as the lingua franca for all Claude hosts. Every struct has a
   flattened `extra` map and `Recognized<T>` for lossless passthrough (§1.3).
3. **Provider = config trait** (`BaseConfig`/`BaseMessagesConfig`): URL, `validate_environment`
   → `AuthScheme`, `transform_request` → HTTP → `transform_response` or stream decoder; selected
   by a `match` on a provider enum (§1.4-1.5).
4. **OpenAI shape as lingua franca loses fidelity** in Python: block order, `strict`, server tools
   shown as client tool calls, citations/redacted thinking in untyped side fields, inconsistent
   tool-call indexes (§2.5).
5. **Streaming:** Rust has a spec-correct, property-tested SSE codec and a tiny
   `StreamTransformer` (transform/finish) abstraction; Python's Anthropic iterator parses lines ad
   hoc, maps every mid-stream error to 500, and relies on a chunk builder to merge deltas (§3).
6. **Errors:** Rust only distinguishes *where* (connect = never sent, network = maybe billed,
   HTTP status + 256-char body; headers incl. `Retry-After` dropped); Python classifies with
   status tables **plus substring heuristics** for context overflow, rate limits and content
   policy; Anthropic `overloaded` collapses into 500 (§4).
7. **Router is Python-only:** retries with per-exception policies, generic / context-window /
   content-policy fallbacks, per-minute failure cooldowns, pre-call context filtering from catalog
   limits, six load-balancing strategies, and no mid-stream splice after content (§5).
8. **Catalog:** 2.8 MB, 4,040 models, 139 providers, MIT; good for limits and prices, but
   capability flags are sparse (absent ≠ false). The Rust `model-catalog` crate is a tolerant,
   alias-aware reader that nothing uses yet (§6).
9. **Not usable as a dependency:** unpublished 0.1.0 crates; the base adapter trait drags AWS,
   GCP and Azure SDKs, secrets backends, a host machine and Python-compat crates (§8).
10. **Blueprint headline:** one light crate with a `Protocol` trait per wire format
    (`openai_chat` first, covering gateways and local servers via endpoint presets, then native
    `anthropic_messages`), translating our 0002 blocks directly (no OpenAI IR), adapters that
    only *classify* errors into 0003 kinds with dispatch state and `Retry-After`, retries and
    fallbacks in the core, a filtered LiteLLM-derived catalog with attribution, and ~10 direct
    dependencies (§10).

---

## 1. Overall Rust architecture

### 1.1 What the Rust rewrite actually is (status reality)

The Rust workspace is **not** a standalone provider library. It is an incremental port that runs
**underneath** the Python SDK and the Python/Rust gateway, and it **declines** anything it cannot
translate faithfully so that Python can serve it instead:

- The chat-completions adapter contract defines an `Unsupported(&'static str)` decline. Its doc
  comment says the core declines rather than guesses, and the host turns that into a transparent
  fallback to the Python implementation; acceptance is an **allowlist**
  (`rs:llms/src/base_llm/chat/transformation.rs:32-40`).
- The generic gate declines **any streaming request** and any parameter not in the provider's
  allowlist (`…/chat/transformation.rs:110-132`, streaming check at `:115-121`). The message gate
  accepts only `system|user|assistant` messages whose content is a string or text-only parts;
  tool calls, tool results and multimodal parts decline (`…:134-164`).
- The OpenAI-chat → Anthropic adapter forwards only `max_tokens`, `temperature`, `top_p`, `stop`
  (`rs:llms/src/anthropic/chat/transformation.rs:47-52`), and its response decoder declines any
  non-text block (`…:133-140`). Its streaming iterator is entirely `todo!()` stubs
  (`rs:llms/src/anthropic/chat/handler.rs:66-163`).
- The OpenAI-compatible adapter deliberately omits `tools` from its param allowlist and declines
  a response that carries `tool_calls` or `refusal` (`rs:llms/src/openai_like/chat/transformation.rs:28-57`,
  `:231-253`).
- `llms/AGENTS.md` lists the "known partial ports" (`rs:llms/AGENTS.md:113-118`), and
  `inference-chat/AGENTS.md:3` states chat completions is currently non-streaming.

The **fully native** path is the Anthropic **Messages** route (`/v1/messages`): Anthropic-shaped
in, Anthropic-shaped out, served by Anthropic, Bedrock InvokeModel, Vertex `rawPredict`, Azure AI
Foundry and DeepSeek (`rs:inference-messages/src/common_utils.rs:36-49`). Here Rust does shape
requests (thinking, betas, param dropping) and streams, but mostly by **relaying bytes**
(§3.3).

Consequence for us: the Rust code is valuable for **structure, typing discipline, framing and
transport**, but the **translation knowledge** (tools, images, thinking, tool-call streaming)
still lives in Python (§2).

### 1.2 Crate graph (provider core only)

```
                      gateway-inference / python-bridge   (hosts; not studied)
                                   │
        ┌──────────────┬───────────┼───────────────┬──────────────┐
  inference-chat  inference-messages  inference-responses   router (scaffold)
        └──────────────┴─────┬─────┴───────────────┘              │
                         inference  (RouteError, caching, outbound send, provider resolution)
                             │
                           llms  (adapter traits + per-provider transformations)
       ┌─────────┬─────────┬─┴──────┬───────────┬──────────┬───────────────┐
  llms-types  framer   http     auth(+aws/azure/gcp)  secrets  host  python-compat
   (pure data) (SSE,   (reqwest,                     (+5      (call   (Python repr/
               AWS ES)  TLS,                          backends) machine) pickle)
                        proxy)
  side crates, not wired into the call path: model-catalog, cost, token-counter
```

Evidence: `rs:llms/Cargo.toml` depends on `llms-types`, `core-utils`, `auth` with
`["aws","azure","gcp"]`, `auth-aws/azure/gcp`, `host`, `framer`, `http`, `secrets`,
`python-compat`, `router-types`. `rs:inference/Cargo.toml` adds `cache`, `cache-response`,
`tracing`. Nothing depends on `model-catalog` (only its own `Cargo.toml` names it). `cost` has
**no dependencies at all**. `router` depends on `config`, `inference`, `inference-messages`,
`router-types`.

Layering rules are written down (`rs:inference/AGENTS.md:137-186`): `gateway-inference` →
`inference-<fmt>` → `inference`; provider code never imports `inference*` (`:184`); each failure
is declared once by the lowest crate that raises it and nested upward (`:191-197`); inference
does not price, budget or rate-limit (`:211-214`).

### 1.3 Canonical request/response type

There is **no single internal IR**. The Rust design is **format-centric**:

- `llms-types/src/formats/<format>/` holds one typed model per *public API format*:
  `chat_completions`, `messages`, `responses`, plus OCR, batches, transcription
  (`rs:llms-types/AGENTS.md:9-23`). Types are named after the format, never the originator
  (`MessagesRequest`, not `AnthropicMessagesRequest`, `:20`).
- Each `inference-<fmt>` route accepts its own format and dispatches to providers that can be
  reached **from that format**. The chat route translates OpenAI-chat to each vendor and
  normalizes back to a LiteLLM-owned `ChatCompletionsResponse` (`:23`). The Messages route
  treats **Anthropic Messages as the lingua franca** for every Claude host, and adapters only
  rewrite URL/body/headers/stream framing (`rs:llms-types/src/formats/messages/AGENTS.md:3-15`).
- The **OpenAI chat format remains the cross-provider lingua franca** in practice: it is what the
  Python SDK speaks and what `completion()` returns, and the Rust chat types are deliberately
  *thin* (`ChatMessage { role: String, content: Option<Text|Parts(Vec<Value>)>, name, extra }`,
  `rs:llms-types/src/formats/chat_completions/request.rs:25-41`). By contrast the Messages types are
  rich and exhaustive: 27 content-part variants including server tools, MCP, compaction and
  thinking (`rs:llms-types/src/formats/messages/content.rs:148-179`).

Two typing patterns recur and are worth copying:

1. **Every struct carries `#[serde(flatten)] extra: Map<String, Value>`** (e.g. `TextBlock`,
   `ToolUseBlock`, `content.rs:181-250`), so unknown fields round-trip losslessly.
2. **`Recognized<T>`** = untagged enum `Known(T) | Unrecognized(Value)`
   (`rs:llms-types/src/recognized.rs:3-17`) for fields where permissive passthrough is the
   contract (tools, thinking config, effort; `formats/messages/request.rs:343-365`). The rule is
   that typing a field must neither reject previously accepted input nor accept previously
   rejected input (`rs:llms-types/AGENTS.md:52-54`).

Gaps: the content-part enum and `MessagesStreamEvent` (`formats/messages/streaming.rs:113`) have
**no catch-all variant**, so a brand-new block or event type fails to deserialize (the native
Anthropic stream path avoids this by relaying bytes, §3.3).

### 1.4 How a provider is defined

Python-mirroring "config" traits, one per API format, implemented by unit structs exposed as
`&'static dyn` constants.

**Chat** (`BaseConfig`, `rs:llms/src/base_llm/chat/transformation.rs:42-108`):
`secret_names`, `supported_openai_param_mappings` (OpenAI name → provider name pairs),
`get_complete_url(api_base, model, params, env_lookup)`, `transform_request(model, messages,
params) -> {body: Value, stream_shape}`, `transform_response(model, {body}) ->
ChatCompletionsResponse`, `model_response_iterator(shape) -> Option<ChatStream>` (None = no
streaming yet), `validate_environment(headers, api_key, …) -> {headers, AuthScheme}`,
`default_headers`, `config_params` (accepted but never serialized), `unsupported_reason`.

**Messages** (`BaseMessagesConfig`, `rs:llms/src/base_llm/messages/transformation.rs:10-78`):
`shape_request`, `get_complete_url(…, stream)`, `transform_anthropic_messages_request(req,
&MessagesTransformContext)`, `transform_anthropic_messages_response`, `secret_names`,
`validate_environment`, `stream_decoder() -> Option<fn(ByteStream) -> EventStream>` (None =
relay bytes untouched), `default_headers`, `request_headers(headers, &req)` (beta selection),
`wire_body(Value)` (e.g. pop `model` when the URL carries it, Vertex
`rs:llms/src/vertex_ai/messages/transformation.rs:139`).

The **call pipeline** (`rs:inference-chat/src/prepare.rs:24-139`, `handler.rs:17-136`):

1. resolve provider from model prefix / `custom_llm_provider` (`rs:inference/src/provider.rs:13-37`);
2. parse messages, reject empty, run `unsupported_reason` → `Unsupported` (`prepare.rs:48-76`);
3. `validate_environment` → `AuthScheme` (Forwarded / Credential{placement} / Token / GcpAccessToken /
   AwsSigV4, `rs:llms/src/base_llm/auth.rs:16-38`), merged with default headers;
4. `get_complete_url`, `transform_request`;
5. `resolve_auth` turns the scheme into headers or a SigV4 signer applied over the final bytes
   (`auth.rs:54-100`);
6. `before_provider_request` interceptor (host hook) may rewrite URL/headers/body;
7. optional response-cache lookup keyed on the effective wire request; send with a total timeout
   (default 600 s, `rs:inference-chat/src/constants.rs:3`);
8. non-2xx → `Transport::Http{status, body}`; 2xx → `after_provider_response` hook →
   `transform_response`.

Auth policy is chosen by the adapter and applied once by shared code; the "deployment credential
outranks a forwarded header, except a forwarded OAuth bearer" precedence is tested per provider
(`rs:llms/src/anthropic/chat/transformation.rs:176-195`).

### 1.5 Registration and selection

No registry object, no plugin trait objects at runtime: a `match` over the `LlmProviders` enum
returns a static config (`rs:inference-chat/src/common_utils.rs:15-38`: Anthropic, Bedrock,
OpenaiLike; `rs:inference-messages/src/common_utils.rs:36-49`: Anthropic, AzureAi, Bedrock,
Deepseek, VertexAi-only-if-model-contains-"claude"). The provider name comes from
`get_custom_llm_provider` in `core-utils` (prefix parsing such as `anthropic/claude-…`), a port of
Python's `get_llm_provider` (`rs:inference/src/provider.rs:13-37`).

### 1.6 ADRs and written conventions

- **ADR-001 "Only type requests inside Rust"** (`litellm-rust/docs/adrs/adr_001_only_type_inference_inside_rust.md:9-15`):
  Python passes raw args/kwargs over the bridge; Rust owns all typing at the boundary. Cost:
  Python interfaces are less self-describing.
- ADR-000 (`docs/adrs/AGENTS.md`) sets the ADR process. Only two ADRs exist; most decisions are
  encoded in `AGENTS.md` files instead.
- **Error rules** (`litellm-rust/AGENTS.md:27-35`): one `thiserror` enum per crate in
  `src/error.rs`; message templates live in `#[error(...)]`; the smallest set of
  **provider-neutral variants** that callers handle differently; provider names and guidance are
  data, not variants; wrap lower errors with `#[from]`/`#[source]`, never flatten to strings.
- **Data-type purity** (`rs:llms-types/AGENTS.md:32-48`): types may validate shape but never
  clamp, map finish reasons, normalize usage or translate; no I/O, env or clock.
- **Python pairs** (`rs:llms/AGENTS.md:98-105`): keep Python operation names; each provider writes
  its own `impl`; no cross-provider wrapper.
- **Tests**: inline `#[cfg(test)]`, `rstest` named cases, never loops over inputs
  (`litellm-rust/AGENTS.md:11-25`); `wiremock` for HTTP (`Cargo.toml:93`).

---

## 2. Translation: OpenAI chat ⇄ Anthropic Messages

### 2.1 Rust (text-only port)

The Rust chat→Anthropic translation (`rs:llms/src/anthropic/chat/transformation.rs`) handles only
plain-text conversations:

- `build_conversation` (core-utils prompt factory) folds system messages out and merges
  consecutive same-role turns; every text becomes a `{"type":"text"}` block; a trailing assistant
  prefill is right-stripped like Python (`anthropic_body`, `:234-277`; tests `:343-380`).
- `system` becomes a list of text blocks; caller params are spread last so a caller-supplied key of
  the same name wins (`:265-275`).
- A conversation that does not open on a user turn is **declined**, because Python only repairs it
  under `litellm.modify_params`, which Rust cannot observe (`:213-228`).
- Response: text blocks concatenated; stop reason through a shared table
  (`rs:core-utils/src/core_helpers.rs:10-28`: `end_turn|stop_sequence→stop`,
  `max_tokens|compaction→length`, `refusal|content_filter|guardrail_intervened→content_filter`,
  unknown→`stop`); usage via `usage_from_parts`, which **folds cache-read and cache-write tokens into
  `prompt_tokens`** and reports the split in `prompt_tokens_details` (`:32-49`).
- `top_k` is withheld on purpose because Python gates it per model later (`:37-46`).

### 2.2 Python `BaseConfig` contract

`py:litellm/llms/base_llm/chat/transformation.py` (505 lines). Abstract:
`get_supported_openai_params` (`:221`), `map_openai_params(non_default, optional, model,
drop_params)` (`:271`), `validate_environment` (`:281`), `transform_request` (`:348`),
`transform_response` (`:380`), `get_error_class(message, status, headers)` (`:408`).
Overridable hooks include `get_complete_url` (`:328`), `sign_request` (`:303`),
`async_transform_request` (`:359`; Anthropic overrides it to inline remote media),
`get_model_response_iterator` (`:412`), `should_fake_stream` (`:154`),
`_add_response_format_to_tools` (generic "JSON mode via a forced tool", `:225-269`),
`translate_developer_role_to_system_role` (`:185`), and a **retry-on-422 hook** that lets an
adapter rewrite the request and try again (`:196-219`). The pipeline is the same as Rust's:
`map_openai_params` → `validate_environment` → `get_complete_url` → `transform_request` →
HTTP → `transform_response` / iterator, wrapped by `CustomStreamWrapper` for streams.

In this section `T` = `py:litellm/llms/anthropic/chat/transformation.py`,
`H` = `…/anthropic/chat/handler.py`, `C` = `py:litellm/llms/anthropic/common_utils.py`.
The prompt factory (`anthropic_messages_pt`) and `utils.py` are **not** in the checkout, so
per-message conversion details are inferred from call sites and marked *(inferred)*.

### 2.3 Request: OpenAI chat → Anthropic Messages

| Concern | Mechanism | Cite |
|---|---|---|
| Supported params | `stream, stop, temperature, top_p, max_tokens, max_completion_tokens, tools, tool_choice, extra_headers, parallel_tool_calls, response_format, user, web_search_options, speed, context_management, cache_control`; `thinking`/`reasoning_effort` only for reasoning models | T:534-563 |
| Unsupported params | the Anthropic mapper silently ignores params it has no branch for; raising `UnsupportedParamsError` vs dropping is decided upstream in `utils.get_optional_params` (*not in checkout*) under `drop_params` | T:1541-1722 |
| System prompt | only the **leading run** of system messages is hoisted to top-level `system` (keeps the cached prefix stable); later ones go to a mid-conversation-system placer gated by a model capability. String → one text block with message-level `cache_control`; list parts keep per-part `cache_control` but **lose any other fields**; empty text skipped | T:2045-2058, T:1761-1815 |
| Tools | `function` → `{name, description, input_schema, type:"custom"}` after schema sanitizing; **`strict` is dropped**; extra keys carried: `cache_control, defer_loading, allowed_callers, input_examples, eager_input_streaming`; Anthropic hosted tools (web search, code execution, computer use, MCP `url`) pass through; OpenAI `mcp` tool mapped to `mcp_servers`; tools already in Anthropic shape pass through; anything else → `ValueError` | T:761-1041 |
| Tool names | rewritten to `^[a-zA-Z0-9_-]{1,128}$` with a collision-safe forward/reverse map stored per request; history `tool_calls` are rewritten too; responses reverse-mapped | T:117-265, T:1052-1203, T:2689-2696 |
| Tools present in history but none declared | inject a dummy tool so Anthropic accepts the `tool_use` blocks | T:1981-1982 |
| `tool_choice` | `auto→{auto}`, `required→{any}`, `none→{none}`, `{function:{name}}→{tool,name}`; `parallel_tool_calls=false` → `disable_parallel_tool_use: true` (creates `{auto}` if needed). Forced choice on models without forced tool use: downgrade to `auto` under `drop_params`, else raise | T:718-759, C:686-722 |
| Assistant `tool_calls` / tool results / turn merging / images / PDFs | done in `anthropic_messages_pt` *(inferred)*; thinking blocks are replayed verbatim at `content[0]` of the assistant turn with signatures (comment at `py:litellm/llms/anthropic/pass_through/adapters/transformation.py:592-596`); empty text and thinking blocks stripped (C:1699-1701) | T:2061-2070 |
| Remote images | async path inlines remote media only for `http://` URLs when configured; https presumably stays a URL source *(inferred)* | T:1945-1962 |
| Reasoning | `reasoning_effort` → `{type:"adaptive", display:"summarized"}` + `output_config.effort` on adaptive models, else `{type:"enabled", budget_tokens: tier}`; tiers default 128/1024/2048/4096/8192/16384 tokens and are env-overridable (Rust port of the same logic: `rs:llms/src/base_llm/messages/context.rs:78-107`); `thinking` silently dropped when history lacks thinking blocks, **only under `modify_params`** | T:1309-1367, T:1625-1693, T:1991-2002 |
| `response_format` | **native** `output_format: json_schema` when the catalog says `supports_native_structured_output` (schema `$defs` inlined, unsupported constraints moved into `description`, `additionalProperties:false` forced); else a forced **response-format tool**, not forced when thinking is on; `json_mode=true` marks the response for unwrapping | T:1596-1617, T:568-703, T:1434-1455 |
| `max_tokens` | `max_tokens` or `max_completion_tokens`; default = model's max output tokens from the catalog | T:1562, T:336-356 |
| `stop` | → `stop_sequences` (whitespace-only removed only under global `drop_params`) | T:1282-1300 |
| Sampling | `temperature/top_p/top_k` dropped (or raise) on models that removed sampling params; `temperature=1` still allowed | C:620-671 |
| `user` | → `metadata.user_id` unless it looks like an email/phone; `metadata` reduced to `user_id` | T:1618-1624, T:2104-2120 |
| `cache_control` | passed through on tools, system parts, content parts, top level; no beta header needed | T:1713-1715, T:887-900 |
| Betas | assembled in two stages from features in the request (computer use, files, MCP, tool search, effort, code execution, structured outputs, compaction, fast mode, …) plus caller-supplied `anthropic-beta`, then filtered per host | C:1152-1341, T:1907-1943, H:378-382 |

### 2.4 Response: Anthropic → OpenAI chat

- Text blocks **concatenated with no separator**; `content=None` if empty (T:2251-2252, T:2725).
- `tool_use` **and `server_tool_use`** → `tool_calls` with `arguments = json.dumps(input)`; the
  `index` is the **content-block position**, not the tool ordinal (T:2254-2259, T:358-386).
- `thinking`/`redacted_thinking` → `thinking_blocks` verbatim (signatures kept) +
  `reasoning_content` = concatenated thinking text (redacted adds nothing) (T:2277-2310).
- Server tool results, citations, compaction, container → `provider_specific_fields`
  (T:2262-2304, T:2577-2650).
- `finish_reason = map_finish_reason(stop_reason)` (table in absent `core_helpers.py`; Rust port
  at `rs:core-utils/src/core_helpers.rs:10-28`); in JSON mode forced to `stop` (T:2732-2741).
- JSON mode: if all tool calls are the response-format tool, its arguments become `content` (a
  `values` wrapper is unwrapped) and `tool_calls` is emptied (T:2198-2228, T:2829-2853).
- Usage: `prompt_tokens = input + cache_creation + cache_read`; details carry cached, creation
  (with 5m/1h split) and raw text tokens; reasoning tokens are taken from the response or
  **estimated with a tokenizer**; server-tool request counts copied (T:2431-2535, T:2344-2375).

### 2.5 Fidelity losses (OpenAI shape as lingua franca)

1. `strict` on function tools dropped (T:768-799).
2. **Server tools masquerade as client tool calls** (`server_tool_use` → `tool_calls`), results
   only in `provider_specific_fields`; an OpenAI-shaped agent loop may try to execute them
   (T:2254, H:839-842).
3. **Block order lost**: text, tool calls, thinking and citations go to separate fields
   (T:2252).
4. Tool-call `index` differs between streaming (ordinal, H:843) and non-streaming (block
   position, T:2257).
5. Redacted thinking absent from `reasoning_content`; streaming signatures arrive as separate
   empty-text thinking blocks that the chunk builder must merge (H:703-708).
6. Dropped: `tool_search_tool_result`, the Anthropic message `id` (stream id is regenerated,
   H:569), which stop sequence matched (T:2740).
7. Citations only per block in `provider_specific_fields` (T:2293-2304).
8. `metadata` reduced to `user_id`; email-like ids silently dropped (T:1618-1624).
9. System parts lose all fields except `type/text/cache_control` (T:1802-1807).
10. Silent request rewrites: thinking dropped or downgraded depending on model and history
    (T:1991-2002, T:1625-1655).
11. `prompt_tokens` includes cache tokens, so it is not Anthropic's `input_tokens`
    (T:2467-2473); consumers must read the details to price correctly.

These are exactly the losses our 0002 design avoids by keeping ordered content blocks,
`reasoning.signature`, `provider` blocks and `extras` (0002 §2, goals 2-3).

---

## 3. Streaming

### 3.1 SSE framing (`framer`)

`rs:framer` is a small, well-tested crate (174 lines of source):

- `frames(byte_stream, codec)` adapts any `Stream<Item=Result<Buf,E>>` into a
  `tokio_util::codec::FramedRead` (`rs:framer/src/framed.rs:10-21`), so the same driver serves
  SSE and AWS EventStream (feature-gated, `rs:framer/Cargo.toml` features `sse`, `aws`).
- `SseCodec` (`rs:framer/src/sse.rs`) implements the WHATWG rules: strips a BOM only at stream
  start (`:8`, `:49-61`), treats `\n`, `\r`, `\r\n` as terminators (`:70-85`), handles a CRLF split
  across chunks by rescanning the buffer from the start, joins multi-line `data:` with `\n`
  (`:137-142`), ignores comments (`:ping`), unknown fields and malformed `retry`, and **drops an
  unterminated trailing event at EOF** (`decode_eof` returns nothing, `:44-46`). It also has an
  encoder (`:151-170`) used to re-emit SSE.
- Tests include a **proptest round-trip of arbitrary events through arbitrary terminators and
  arbitrary chunk cuts** (`rs:framer/tests/sse.rs:53`), CRLF splits (`:116-119`), EOF cases
  (`:100-105`), and invalid UTF-8 ending the stream after earlier events (`:132`).

### 3.2 Decode → transform pipeline

`rs:llms/src/base_llm/base_model_iterator.rs:8-91` defines a tiny, reusable abstraction:

- `StreamTransformer { transform(&mut self, Input) -> Result<Vec<Output>>; finish(&mut self) ->
  Result<Vec<Output>> }`: a stateful, synchronous, 1→N mapper with an end-of-stream flush.
- `transform_stream(events, transformer)` drives it with `stream::unfold`; the **first error ends
  the stream** and `finish` is not called after an error (tests `:129-161`).
- `ChatStream::new(decode_fn, iterator)` pairs a wire decoder with an iterator
  (`rs:llms/src/base_llm/chat/streaming.rs:23-53`). `StreamShape` carries request-time decisions
  the iterator needs (json-mode, tool-name reverse map) so they are "settled while the request is
  built, never re-derived from the body" (`:13-21`).

### 3.3 Messages route: relay first, decode only when the wire differs

`rs:inference-messages/src/handler.rs:207-252`: when the adapter's `stream_decoder()` is `None`
(Anthropic, Vertex with `alt=sse`, Azure, DeepSeek), the response body is **relayed byte for
byte** (`response.chunk()` loop) with no parsing. Only Bedrock supplies a decoder: AWS EventStream
frames → base64 JSON chunks → typed `MessagesStreamEvent`, with Bedrock invocation metrics folded
into Anthropic usage, then re-encoded as Anthropic SSE
(`rs:llms/src/bedrock/messages/invoke_transformations/anthropic_claude3_transformation.rs:178-214`,
`rs:llms/src/base_llm/messages/streaming.rs:12-36`).

Consequences, pinned by tests:

- An HTTP error **before** the first byte fails the call without opening the stream
  (`rs:inference-messages/tests/messages/stream.rs:239`).
- A stream that ends **without `message_stop`** is relayed as is; Python's synthetic `api_error`
  for that case lives above this route (`:263-265`).
- The configured timeout is a **total** deadline that also covers a stalled stream body (`:308`);
  there is no separate idle/read timeout.
- Mid-stream Anthropic `error` events pass through as bytes; on the decode path, the first
  undecodable event ends the stream with `InvalidResponse` (`rs:inference-messages/src/handler.rs:288`).

### 3.4 Python (the real normalizer)

**Anthropic iterator** (`H` = `py:litellm/llms/anthropic/chat/handler.py:545-1280`):

- Line handling is ad hoc, not a spec-compliant SSE parser: bytes are trimmed to `data:`, only
  `data:` lines are parsed, `event:` and blank lines yield empty chunks (H:1186-1205). If JSON
  decoding fails once, the iterator switches **permanently** into a fragment-accumulation mode
  that buffers until a parse succeeds (H:1108-1164). (The Rust `SseCodec` makes this unnecessary.)
- `content_block_start` for `tool_use`/`server_tool_use` increments a 0-based `tool_index` and
  emits `{id, name, arguments:""}` (H:839-881). Each `input_json_delta` then emits
  `{id:None, name:None, arguments: partial_json, index: tool_index}`, **only if the current
  block is a tool block** (web-search result blocks also produce `input_json_delta`, H:671-687).
  A tool block with no deltas emits `arguments:"{}"` at `content_block_stop` (H:928-942).
- `thinking_delta` → `reasoning_content` + one-element `thinking_blocks`; `signature_delta` →
  an empty-text thinking block carrying only the signature (H:690-708).
- `message_delta` → `finish_reason` and **cumulative** usage merged over `message_start` usage,
  since `message_delta` may omit fields (H:635-646, H:1086-1106).
- **Mid-stream `error` events are always raised as `AnthropicError(status_code=500)`**, whatever
  the error type (`overloaded_error`, `rate_limit_error`, …) (H:1003-1012), although a proper
  type mapper exists elsewhere (C:88-101).
- `ping` and unknown event types yield empty chunks (no branch).

**Generic wrapper** (`py:litellm/litellm_core_utils/streaming_handler.py`, `CustomStreamWrapper`):

- Usage is forwarded only when `stream_options.include_usage` is true (`:421-422`); a
  provider's `finish_reason` is held back and re-emitted on a final chunk so usage can follow
  it (`:1137-1144`, `:1032-1068`).
- If the stream ends without a finish reason, a synthetic `stop` is emitted, upgraded to
  `tool_calls` if tool calls were seen (`finish_reason_handler`, `:1729-1742`).
- Guards: max total stream duration → `Timeout` (`:311-323`); **repetition detector** raises
  `InternalServerError` after N identical >2-char chunks to break model loops (`:450-488`).
- The assembled final response comes from `litellm.stream_chunk_builder` (`:1876`; module not
  in checkout), which merges tool-call deltas by `index` and concatenates arguments.
- On failure: per-chunk errors are mapped with `exception_type()` (`:1539-1548`); `ReadError` /
  `RemoteProtocolError` after a finish reason finalize gracefully, before it they are errors
  (`:2129-2132`); partial usage and cost are reconstructed from received chunks so failed streams
  are still billed (`:2260-2295`); non-429 4xx are raised directly, everything else becomes
  `MidStreamFallbackError(generated_content, is_pre_first_chunk)` for the router (§5)
  (`:2297-2387`).

**OpenAI-compatible streams** pass through `handle_openai_chat_completion_chunk`
(`:621-661`); tool-call deltas are already in OpenAI shape and are merged later by the chunk
builder.

---

## 4. Error mapping

### 4.1 Rust: transport-level only

The Rust core **does not classify provider errors**. It distinguishes *where* a failure
happened, not *what kind* it is:

- `rs:http/src/transport.rs:1-9`: `Http { status, body }` | `Network(String)` | `Connect(String)`.
  `Connect` means "before dispatch" (connect or builder error and not a timeout, `:12-20`), so the
  request **never reached the provider** and is safe to retry or reroute; `Network` (including
  timeouts) may have been received and billed. URLs are stripped from messages so query-string
  secrets do not leak (`:29-35`; test `:39-53`).
- Non-2xx responses become `Http { status, body: truncate_error_body(text) }`, truncated to **256
  characters** (`rs:http/src/request.rs:14`, `:56-62`; `rs:inference-messages/src/handler.rs:177-190`).
  **Response headers are not captured**, so `Retry-After` is lost on the Rust path; the bridge
  even sets `headers = []` on the Python exception (`rs:python-bridge/src/routes/inference.rs:134-140`).
- `RouteError` (`rs:inference/src/error.rs:13-42`) nests transport, auth, secrets, headers,
  `InvalidRequest/InvalidResponse(ErrorDetail)`, `Unsupported`, `PostCallHook`. `is_request()`
  (`:59-76`) answers one question: is the caller's request at fault?
- After a successful HTTP exchange, any normalization failure is re-tagged as
  `InvalidResponse` ("already billed; a host with a reference implementation must not retry")
  (`rs:inference-chat/src/handler.rs:131-149`). This "has the provider already been called?"
  distinction is the single most useful idea in the Rust error design.
- `rs:llms/src/error.rs:1-75` is the transformation error; `ErrorDetail::UnsupportedParameter`
  reproduces Python's message including the `litellm.drop_params = True` hint (`:54-62`).
- Classification then happens in Python: the bridge raises `RustUpstreamError(status, body)` and
  calls the Python `map_failure` (`rs:python-bridge/src/errors.rs:8-30`,
  `routes/inference.rs:142-147`). The gateway copies the upstream status into its own response
  (`rs:gateway-inference/src/error.rs:67-69`) and rebuilds an Anthropic or OpenAI error envelope.

### 4.2 Python: exception hierarchy

Note: this sparse checkout lacks `litellm/utils.py`, `litellm/constants.py`,
`litellm/router_utils/` and `litellm/router_strategy/`; claims about them are from call sites
only and marked *(call site)*.

`py:litellm/exceptions.py` subclasses the **OpenAI SDK exception types**, so callers can catch
either `litellm.RateLimitError` or `openai.RateLimitError`. Every class carries `message` (prefixed
`litellm.<Name>:`), `llm_provider`, `model`, `litellm_debug_info`, `max_retries`, `num_retries`.

| LiteLLM class | Base | Status | Notes |
|---|---|---|---|
| `AuthenticationError` | `openai.AuthenticationError` | 401 (`:143`) | `CallerCredentialAuthenticationError` subclass: per-user key, do **not** cool the shared deployment (`:175-178`) |
| `PermissionDeniedError` | `openai.PermissionDeniedError` | 403 (`:415`) | |
| `NotFoundError` | `openai.NotFoundError` | 404 (`:193`) | |
| `BadRequestError` | `openai.BadRequestError` | 400 (`:237`) | parent of the next four |
| ↳ `ContextWindowExceededError` | `BadRequestError` | 400 (`:547-569`) | a *subclass of bad request*, so naive handlers treat it as 400 |
| ↳ `ContentPolicyViolationError` | `BadRequestError` | 400 (`:674-699`) | `provider_specific_fields` |
| ↳ `PaymentRequiredError` | `BadRequestError` | 402 (`:588-614`) | |
| ↳ `UnsupportedParamsError` | `BadRequestError` | 400 (`:1022-1034`) | raised when `drop_params` is off |
| `UnprocessableEntityError` | `openai.UnprocessableEntityError` | 422 (`:327`) | |
| `Timeout` | `openai.APITimeoutError` | 408 or upstream (504 kept) (`:375`) | |
| `RateLimitError` | `openai.RateLimitError` | 429 (`:474`) | `category` (vendor vs LiteLLM limit), `rate_limit_type` (requests/tokens/…) (`:24-84`); vendor headers stay on `response.headers`, never copied to `headers` (header-injection defence, `:488-514`) |
| `InternalServerError` | `openai.InternalServerError` | 500 (`:820`) | Anthropic **overloaded (529) is mapped here** |
| `BadGatewayError` / `ServiceUnavailableError` | `openai.APIStatusError` | 502 / 503 (`:719-781`) | |
| `APIError` | `openai.APIError` | any (`:876-889`) | |
| `APIConnectionError` | `openai.APIConnectionError` | 500 (`:918-934`) | catch-all when no status |
| `MidStreamFallbackError` | `ServiceUnavailableError` | original or 503 (`:1207-1277`) | `original_exception`, `generated_content`, `is_pre_first_chunk` |
| `BudgetExceededError` | plain `Exception` | configurable (`:1076-1102`) | gateway budget |

Provider-side, adapters raise `BaseLLMException` (status, `status_code_is_synthesized`, message,
headers, request/response, body; `py:litellm/llms/base_llm/chat/transformation.py:41-66`);
Anthropic's subclass is `AnthropicError` (`py:litellm/llms/anthropic/common_utils.py:380-387`).
Anthropic rate-limit headers are renamed to `x-ratelimit-*` and every original header is kept
with an `llm_provider-` prefix (`common_utils.py:2091-2104`).

### 4.3 Python: `exception_type()` mapping

`py:litellm/litellm_core_utils/exception_mapping_utils.py:2394-2779` is a ~2,800-line
classifier:

1. Pass-through if the error is already a LiteLLM type (`:2402`).
2. A global **string** check raises `Timeout` for "Request timed out", "The read operation timed
   out", etc. before any provider logic (`:2486-2499`).
3. Per-provider branches (`:2511-2701`). OpenAI and every OpenAI-compatible provider share
   `_map_openai_exception` (`:294-552`); Anthropic has `_map_anthropic_exception` (`:555-665`).
4. Generic status table `_map_exception_by_status` (`:2273-2385`): 401 auth, 402 payment,
   403 permission, 404 not found, 408 timeout, 429 rate limit, 500 internal, 502 bad gateway,
   503 unavailable, 504 timeout, other <500 bad request, other ≥500 API error. Skipped when the
   status was synthesized (`:2285`).
5. No status at all → `APIConnectionError` (`:2725-2752`).

**Detection is substantially string matching** (`ExceptionCheckers`, `:41-136`):

- **Context window** (`:84-116`): 9 lowercase substrings ("this model's maximum context length
  is", "exceeds the available context size" for llama.cpp, Gemini's "exceeds the maximum number
  of tokens allowed", …) plus two compound patterns (Cerebras "current length is"/"while limit
  is"; "maximum input length is"+"tokens"), after excluding OpenAI's `user`-param length error.
  Anthropic adds "prompt is too long" and "prompt: length" (`:565-574`).
- **Rate limit** (`:47-81`): standalone `429` (only if the real status is unknown or 429, so an
  echoed prompt containing "429" inside a 400 does not match), regex `rate[\s_-]*limit`,
  Mistral's "service tier capacity exceeded".
- **Content policy:** OpenAI body `code == "cyber_policy"`, "content_policy_violation",
  "violating our usage policy", "rejected as a result of the safety system" (`:263-274`,
  `:368-372`); Azure 7 substrings (`:119-136`); Anthropic "content filtering policy" (`:587`).
- **OpenAI branch order** (`:294-552`): policy code → rate-limit string → context string →
  `model_not_found` → timeout string → content policy → encrypted-content → generic
  `invalid_request_error` → server-error strings → "Request too large" (mapped to **429**) →
  missing key → status table (note **422 → BadRequest**).
- **Anthropic branch** (`:555-665`): context strings → "overloaded_error"/"Overloaded" →
  **`InternalServerError`** → "Invalid API Key" → content-filter string → status table (400 and
  413 → bad request; **500 and 529 → internal server**).

Message extraction reads `body.message` then `body.error.message` (`:139-181`); response headers
are collected from `exc.headers` / `exc.response.headers` and attached as
`litellm_response_headers` for the router's backoff and cooldown logic (`:195-212`, `:2423`).

### 4.4 Retryability and `Retry-After`

- `litellm.should_retry(status)` *(call site)*: router comment "the pre-stream retry rule (408,
  409, 429, 5xx)" (`py:litellm/router.py:643`, used at `:650`, `:8017`, `:8298`).
- Sleep before a router retry (`router.py:8360-8403`): **0 if another healthy deployment exists**
  in the group; otherwise `litellm.calculate_retry_after(remaining, max_retries, headers,
  min_timeout=router.retry_after)` *(call site; body in `utils.py`)*. From memory of upstream
  (unverified here): `retry-after-ms`, then `retry-after` (seconds or HTTP date) honored if
  0 < x ≤ 60 s, else exponential from 0.5 s capped at 8 s with jitter.
- Cooldown length: deployment `cooldown_time` > Retry-After header > router default
  (`router.py:8661-8682`).
- SDK-level: `completion()` passes `max_retries` to the vendor SDK client (OpenAI SDK retries
  internally), but **forces `max_retries=0` when the router is calling**, so retries happen in
  exactly one layer (`py:litellm/main.py:5433-5438`).

### 4.5 Mapping to our 0003 §4 kinds

| 0003 `kind` | LiteLLM equivalent | Notes |
|---|---|---|
| `rate_limited` | `RateLimitError` (429, "rate limit" text, "Request too large") | LiteLLM separates vendor vs gateway limits via `category`; we could keep that in `provider_code` |
| `overloaded` | **none**: Anthropic 529/"overloaded" → `InternalServerError` | We keep it distinct (fallback trigger, different backoff) |
| `server` | `InternalServerError`, `BadGatewayError`, `ServiceUnavailableError`, `APIError ≥500` | |
| `timeout` | `Timeout` (408, 504, timeout strings) | 504 as timeout is a reasonable choice we can copy |
| `network` | `APIConnectionError`; Rust `Transport::Connect/Network` | Rust's connect-vs-network split = "safe to retry" vs "maybe billed" |
| `auth` | `AuthenticationError` | caller-credential subclass = do not penalize the deployment |
| `permission` | `PermissionDeniedError` | |
| `not_found` | `NotFoundError` (404, `model_not_found`) | |
| `invalid_request` | `BadRequestError`, `UnprocessableEntityError`, `UnsupportedParamsError` | |
| `context_overflow` | `ContextWindowExceededError` (string match on 400) | We need the same string table; it is the only portable signal |
| `content_filter` | `ContentPolicyViolationError` (+ `finish_reason=content_filter` on success can trigger a router fallback, `router.py:8838-8967`) | 0003 deliberately keeps "refused request" ≠ "declined in answer" |
| `unknown` | `APIError` without status / mapping failure fallback | |
| — | `PaymentRequiredError` (402) | We have no kind; propose mapping 402 → `auth`-like provider_fatal, or add `quota_exhausted` |

---

## 5. Router: retries, fallbacks, cooldowns, load balancing

**Rust:** a scaffold. `Router` is a `HashMap<public_name, Deployment>` built from the config
model list; the last duplicate wins (`rs:router/src/lib.rs:9-38`). The README is explicit:
"Multiple deployments per model group, routing strategies, retries, cooldowns, and fallbacks are
not implemented yet" (`rs:router/README.md:3`). `LitellmParams` already carries `timeout`,
`stream_timeout`, `max_retries`, `tpm`, `rpm`, `drop_params` etc. as data
(`rs:router-types/src/litellm_params.rs:21-53`). No retry loop exists anywhere in the Rust
provider core.

**Python** (`py:litellm/router.py`, 15,339 lines) is where all policy lives.

**Configuration** (`:935-1360`): `num_retries` (default falls back to `openai.DEFAULT_MAX_RETRIES`,
`:1215-1220`), `max_fallbacks` (`:1222-1227`), `retry_after` minimum wait (default 0, `:953`),
`retry_policy` (per-exception retry counts: BadRequest, Authentication, Timeout, RateLimit,
ContentPolicyViolation; `:1334-1344`, `:15225-15229`), `allowed_fails` / `allowed_fails_policy`
(per-exception failure budgets before cooldown, `:1348-1360`, `:15223-15276`), `cooldown_time`
(`:1199`), `fallbacks`, `default_fallbacks` (`{"*": …}`, `:1249-1254`),
`context_window_fallbacks` (`:1256`), `content_policy_fallbacks` (`:1258-1260`),
`routing_strategy` (default `simple-shuffle`, `:960`). Cooldown state lives in a `DualCache`
(in-memory + optional Redis) wrapped by `CooldownCache` (`:1117`, `:1200`).

**Retry loop** (`async_function_with_retries`, `:8027-8223`): first attempt; on error, a
per-deployment or per-exception `RetryPolicy` count can override `num_retries`
(`:8068-8116`); sleep (§4.4); loop re-checking `should_retry_this_error`; deployments that
returned non-retryable statuses are skipped on later attempts (`:8010-8024`). On exhaustion the
last exception is raised with `num_retries/max_retries` stamped on it.

`should_retry_this_error` (`:8265-8327`) **refuses to retry** (so fallbacks run instead) when:
context-window error and context-window fallbacks exist; content-policy error and content-policy
fallbacks exist; status not retryable (except 401/403 handled below); `NotFoundError`; rate limit
with 0 healthy deployments and fallbacks configured; auth/permission error on a group with ≤1
deployment; 0 healthy deployments.

**Fallbacks** (`async_function_with_fallbacks_common_utils`, `:7622-7889`), in order:
deployment `order` tiers → weighted intra-group failover → client-side fallbacks → for
`ContextWindowExceededError` the `context_window_fallbacks` map (else generic) → for
`ContentPolicyViolationError` the `content_policy_fallbacks` map → generic group fallbacks then
`"*"`. Bounded by `max_fallbacks` and a recursion depth (`:7659-7662`). If every fallback fails,
the **original** error is re-raised with a fallback-outcome note (`:7863-7889`). A successful
response with `finish_reason == "content_filter"` (or an Anthropic refusal) can itself be turned
into an error to trigger a content-policy fallback (`:8838-8967`).

**Cooldowns** (`deployment_callback_on_failure`, `:8592-8715`, registered as a failure callback
at `:1315-1323`): skipped for caller-scoped auth failures, caller-set timeouts and some internal
404s; otherwise increments a per-minute failure counter and cools the deployment for
`cooldown_time` or the Retry-After value. Thresholds (`allowed_fails` per minute, which statuses
cool immediately) live in `router_utils/cooldown_handlers.py` *(not in checkout)*.

**Healthy-deployment pipeline** (`async_get_healthy_deployments`, `:13495-13624`): team and
web-search filters → blocked/paused → health check → **cooldown filter** → callbacks →
`_pre_call_checks` → tags/plugins/order. `_pre_call_checks` (`:12942-13123`) counts tokens
once and **drops deployments whose `max_input_tokens` is too small** (context-window pre-check
from catalog data), drops over-RPM deployments, checks region and `response_format` support; if
nothing survives it raises `ContextWindowExceededError`, which then flows into
`context_window_fallbacks`.

**Load-balancing** strategies (`router_strategy/*`, not in checkout; wiring at `:135-156`,
`:1458-1463`, `:1588-1617`): `simple-shuffle` (weighted random by rpm/tpm/weight),
`least-busy`, `usage-based-routing` (v1/v2, tpm/rpm windows), `latency-based-routing`,
`cost-based-routing`, tag-based routing, budget limiter. Selection in
`_async_get_available_deployment` (`:13804-13939`).

**Mid-stream fallback** (`:3044-3205`): the stream wrapper converts a failure into
`MidStreamFallbackError`. **If any content was already emitted, the original error is re-raised
(no splicing)**; only pre-first-content failures fall back to another deployment, merging usage
and closing both streams in a shielded `finally` (`:3104-3199`). For `/v1/messages`, lifecycle
frames are buffered until the first content delta so a fallback is still possible
(`:699-713`).

---

## 6. Model catalog

### 6.1 The data file

`data:model_prices_and_context_window.json`: **2.8 MB, 71,904 lines, 4,042 top-level keys**
(4,040 models + `sample_spec` at line 2 + `fallback_generalizations` at line 59,558), **139
distinct `litellm_provider` values**, 220 distinct field names. Modes: chat 2,898,
image_generation 406, responses 185, embedding 151, audio_transcription 94, others.

Keys are model ids, optionally provider-prefixed (`claude-sonnet-4-5` at line 15,274,
`gpt-4o` at 33,132, `ollama/llama3` at 42,411, `azure/gpt-…`, `bedrock/…`). An entry is a flat
object. The fields we care about:

| Group | Fields |
|---|---|
| Identity | `litellm_provider`, `mode`, `deprecation_date`, `source` (pricing URL), `aliases` (Rust reader supports it; unused in this snapshot) |
| Limits | `max_input_tokens`, `max_output_tokens`, `max_tokens` (legacy; sample_spec says "set to max_output_tokens if provider specifies it") |
| Tools | `supports_function_calling`, `supports_parallel_function_calling`, `supports_tool_choice`, `supports_forced_tool_use`, `supports_parallel_tool_use_config`, `supports_tool_search` |
| Structured output | `supports_response_schema`, `supports_native_structured_output` |
| Modalities | `supports_vision`, `supports_image_input`, `supports_pdf_input`, `supports_audio_input/output`, `supports_video_input`, `supported_modalities`, `supported_output_modalities` |
| Reasoning | `supports_reasoning`, `supports_adaptive_thinking`, `supports_legacy_thinking`, `supports_{none,minimal,low,xhigh,max}_reasoning_effort`, `default_reasoning_effort` |
| Caching | `supports_prompt_caching`, `prompt_cache_min_tokens`, `cache_read_input_token_cost`, `cache_creation_input_token_cost(_above_1hr)` |
| Messages quirks | `supports_system_messages`, `supports_assistant_prefill`, `supports_sampling_params`, `supports_native_streaming` |
| Pricing | `input_cost_per_token`, `output_cost_per_token`, `output_cost_per_reasoning_token`, tiered `*_above_{128k,200k,272k}_tokens`, `*_batches`, `*_priority`, `*_flex`, per-image/audio/second/query costs |

The typed Rust mirror lists every field (`rs:model-catalog/src/model_info.rs:11-562`;
`supports_*` at `:476-562`). The JSON Schema
(`data:model_prices_and_context_window.schema.json`) says consumers "should ignore unknown
fields rather than reject them" because new optional fields are added regularly (its
`description`, line 4); `model-catalog/AGENTS.md:1-6` notes the intent to eventually remove the
schema file.

**Sparsity is the main caveat.** For the 2,898 chat entries:

| Field | true | false | absent |
|---|---|---|---|
| `supports_function_calling` | 2,071 | 68 | 759 |
| `supports_parallel_function_calling` | 448 | 45 | **2,405** |
| `supports_vision` | 1,131 | 290 | 1,477 |
| `supports_reasoning` | 1,335 | 17 | 1,546 |
| `supports_response_schema` | 1,322 | 57 | 1,519 |
| `supports_prompt_caching` | 1,001 | 36 | 1,861 |
| `max_input_tokens` | present 2,778 | | 120 |

"Absent" does **not** mean "unsupported": `claude-sonnet-4-5` has no
`supports_parallel_function_calling` although Anthropic supports parallel tool use, and
`ollama/llama3` has only limits and zero costs. So the file is good for **limits and prices**
and a **hint** for capabilities, but capability absence must be treated as *unknown*.

`fallback_generalizations` holds regex rules that map unknown ids to a family/provider (e.g.
bare `claude-…` → anthropic, Bedrock `anthropic.claude-…` → bedrock), anchored deliberately so
unknown namespaces stay unroutable.

`data:provider_endpoints_support.json` (189 providers) is a separate, much smaller matrix:
per provider, which LiteLLM endpoints are supported (`chat_completions`, `messages`,
`responses`, `embeddings`, …) plus display name and docs URL. Useful as documentation, not as
model capability data.

**Update cadence and loading.** There is no git history in this sparse checkout, so I cannot
measure cadence here. The schema text says fields are added regularly, and many entries carry
`deprecation_date` and per-tier pricing that change with vendor announcements; in upstream
LiteLLM the SDK by default fetches the latest copy from GitHub at import and falls back to the
bundled copy (`LITELLM_LOCAL_MODEL_COST_MAP=True` forces local). That loader lives in
`litellm/__init__.py`, which is **not in this checkout** (unverified here).

### 6.2 Rust `model-catalog` and `cost`

- `Catalog::parse(bytes, Provenance{source, revision, etag})` (`rs:model-catalog/src/catalog.rs:69-147`)
  reads the root as an ordered map, skips `sample_spec`, parses `fallback_generalizations`
  (`deny_unknown_fields` on the wrapper, `rs:model-catalog/src/fallback.rs:1-23`), keeps each
  entry both as **raw JSON fields** and as a typed `ModelInfo` (tolerant: unknown fields ignored),
  resolves `aliases` into a separate index while recording collisions as `AliasIssue`s instead of
  failing, and builds a lowercase index.
- `lookup(key)` (`:174-204`): exact key or alias first, then case-insensitive, then canonicalize
  alias → entry. No provider-prefix stripping and no regex fallback here.
- `validate(IntegrityLimits)` (`:149-172`) guards a **remote refresh**: fail if the model count is
  below a minimum or has shrunk below a ratio of a reference count, so a truncated download
  cannot wipe the catalog. `validate_registry` (`rs:model-catalog/src/validation.rs:17-34`) is
  the stricter CI check for the checked-in file.
- **Nothing in the call path uses this crate.** Capabilities for the Messages route come from the
  Python host (`litellm.rust_bridge.model_capabilities.anthropic_model_capabilities`) as a small
  struct (`rs:python-bridge/src/routes/messages/host.rs:216-224`,
  `rs:llms/src/base_llm/messages/context.rs:26-46`).
- `rs:cost` is a dependency-free pure calculator: `compile(&pricing)` → immutable plan →
  `calculate(&request)`, with tiered thresholds, service tiers, cache read/write rates and their
  fallbacks, off-peak windows, and a distinction between `Rate::Missing`, `Rate::Null` and
  `Rate::Value(0.0)` (`rs:cost/src/lib.rs:1-60`, `rs:cost/README.md`). It does not read the
  catalog; the caller maps fields to rates. Its reference fixture was generated from the Python
  implementation.
- `rs:token-counter` wraps tiktoken-rs and Hugging Face `tokenizers` (both heavy, feature-gated)
  behind a `Tokenizer` trait (`rs:token-counter/README.md:1-19`).

### 6.3 Can we consume it as data?

Yes. The file is outside `enterprise/` and therefore MIT (`data:LICENSE:1-13`, "Copyright (c)
2023 Berri AI"). Obligations: keep the copyright and permission notice with any copy or
substantial portion we ship (e.g. `THIRD_PARTY_NOTICES` + a header in the generated file).
Pricing and limits are facts, but the compilation is theirs; attribution is cheap and correct.
Practical concerns: 2.8 MB is too big to embed whole in every SDK wheel/package for little
benefit; we should **extract a filtered subset** (chat-mode entries of the providers we support)
at build time.

---

## 7. HTTP client (`rs:http`)

- **reqwest 0.12** with `default-features = false` and `json, multipart, rustls-tls, http2, stream`
  (`litellm-rust/Cargo.toml:89`); **rustls 0.23 with the `ring` provider** (`:95`). TLS config is
  built by hand and passed via `use_preconfigured_tls` (`rs:http/src/config.rs:100`).
- **Roots:** bundled `webpki-roots` by default (`rs:http/src/tls.rs:196`); a CA bundle path
  (`SSL_CERT_FILE`/`SSL_VERIFY=<path>`) replaces them; `SSL_VERIFY=false` disables verification;
  optional client certificate (mTLS); optional ECDH curve and TLS 1.2 cipher restrictions mapped
  from OpenSSL names, with unsupported tokens *reported*, not fatal (`config.rs:61-92`).
- **Defaults** (`rs:http/src/settings.rs:128-145`): connect timeout 10 s, pool idle timeout 120 s,
  **HTTP/1.1 only unless `LITELLM_HTTP2`** (`config.rs:116-120`), optional TCP keepalive, optional
  force-IPv4, user agent override. Settings are layered (highest precedence first) and read from
  env names that mirror Python's aiohttp knobs (`settings.rs:56-82`).
- **Request timeout:** per request, total (600 s default for chat and messages), applied on the
  request builder; no streaming idle timeout (§3.3).
- **Proxies:** resolved from `HTTPS_PROXY/ALL_PROXY/NO_PROXY` (lowercase wins, empty lowercase
  clears) and applied explicitly after `no_proxy()` so the process environment is never read
  implicitly (`rs:http/src/proxy.rs:17-36`, `config.rs:129-133`); a `trust_proxy_env` switch can
  disable them.
- **Pooling:** `HttpClientPool` caches one `reqwest::Client` per `(HttpClientConfig, variant)` with
  a 1-hour TTL; build failures are not cached (`rs:http/src/pool.rs:12-30`, tests `:211-330`).
  Variants: `Provider`, `NoRedirect`, `Media`, `UnpinnedMedia`.
- **Media fetcher:** SSRF-safe downloader for image URLs (private-network blocking unless allowed,
  max bytes, max redirects, injected DNS resolver; `rs:http/src/media.rs:101-276`).
- Error bodies truncated to 256 chars; errors strip URLs (§4.1).

---

## 8. Coupling and weight

### 8.1 What the provider core drags in

| Dependency of `llms` / `inference*` | Why | Needed for OpenAI-compat + Anthropic only? |
|---|---|---|
| `auth` with `aws, azure, gcp` features always on (`rs:llms/Cargo.toml`) | `AuthScheme` names `SigV4Signer`, `AwsCredentialSource`, `VertexConfig` directly in the **base** layer (`rs:llms/src/base_llm/auth.rs:8-38`) | No. Pulls `aws-config`, `aws-sdk-sts`, `aws-sigv4`, `gcp_auth`, `google-cloud-auth`, `azure_core`, `azure_identity` |
| `secrets` (+ AWS, Google, HashiCorp Vault, Azure, CyberArk backends) | credential lookup by name through a `Secrets` source; `env_lookup` closures everywhere | No; env + explicit key is enough |
| `host` + `coroutine` | resumable "machine" protocol so Python and the HTTP gateway can drive the same call, interceptors, observation events (`rs:host/AGENTS.md`) | No; we have our own runtime and event model |
| `python-compat` | Python `repr`/truthiness so error messages match Python byte for byte (e.g. `rs:llms/src/anthropic/messages/thinking.rs:11`) | No |
| `router-types` | `LitellmParams` (deployment config) is an input to `BaseMessagesConfig::get_complete_url` (`rs:llms/src/base_llm/messages/transformation.rs:19-26`) | No |
| `cache`, `cache-response` (via `inference`) | response caching is built into the call runner (`rs:inference/src/caching.rs`) | No |
| `tracing` | spans for send/prepare | Optional |
| `framer` (with `aws` default feature → `aws-smithy-eventstream`) | SSE + AWS EventStream | SSE part only (`default-features=false, features=["sse"]`) |
| `http` (+ optional `rmcp`/a second reqwest 0.13 for MCP) | client pool, TLS, proxies, SSRF-safe media | Partly; ideas yes, crate no |

The workspace lock file has **734 packages** (`litellm-rust/Cargo.lock`). Even
`inference-chat`, which serves three providers, depends on `auth` with all cloud features,
`cache-response`, `host`, `secrets`.

### 8.2 What would have to be cut

To use only OpenAI-compatible + Anthropic: drop the AWS/GCP/Azure `AuthScheme` variants (or move
them behind features in a crate *above* the base trait), replace `Secrets`/`LitellmParams` inputs
with a plain config struct, remove `host`/`coroutine`/`python-compat`/caching from the call path,
and finish the parts that are stubs (`anthropic/chat/handler.rs` streaming, tool calls,
multimodal). At that point little of their code remains; the reusable pieces are `framer::sse`
(~170 lines), `base_model_iterator` (~90 lines), the error-taxonomy *ideas*, and the typed
Messages format in `llms-types` (which depends only on serde, serde_json, serde_with, strum,
indexmap, macro_rules_attribute).

### 8.3 Can we depend on these crates?

Not sensibly. They are `version = "0.1.0"`, **not published on crates.io**, have no stability
promise, and are explicitly "partial ports" tracking Python behavior. A git dependency would pin
us to a fast-moving monorepo (edition 2024, MSRV 1.88, `litellm-rust/Cargo.toml:6-7`) and its transitive
cloud SDKs. Copying is allowed by MIT with attribution, but the only parts worth copying verbatim
are small (SSE codec, stream driver); everything else is cheaper to write against our own 0002
types. **Recommendation: learn, don't link.**

---

## 9. Critical assessment

### 9.1 What is good

1. **Decline instead of guess.** The Rust core uses an allowlist and returns `Unsupported` for
   anything it has not seen, with tests that prove declines happen *before* the call
   (`rs:llms/src/base_llm/chat/transformation.rs:32-40`; tests in
   `anthropic/chat/transformation.rs:400-530`). For us: an adapter must emit a `warning` or an
   `invalid_request` error, never silently drop (0002 goal 2).
2. **"Already billed" vs "never sent."** `Connect` vs `Network` and the re-tagging of
   post-response failures as `InvalidResponse` (`rs:http/src/transport.rs:12-20`,
   `rs:inference-chat/src/handler.rs:131-149`) answer the question that matters for retry
   safety and fallback.
3. **Lossless typing.** `extra` maps on every struct plus `Recognized<T>` keep unknown fields and
   unknown enum values round-tripping (`rs:llms-types/src/recognized.rs`). Pure data crates with
   no I/O (`rs:llms-types/AGENTS.md:45-48`).
4. **Spec-correct SSE with property tests** (`rs:framer/tests/sse.rs:53`), and a tiny
   `StreamTransformer` abstraction with `finish()` for end-of-stream flushes.
5. **Relay when the wire already matches** (Anthropic → Anthropic is byte relay), decode only
   when it differs. Cheap and lossless.
6. **Auth as data:** adapters return an `AuthScheme`; one function applies it; signing happens
   over final bytes (`rs:llms/src/base_llm/auth.rs:16-100`).
7. **Single retry layer:** the router disables SDK retries (`py:litellm/main.py:5433-5438`).
8. **Router ideas worth keeping:** context-window and content-policy fallbacks as separate maps;
   pre-call context-window filtering using catalog limits; retry immediately when another
   healthy deployment exists; never splice a fallback stream after content was emitted
   (`py:litellm/router.py:3104-3113`); reconstruct partial usage from chunks for billing.
9. **Errors carry data, not prose** in Rust (`litellm-rust/AGENTS.md:27-35`), and URLs are
   stripped from transport errors.

### 9.2 What is debt

1. **OpenAI chat as lingua franca loses fidelity** (§2.5): block order, server tools as client
   tool calls, `strict`, citations and redacted thinking pushed into untyped
   `provider_specific_fields`, inconsistent tool-call `index`. The Rust rewrite implicitly admits
   this by making Anthropic Messages a *second* canonical format with rich types instead of
   extending the chat types.
2. **Python parity shapes the Rust design.** Error messages reproduce Python `repr`
   (`python-compat`), the drop-params hint text is pinned (`rs:llms/src/error.rs:54-62`),
   behaviors that depend on Python globals (`litellm.modify_params`, global `drop_params`) cause
   declines (`anthropic/chat/transformation.rs:213-228`), and thinking budgets are read from
   process env (`context.rs:91-107`). ADR-001 leaves Python callers untyped.
3. **Stringly-typed error classification.** Context-window, rate-limit and content-policy
   detection are substring tables (`exception_mapping_utils.py:41-136`); the OpenAI branch
   checks ~12 string conditions before looking at the status code (`:294-552`). It works because
   LiteLLM sees many providers, but it is order-sensitive and silent when vendors reword.
4. **Information destroyed at boundaries.** Anthropic `overloaded` (529) becomes
   `InternalServerError`; mid-stream `error` events of any type become status 500
   (`handler.py:1003-1012`); the Rust path truncates error bodies to 256 chars and **drops
   response headers including `Retry-After`** (`rs:python-bridge/src/routes/inference.rs:134-140`).
5. **Global mutable configuration** (`litellm.drop_params`, `litellm.modify_params`,
   `litellm.num_retries`) changes translation behavior per process, not per call.
6. **Capability data is sparse and implicit**: absence ≠ false (§6.1), and capability checks
   are scattered (`supports_forced_tool_use` in C:686-722, sampling removal in C:620-671), not
   declared by the adapter.
7. **Heavy, cyclic-feeling coupling**: the base adapter trait imports AWS/GCP auth types; the
   router depends on `inference-messages` (`rs:router/Cargo.toml`). The "light" path is not
   separable.
8. **Partial port with `todo!()`** in a public type (`anthropic/chat/handler.rs:66-163`) and an
   orphaned `model-catalog` crate.
9. **No idle timeout on streams** (one 600 s total deadline) in Rust; Python adds a max stream
   duration but also no per-chunk idle timeout in the code studied.
10. **Ad hoc SSE parsing in Python** with a permanent "fragment mode" fallback
    (`handler.py:1108-1164`): a symptom of not having a framing layer.

---

## 10. Blueprint for `criogen-providers`

This is a proposal for one crate, `criogen-providers`, that implements the 0002 §7 provider
contract and the 0003 §4 error kinds. It assumes the recommendation in `architecture.md` §8.1:
Rust adapters for the main providers, plus a user-implementable `Model` escape hatch in the SDKs.
Legend: **[L]** learned from LiteLLM, **[D]** our decision.

### 10.1 Principles

1. **[D] Our 0002 types are the only canonical shape.** No OpenAI-shaped IR in the middle.
   Each wire protocol translates 0002 ⇄ wire directly (LiteLLM's §2.5 losses come from routing
   everything through OpenAI chat).
2. **[L] Protocol ≠ provider.** One `openai_chat` protocol serves OpenAI, OpenRouter, Together,
   Groq, vLLM, LM Studio, Ollama's `/v1`, a LiteLLM proxy, any gateway; one `anthropic_messages`
   protocol serves Anthropic (and later Bedrock/Vertex hosts via different URL/auth/framing).
   Vendor differences are **endpoint data** (base URL, auth placement, headers, quirk flags),
   like LiteLLM's `openai_like` config and its Messages hosts
   (`rs:inference-messages/src/common_utils.rs:36-49`).
3. **[L] Decline, don't guess.** Anything the adapter cannot represent becomes either an explicit
   `warning` event (dropping, per 0002 goal 2) or an `invalid_request` `ModelError` *before*
   the HTTP call. Never silently drop.
4. **[L] Adapters classify, the core decides.** Adapters never retry, sleep or fall back; they
   return a classified `ModelError` with `retry_after` and dispatch state. Retry and fallback are
   0003 §5 policies executed once in the core (LiteLLM's single-retry-layer rule,
   `py:litellm/main.py:5433-5438`).
5. **[D] Lossless where the provider is the same.** Opaque data (signatures, server-tool blocks,
   unknown fields) round-trips through `extras.<provider>` and `provider` blocks
   ([L] `extra` maps + `Recognized<T>` pattern, `rs:llms-types/src/recognized.rs`).
6. **[D] Small dependency footprint**, no cloud SDKs, no tokenizers, no Python coupling.

### 10.2 Module layout

```
criogen-providers/
  src/
    lib.rs            pub use: Model, ProviderModel, Endpoint, ModelError, catalog
    model.rs          Model trait (invoke / stream) + ProviderModel (protocol + endpoint + http)
    endpoint.rs       Endpoint {protocol, base_url, model, auth, headers, quirks, caps_override}
                      + presets: openai, openrouter, ollama, vllm, lmstudio, anthropic, custom
    protocol.rs       Protocol + StreamDecoder traits, WireRequest, HttpFailure
    protocols/
      openai_chat/    encode.rs decode.rs stream.rs errors.rs quirks.rs
      anthropic/      encode.rs decode.rs stream.rs errors.rs betas.rs
    transport/
      client.rs       reqwest client builder (TLS roots, proxy, pool, timeouts)
      send.rs         send + timeouts (connect / first byte / idle / total), dispatch tracking
      sse.rs          incremental SSE parser (WHATWG rules)            [L framer]
      retry_after.rs  retry-after-ms / retry-after (secs | HTTP date)
    stream/
      assembler.rs    MessageDelta -> assembled Message (0002 §8), args parse at block end
      normalize.rs    tool-call id repair, duplicate ids, finish-reason mapping (0002 §7.1)
    error.rs          ModelError, ModelErrorKind (0003 §4), Dispatch
    capabilities.rs   Capabilities + resolution order
    catalog/          mod.rs (lookup) + models.json (generated subset, LiteLLM attribution)
  tests/              fixtures/<protocol>/<case>.{req.json,wire.json,sse,resp.json}
  xtask/catalog.rs    regenerate catalog subset from LiteLLM JSON
```

### 10.3 Trait sketch (our own code)

```rust
/// What the agent runtime calls. Implemented by ProviderModel, and by SDK-side user models.
#[async_trait]
pub trait Model: Send + Sync {
    fn capabilities(&self) -> &Capabilities;
    async fn invoke(&self, req: &ModelRequest, cx: &CallCx) -> Result<ModelResponse, ModelError>;
    fn stream(&self, req: &ModelRequest, cx: &CallCx)
        -> BoxStream<'static, Result<StreamItem, ModelError>>;
}

/// One wire protocol. Pure translation: no I/O, no clock, no env (LiteLLM llms-types rule).
pub trait Protocol: Send + Sync + 'static {
    fn name(&self) -> &'static str;                         // "openai_chat" | "anthropic_messages"
    fn base_capabilities(&self, ep: &Endpoint) -> Capabilities;
    /// 0002 request -> HTTP request parts. Warnings for explicit drops go into `out`.
    fn encode(&self, req: &ModelRequest, ep: &Endpoint, stream: bool, out: &mut Diagnostics)
        -> Result<WireRequest, ModelError>;                 // {path, headers, body: serde_json::Value}
    fn decode(&self, body: &[u8], ep: &Endpoint, out: &mut Diagnostics)
        -> Result<ModelResponse, ModelError>;
    fn stream_decoder(&self, ep: &Endpoint) -> Box<dyn StreamDecoder>;
    /// Map a non-2xx response or an in-band error object to a classified error.
    fn classify(&self, failure: &HttpFailure, ep: &Endpoint) -> ModelError;
}

/// Stateful, synchronous, 1 -> N; `finish` flushes at EOF.        [L StreamTransformer]
pub trait StreamDecoder: Send {
    fn on_event(&mut self, ev: SseEvent) -> Result<Vec<StreamItem>, ModelError>;
    fn finish(&mut self) -> Result<Vec<StreamItem>, ModelError>;   // error if no terminal event
}

pub enum StreamItem {
    Start { provider_message_id: Option<String> },
    Delta { index: u32, delta: BlockDelta },        // 0002 §8: text | reasoning | tool_call{args_fragment} | …
    BlockEnd { index: u32 },
    Usage(Usage),                                   // cumulative; last one wins
    Finish { reason: FinishReason, raw: Option<String> },
    Warning(Warning),
}

pub struct ModelError {
    pub kind: ModelErrorKind,           // 0003 §4
    pub status: Option<u16>,
    pub provider_code: Option<String>,  // e.g. "overloaded_error", "context_length_exceeded"
    pub retry_after: Option<Duration>,
    pub request_id: Option<String>,     // x-request-id / request-id
    pub dispatch: Dispatch,             // [L] Connect vs Network, billed re-tag
    pub message: String,                // sanitized, URL-free, bounded length
}
pub enum Dispatch { NotSent, Sent, Streaming { emitted_deltas: bool } }
```

The `ProviderModel` owns the only I/O path: `encode` → `transport::send` → status check →
`decode` or `stream_decoder` over `transport::sse`, feeding `stream::assembler`, which yields
0002 `message_delta` events and the final authoritative message (`message_completed`).

### 10.4 What to implement first

1. **Foundation [D]:** `ModelError` + classifier tables, transport (client, timeouts, dispatch
   tracking, Retry-After), SSE parser with proptest, delta assembler. No provider yet; tested with
   synthetic SSE.
2. **`openai_chat` [L openai_like, D on quirks]:** non-streaming and streaming; text, tools,
   tool results, parallel calls, usage via `stream_options.include_usage`; presets for OpenAI,
   OpenRouter, Ollama (`http://localhost:11434/v1`, empty key allowed [L
   `rs:llms/src/openai_like/common_utils.rs:6-23`]), vLLM, LM Studio, generic. This alone covers
   gateways and local servers.
3. **`anthropic_messages` native:** ordered blocks, thinking + signature round-trip, redacted
   thinking, `cache_control` via `extras.anthropic`, tool_result with images, streaming with block
   indexes, beta header assembly from request features [L `C:1152-1341`].
4. **Structured output** (`native` and `tool` strategies, 0004 §6), images/files, catalog subset
   and capability resolution.
5. **Hardening:** quirks registry fed by contract-test findings, idle timeouts, error-body corpus.
6. Later, behind features: OpenAI Responses API, Gemini, Bedrock/Vertex hosts for Anthropic
   (AWS EventStream framing [L framer `aws` feature]).

### 10.5 0002 → OpenAI chat completions

| 0002 | Wire | Notes |
|---|---|---|
| `system` message | `{role:"system"}` (quirk `system_role: developer` for o-series) | text blocks joined with `\n\n` |
| `user` text / image / audio / file | `content` parts: `text`, `image_url{url}` (base64 → data URL), `input_audio{data,format}`, `file{file_id \| file_data}` | unsupported part → `warning` + drop, or `invalid_request` if it would leave the message empty |
| `assistant` text + `tool_call` blocks | `content` (texts joined) + `tool_calls[{id, type:"function", function:{name, arguments: JSON string}}]` | block order is lost on this wire; `warning` only if text follows a tool call |
| `assistant` `reasoning` | dropped with `warning`; quirk `reasoning_echo: "reasoning_content"` for servers that want it back | signatures never sent cross-provider (0002 §9) |
| `assistant` `provider` block | dropped unless `provider == endpoint provider` | |
| `tool` message | `{role:"tool", tool_call_id, content}`; `json` → serialized text; `status: error` → text prefixed by the error | images in tool results not supported → moved into a following `user` message with `warning` (capability `tool_result_images=false`) |
| `tools` | `{type:"function", function:{name, description, parameters, strict?}}` | `strict` from `extras.openai` or output strategy |
| `tool_choice` | `"auto" \| "none" \| "required" \| {type:"function", function:{name}}` | |
| `settings.max_tokens` | `max_completion_tokens` (OpenAI) or `max_tokens` (quirk, default for compat) [L `openai_like:99-104`] | |
| `settings.parallel_tool_calls` | sent only when tools are present and quirk allows | some servers reject the field |
| `output {native}` | `response_format:{type:"json_schema", json_schema:{name, schema, strict:true}}` | |
| streaming | `stream:true, stream_options:{include_usage:true}` (quirk off for servers that reject it) | |
| `extras.openai` | merged into the body last (caller wins) [L `anthropic/chat/transformation.rs:265-275`] | |

Response side: `message.content` → text block; `refusal` → text block with
`extras.openai.refusal=true` and `finish_reason: content_filter` (0003: not an error);
`tool_calls` → `tool_call` blocks (unparseable or non-object args → `invalid_tool_call`);
`reasoning_content`/`reasoning` (gateway fields) → `reasoning` block **before** the text;
`finish_reason`: `stop→stop`, `length→length`, `tool_calls|function_call→tool_calls`,
`content_filter→content_filter`, else `other` with `raw_finish_reason`; null `*_tokens` → 0 [L
`openai_like:221-229`]; `prompt_tokens_details.cached_tokens` → `cache_read_tokens`,
`completion_tokens_details.reasoning_tokens` → `reasoning_tokens`.

**Usage semantics [D, aligned with LiteLLM]:** `input_tokens` = all input tokens processed,
including cache reads/writes; `cache_*_tokens` are subsets. For Anthropic we therefore add
`input_tokens + cache_read_input_tokens + cache_creation_input_tokens`
(same folding as `rs:core-utils/src/core_helpers.rs:32-49`). Document it in 0002 §1.

### 10.6 0002 → Anthropic Messages

| 0002 | Wire | Notes |
|---|---|---|
| leading `system` messages | top-level `system: [text blocks]`, `cache_control` from block `extras.anthropic` | [L] only the leading run is hoisted (`T:2045-2058`); a later system message → `invalid_request` by default [D] |
| `user` text / image / file | `text`; `image{source:{type:"base64",media_type,data} \| {type:"url",url}}`; PDF → `document`; `file_id` → `{type:"file"}` + files beta | |
| `assistant` blocks, **in order** | `reasoning` → `thinking{thinking, signature}` or `redacted_thinking{data}` (same provider only); `text`; `tool_call` → `tool_use{id, name, input}`; `provider` → raw `data` | order is preserved 1:1 |
| `tool` messages (consecutive) | one `user` turn of `tool_result{tool_use_id, content:[text\|image], is_error}`; a following `user` message is merged into the same turn | Anthropic requires alternation [L] |
| `tools` | `{name, description, input_schema, cache_control?}`; hosted tools via `extras.anthropic.tools` | |
| `tool_choice` | `auto→{auto}`, `none→{none}`, `required→{any}`, `{name}→{tool,name}`; `parallel_tool_calls:false` → `disable_parallel_tool_use:true` [L `T:718-759`] | |
| `settings.max_tokens` | required; default = catalog `max_output_tokens` capped at a configurable ceiling [L `T:336-356`] | |
| `settings.stop` | `stop_sequences` | |
| reasoning settings | `extras.anthropic.thinking` passthrough first; a neutral `settings.reasoning {effort}` mapping to budgets is an open question (§10.13) | [L] effort→budget tiers (`rs:…/messages/context.rs:78-89`) |
| `output {native}` | `output_format: json_schema` + beta when the catalog says native structured output; else forced tool (not forced while thinking) [L `T:1596-1617`] | |
| headers | `x-api-key`, `anthropic-version: 2023-06-01`, `anthropic-beta` assembled from used features + `extras` | |

Response: content blocks map **1:1 in order**; `citations` → `extras.anthropic.citations` on the
text block; `server_tool_use` / `*_tool_result` / unknown types → `provider` blocks (never
`tool_call`, so the runtime never executes a hosted tool, fixing LiteLLM loss §2.5-2);
`stop_reason`: `end_turn|stop_sequence → stop`, `max_tokens → length`, `tool_use → tool_calls`,
`refusal → content_filter`, `pause_turn` and others → `other` with raw kept; `stop_sequence`
value kept in `extras`; message `id` kept in metadata.

### 10.7 Streaming assembly

- **Framing [L]:** our own incremental SSE parser (WHATWG: `\n`/`\r`/`\r\n`, BOM, multi-line
  data, comments ignored, unterminated tail dropped), proptest round-trip with random chunk cuts
  as in `rs:framer/tests/sse.rs:53`. OpenAI's `data: [DONE]` ends the stream.
- **Anthropic:** wire block `index` == our block `index` (order preserved). `content_block_start`
  opens a block (tool_use carries `id`, `name`); `input_json_delta` → `args_fragment`;
  `thinking_delta`/`signature_delta` → reasoning delta (signature appended to the same block,
  not a new block, unlike `H:703-708`); `content_block_stop` → parse args (failure →
  `invalid_tool_call`); `message_delta` → finish + **cumulative** usage merged over
  `message_start` usage [L `H:635-646`]; `ping` ignored; **unknown event types ignored**
  (forward compatible) [D, vs. the closed Rust enum, §1.3]; `error` event → `ModelError`
  classified by `error.type` (`overloaded_error→overloaded`, `rate_limit_error→rate_limited`,
  `api_error→server`, …) with `dispatch=Streaming{emitted}` [D, fixing `H:1003-1012`].
- **OpenAI chat:** text deltas go to a text block; `delta.tool_calls[k]` keyed by `k` (the wire
  `index`) allocate one of our block indexes on first sight; first fragment carries `id`/`name`;
  repairs [D]: missing `index` → treat each id as a new call; missing `id` → `call_<uuid>`
  (0002 §7.1); a full call in one chunk (Ollama style) is fine; arguments arriving as an
  object rather than a string are re-serialized. `reasoning_content`/`reasoning` → reasoning
  block placed first. A final chunk with empty `choices` and `usage` is consumed. An in-band
  `{"error": …}` data object → classified `ModelError`.
- **End of stream:** no terminal event (`message_stop` / `[DONE]` / finish_reason) → `server`
  error with `dispatch=Streaming{emitted}` unless a finish reason was already seen [L: Python
  finalizes gracefully after finish, errors before, `streaming_handler.py:2129-2132`].
- **Truncation:** `finish_reason=length` while a tool call is open → `invalid_tool_call`
  `"truncated"` (0002 §7.1).
- **Timeouts [D, LiteLLM lacks the idle one]:** connect 10 s [L default], first byte = per-attempt
  timeout, **idle between chunks** (default 60 s), total per attempt 120 s (0003 §5.1).
- **[L] Repetition guard** (`streaming_handler.py:450-488`) is worth an optional middleware later,
  not v1.

### 10.8 Error classification (adapter → 0003 kinds)

Order of evaluation [D]: transport failure → in-band structured code → HTTP status → message
substrings (only to *refine* a 400/413 into `context_overflow` or `content_filter`).

| Signal | `kind` | Retry (0003) | `dispatch` |
|---|---|---|---|
| DNS / connect refused / TLS handshake failure | `network` | yes | `NotSent` [L `transport.rs:12-20`] |
| connection reset / body read error after send | `network` | yes | `Sent` |
| connect/first-byte/idle/total timeout | `timeout` | yes | `Sent` (connect timeout: `NotSent`) |
| 429 + `insufficient_quota` / billing code | `permission` (proposal: new `quota_exhausted`) | **no** | `Sent` |
| 429 | `rate_limited` + `retry_after` from `retry-after-ms` / `retry-after` | yes | `Sent` |
| 529, `overloaded_error`, "overloaded" | `overloaded` | yes | `Sent` |
| 500, 502, 503, 504, 409 (OpenAI lock conflict) | `server` | yes | `Sent` |
| 401 | `auth` | no | |
| 402 | `permission` (or `quota_exhausted`) | no | |
| 403 | `permission` | no | |
| 404, OpenAI `model_not_found`, Ollama "model not found" | `not_found` | no | |
| 400/413 + OpenAI `context_length_exceeded`, Anthropic "prompt is too long", llama.cpp "exceeds the available context size", vLLM/HF "maximum context length" etc. | `context_overflow` | no (hook) | [L substring table `exception_mapping_utils.py:84-116`] |
| 400 + `content_policy_violation` / safety-system text / Azure content filter | `content_filter` | no | [L `:119-136`, `:368-372`] |
| other 400, 422, 413 | `invalid_request` | no | |
| 2xx with unparseable body | `unknown` (proposal: `invalid_response`) | no (already billed) [L `handler.rs:131-149`] | `Sent` |
| anything else | `unknown` | no | |

Every error carries `request_id` (`x-request-id`, `request-id`), a sanitized message (URL-free,
bounded to ~2 KB, not 256 chars) and the raw provider `type`/`code` in `provider_code`.
Response headers are **kept** long enough to extract `retry_after` (fixing §4.1).

### 10.9 Retry and fallback placement

- **Adapter:** classification only (above). Never sleeps, never re-sends. The HTTP client's
  own retries are disabled.
- **Core (0003 §5.3 order: hooks → fallback chain → retry loop → per-attempt timeout →
  adapter):**
  - retry transient kinds with the 0003 backoff; honor `retry_after` up to `max_retry_after`;
  - [L] `dispatch=NotSent` errors are always safe to retry and fall back immediately;
  - [L] when a fallback target exists and the error is `rate_limited`/`overloaded`, prefer moving
    on without sleeping (LiteLLM sleeps 0 if another healthy deployment exists,
    `router.py:8360-8380`);
  - streaming failure after deltas: emit `message_aborted`, retry with a new `message_id`
    (0003 §5.2) — this is *more* permissive than LiteLLM, which refuses mid-stream fallback once
    content was emitted (`router.py:3104-3113`); it is safe for us because tools run only after
    `message_completed`;
  - [L] `fallback_on` per fallback entry may include `context_overflow` (a larger-context model)
    and `content_filter` (a different vendor), mirroring `context_window_fallbacks` and
    `content_policy_fallbacks`; default stays 0003's `overloaded, server, timeout`;
  - [L] optional pre-call context check: if catalog `max_input_tokens` is known and a cheap
    estimate (bytes/4) exceeds it by a margin, raise `context_overflow` before sending
    (`router.py:12942-13123` does the precise version with a tokenizer).
- **Not in v1 [D]:** cooldowns, load balancing, deployment health; these are gateway concerns.
  Users who need them point the `openai_chat` endpoint at a LiteLLM proxy or OpenRouter.

### 10.10 Catalog usage

- **[D] Generated subset:** an `xtask` reads LiteLLM's JSON (pinned commit or release), keeps
  `mode == "chat"` entries for providers we support (openai, anthropic, plus gateway-prefixed ids
  we choose), and emits `catalog/models.json` with only: limits, prices (input, output, cache
  read/write, reasoning), and capability fields as **tri-state** (`true/false/absent`). Expected
  size: tens of KB. A header records source commit, date and the MIT notice; the repo gets a
  `THIRD_PARTY_NOTICES` entry ("Model data derived from LiteLLM, Copyright (c) 2023 Berri AI, MIT").
- **[L] Tolerant reader:** unknown fields ignored; alias + case-insensitive lookup; integrity check
  (minimum count) when users load a newer copy (`rs:model-catalog/src/catalog.rs:149-204`).
- **Capability resolution [D]** (honoring 0002 §7 "declared by adapters, never guessed from model
  names"): `user override` > `endpoint preset override` > `protocol base capabilities ∧ catalog
  entry (exact id)` > conservative default. Absent catalog fields mean *unknown*: we attempt the
  feature and let the provider's `invalid_request` decide, instead of disabling it. No regex
  family guessing.
- **Uses:** default `max_tokens` for Anthropic; pre-call context check; `native` vs `tool`
  structured-output strategy; cost in `usage` metadata (pure function, [L] `rs:cost` design of
  `Missing` vs `Null` vs `0.0`).
- Users can load a fresher LiteLLM JSON at runtime (`Catalog::from_litellm_json(bytes)`) without
  a release.

### 10.11 Dependency budget

| Crate | Purpose | Note |
|---|---|---|
| `reqwest 0.12` (`default-features=false`, `rustls-tls`, `stream`, `http2` optional) | HTTP | [L] same choice |
| `rustls` (ring provider), `webpki-roots`, optional `rustls-native-certs` | TLS | native roots behind a flag for corporate proxies; CA bundle path supported [L] |
| `tokio` (rt, time, net), `futures-util`, `bytes` | async + streams | already in core |
| `serde`, `serde_json` | wire JSON | `preserve_order` not needed |
| `thiserror` | errors | |
| `httpdate` (tiny) | `Retry-After` HTTP-date | or hand-parse |
| `uuid` or core id generator | `call_<uuid>` repair | reuse core |
| `tracing` (optional feature) | spans | |
| dev: `wiremock`, `rstest`, `proptest`, `insta` | tests | [L] wiremock + rstest named cases |

No `eventsource-stream`: our SSE parser is ~150 lines and gets the proptest treatment. Excluded:
cloud SDKs, tokenizers, tiktoken, `tokio-util` codecs (optional), anything Python.

### 10.12 Test strategy

1. **Golden translation tests** (pure, fast): `fixtures/<protocol>/<case>.req.json` (0002
   ModelRequest) → expected `wire.json`; recorded wire response → expected 0002 ModelResponse.
   `insta` snapshots, `rstest` named cases [L], one case per mapping row in §10.5-10.6.
2. **Recorded streams:** raw SSE bytes captured once from live providers (scrubbed of keys and ids),
   replayed through the parser + decoder + assembler with **random chunk cuts** (proptest) and
   compared to the non-streamed expectation [L framer proptest].
3. **wiremock HTTP behaviors:** each row of §10.8 (status + body corpus → kind), `Retry-After`
   both forms, connect refused → `NotSent`, stalled body → idle timeout, stream without terminal
   event, mid-stream `error` event, in-band OpenAI error object, gzip, redirects disabled.
4. **Error-body corpus:** real error payloads from OpenAI, Anthropic, OpenRouter, vLLM, Ollama,
   llama.cpp kept as fixtures so classifier changes are regression-tested.
5. **Live contract tests** (nightly / manual, env-gated): two gateways (e.g. OpenRouter and a
   self-hosted LiteLLM proxy or vLLM) and **Ollama** (in CI via container with a small
   tool-capable model). Scenarios: plain text, single and parallel tool calls, tool-result loop,
   streamed tool arguments, image input where supported, structured output, usage present,
   cancel mid-stream. Assert **invariants** (valid transcript, args parse, usage > 0, finish
   reason), never exact text. Findings feed the quirks table.
6. **Cross-SDK parity:** the same fixtures run through the Python and TS bindings once they exist.

### 10.13 Open questions for the design docs

1. Add `quota_exhausted` and `invalid_response` kinds to 0003 §4, or map them to `permission` /
   `unknown`?
2. Neutral reasoning setting in 0002 `settings` (`reasoning: {effort | budget_tokens}`) vs.
   `extras` only.
3. Mid-conversation `system` messages for Anthropic: reject, convert to user text, or hoist?
4. Usage semantics (`input_tokens` includes cache tokens) to be written into 0002 §1.
5. Is OpenAI's Responses API a v1 protocol (it is where new OpenAI features land) or later?

# 0006: Provider layer and embeddable gateway

Status: **Approved; §11 steps 1–3 built.** Built so far in
`crates/criogen-providers`: the 0002 types, `ModelError` and the HTTP
error classifier (§6), the `Model` trait (§3), a `FakeModel` for tests, the
incremental SSE parser, the stream assembler (§5), the HTTP transport, the
`openai_chat` and `anthropic_messages` protocols with endpoint presets (§4)
and `ProviderModel`. Next: §11 step 4 (catalog subset, capability
resolution), then the gateway.

`openai_chat` as built: `OpenAiQuirks` (`max_tokens_field`, `stream_usage`,
`parallel_tool_calls`, `reasoning_effort`, `system_role`, `reasoning_echo`);
presets `Endpoint::{openai, openrouter, ollama, lmstudio, vllm,
openai_compatible}`. Invalid tool calls are sent back as calls with their raw
arguments, so their error results stay paired. Images in tool results move
to a user message after the tool run. The non-streaming decoder converts the
response into `StreamItem`s and runs the same `Assembler` as streaming, so id
repair and argument parsing are identical in both modes. `ModelResponse`
carries `warnings` (encode drops plus decode repairs). `Model::stream(request,
on_delta)` has a default (one delta per text block from `invoke`).
`preview_request` shows the exact body without sending it.

`anthropic_messages` as built (2026-10-12): `Protocol::AnthropicMessages(
AnthropicOptions)` with `version` (`anthropic-version`, default
`2023-06-01`), `default_max_tokens` (8,192; the API requires `max_tokens`)
and `betas`; presets `Endpoint::{anthropic, anthropic_compatible}`
(`x-api-key` auth, `POST {base_url}/messages`). Before building, the current
API was checked against Anthropic's documentation; three rows of research
§10.6 changed:

- **Reasoning.** `Reasoning::Effort` → `thinking: {type: "adaptive"}` plus
  `output_config.effort` (newer models reject the old form);
  `Reasoning::Budget` → `thinking: {type: "enabled", budget_tokens}` for
  older models (`max_tokens` must exceed it: `invalid_request` before
  sending). With reasoning on and no `max_tokens`, room is added to the
  default (effort: 2,048 / 8,192 / 24,576; budget: the budget).
- **Structured output.** Native output is `output_config.format:
  {type: "json_schema", schema}`, no beta flag. The `anthropic` preset
  declares native structured output, so agents use it (newer models
  reject forced tool choice).
- **Files.** `file_id` sources need no beta flag.

Other decisions: only leading system messages are sent (as `system`
blocks); a later one is `invalid_request`. Consecutive turns of one role are
merged, so tool results and a following user message form one user turn,
results first. Reasoning blocks are sent back exactly as received
(signature, redacted data, empty text); reasoning without a signature
(another provider's) is dropped with `reasoning_dropped`. Block `extras.
anthropic` fields (`cache_control`, `citations`) are copied onto the wire
block both ways; `ContentBlock::with_extra` sets them. `extras.anthropic` on
the request is merged into the body last, except `betas` (header) and
`tools` (hosted tool definitions, appended). Hosted tool blocks
(`server_tool_use`, `*_tool_result`, unknown types) become `provider` blocks
and are never run; streamed hosted inputs are reassembled. Usage: our
`input_tokens` = Anthropic `input_tokens` + cache reads + cache writes;
`output_tokens_details.thinking_tokens` → `reasoning_tokens`. Stop reasons:
`end_turn`/`stop_sequence` → `stop` (the sequence is kept in metadata),
`max_tokens`/`model_context_window_exceeded` → `length`, `tool_use` →
`tool_calls`, `refusal` → `content_filter`, others (`pause_turn`) → `other`.
Streaming: the wire `index` is our block index; a finished tool call's end
is held until the next block or the stop reason, so a call cut by
`max_tokens` is `truncated`; `ping` and unknown events are ignored; an
`error` event is classified with its type standing in for the HTTP status
(`api_error` → 500 → `server`, `overloaded_error` → 529, ...). The assembler
gained `StreamItem::Extras` (fields merged into a block at the end), used
for streamed citations.

A live check against a local Ollama found that its OpenAI endpoint answers an
unknown model with 404 and in-band type `api_error`; generic in-band types
(`api_error`, `server_error`) therefore no longer override the HTTP status.

The live check (`tests/live`, 2026-10-13) found that Gemini's
OpenAI-compatible endpoint wraps errors in a list
(`[{"error": {"code": 400, "status": "INVALID_ARGUMENT", "message"}}]`) and
reports a bad API key as HTTP 400 "Please pass a valid API key". The
classifier now reads the list shape, Google's `status` and
`details[].reason` codes (`API_KEY_INVALID`, `UNAUTHENTICATED`,
`PERMISSION_DENIED`, `RESOURCE_EXHAUSTED`, `DEADLINE_EXCEEDED`), and treats a
400 whose message says the key is invalid as `auth`. Without this a bad
Gemini key was `invalid_request`, so a fallback on `auth` never fired.

The live check against Ollama 0.6 with DeepSeek-R1 found the model's
reasoning printed inside the answer as `<think>…</think>` (servers without a
reasoning parser do this). `openai_chat` now moves `<think>` reasoning at
the start of the answer into a reasoning block, in whole and streamed
responses (tags may be cut anywhere across chunks; partial tags are held
back until decided). Only a leading block is read: a `<think>` later in the
text stays text. An unclosed block (cut by `max_tokens`) stays reasoning.
`OpenAiQuirks::think_tags` (default on) turns it off.

Transport as built: `Transport` (reqwest 0.13, rustls with the OS trust
store via the platform verifier, HTTP/2, `HTTP(S)_PROXY`), behind the default
feature `http`. `send` returns a whole 2xx body; `open` returns a `Body`
read chunk by chunk. Timeouts: connect (10 s), first byte (defaults to the
total), idle between chunks (60 s), total (120 s). Dispatch: connection
failures (refused, DNS, TLS, connect timeout) are `not_sent`; failures after
sending are `sent`; failures while reading a 2xx body are `streaming`.
Non-2xx bodies (up to 64 KB) go through `classify_http`. Error messages are
built without the URL (it may carry a key) and redacted with the endpoint's
secrets. Tests use a scripted local TCP server, not `wiremock`, so they can
stall, trickle and cut responses at exact points.

Streaming as built: decoders produce `StreamItem`s (`Start`, `Delta{index,
BlockDelta}`, `BlockEnd`, `Usage`, `Finish`, `Warning`); `BlockDelta` also has
a `Block` form for blocks that arrive whole (redacted reasoning, provider
blocks). The `Assembler` returns each delta as a `MessageDelta` (the
`message_delta` event) and, at the end, the authoritative message plus
warnings. Tool-call arguments are parsed once, at block end or at finish;
an open tool call when the finish reason is `length` becomes an
`invalid_tool_call` with error `truncated`. `Usage` items replace earlier
values field by field when non-zero. The SSE parser is tested with 2,000
pseudo-random chunk cuttings (own xorshift, no `proptest` dependency).

`Model::invoke` borrows the request (`&ModelRequest`) instead of taking it
by value, so retries and fallbacks do not copy the transcript. `stream()`
will be added with a default implementation (one delta per block from
`invoke`), so existing implementations keep compiling.

Implements the provider contract of
[0002 §7](0002-messages-and-tools.md) and the error kinds of
[0003 §4](0003-error-model.md). It serves both products (architecture §1): the
**agent engine** calls it in-process, and the **gateway** exposes it over HTTP.
Research: [litellm-providers](../research/litellm-providers.md) (the full
blueprint and mapping tables are in its §10, referenced below),
[llmgateway](../research/llmgateway.md), [build-analysis](../research/build-analysis.md).

Decision context (user, 2026-10-11): learn from LiteLLM (MIT) and rebuild only
the light provider core in Rust. LiteLLM's own Rust crates are unpublished and
couple the provider core to cloud SDKs, secrets and a Python host (734 packages
in their lockfile). llmgateway is AGPL-3.0: studied at the architecture level
only, never ported.

---

## 1. Principles

1. **Our 0002 types are the only canonical shape.** Each wire protocol
   translates 0002 ⇄ wire directly. LiteLLM routes everything through
   OpenAI-chat shapes and loses block order, `strict`, server-tool semantics
   and citations as a result (research §2.5).
2. **Protocol ≠ provider.** A *protocol* is a wire format; an *endpoint* is
   data: base URL, auth, headers and quirk flags. One `openai_chat` protocol
   serves OpenAI, OpenRouter, Groq, Together, vLLM, LM Studio, Ollama,
   a LiteLLM proxy and any gateway.
3. **Decline, don't guess.** Anything a protocol cannot represent is either
   dropped with a `warning` event or rejected as `invalid_request` **before**
   the HTTP call. Never silently.
4. **Adapters classify; the core decides.** Adapters never retry, sleep or fall
   back. They return a classified `ModelError`. Retry and fallback are the
   0003 §5 policies, executed once in the core.
5. **Lossless for the same provider.** Unknown fields, signatures and
   server-tool blocks round-trip through `extras.<provider>` and `provider`
   blocks (LiteLLM's `extra`-map pattern, research §1.3).
6. **Light.** About 10 direct dependencies. No cloud SDKs, tokenizers or
   Python coupling. Each protocol is a Cargo feature.

## 2. Crates

```
criogen-providers   library: Model trait, protocols, transport, catalog   (modes 1–2)
criogen-gateway     HTTP server on top: axum router + standalone binary   (modes 3–4)
```

The agent engine depends only on `criogen-providers`. The gateway crate is
optional and adds axum. Module layout: research §10.2.

## 3. Core types (our own design; sketch in research §10.3)

- **`Model` trait**, the seam the agent runtime calls:
  `capabilities()`, `invoke(req) -> ModelResponse`, `stream(req) -> Stream<StreamItem>`.
  It is implemented by `ProviderModel` (protocol + endpoint + HTTP client) and by
  user models written in Python/TS (the escape hatch).
- **`Protocol` trait**: pure translation with no I/O, clock or environment.
  `encode`, `decode`, `stream_decoder`, `classify`.
- **`StreamDecoder`**: stateful SSE event → `StreamItem`s, with `finish()` at
  end of stream.
- **`Endpoint`**: `{protocol, base_url, auth, headers, quirks, capability_overrides}`
  plus presets.
- **`ModelError`**: `kind` (0003 §4), `status`, `provider_code`, `retry_after`,
  `request_id`, `dispatch` (`NotSent` | `Sent` | `Streaming{emitted_deltas}`),
  and a sanitized `message` (≤ 2 KB).

`dispatch` is LiteLLM's best error idea (research §4.1). `NotSent` (DNS, connect
refused) is always safe to retry or fall back immediately. `Sent` may already
have been billed.

## 4. Protocols and endpoints

| Order | Protocol | Covers |
|---|---|---|
| 1 | `openai_chat` | OpenAI, OpenRouter, Groq, Together, Fireworks, vLLM, LM Studio, Ollama (`/v1`), LiteLLM proxy, llmgateway, Vercel AI Gateway, any OpenAI-compatible server |
| 2 | `anthropic_messages` | Anthropic, natively: ordered blocks, thinking signatures, `cache_control`, hosted tools as `provider` blocks |
| 3 | `openai_responses` | OpenAI (the `openai` preset since 0.1.6), and servers that implement `/responses`: §16 |
| later | `gemini`, Bedrock/Vertex hosts for Anthropic | behind features, driven by demand |

**Model strings.** `"<endpoint>:<model>"`, where the endpoint is a preset
(`openai`, `anthropic`, `openrouter`, `ollama`, `vllm`, `lmstudio`) or a name
the user registered:

```python
gw = Gateway(endpoints={
    "corp": OpenAICompatible(base_url="https://llm.corp.internal/v1", api_key_env="CORP_KEY"),
})
await gw.chat("corp:llama-4-70b", "hi")
```

No guessing the provider from the model name (LangChain does this,
[research](../research/langchain-agents.md) §4). An unknown endpoint prefix is an
`InvalidRequestError` listing the known endpoints.

**Mapping tables.** 0002 → OpenAI chat: research §10.5. 0002 → Anthropic
Messages: research §10.6. Both are normative for implementation. Each row
becomes a golden test case (§8).

### 4.1 Named providers (2026-11-04)

Research: [providers-2026.md](../research/providers-2026.md). One table,
`criogen_providers::PRESETS`, holds every provider Criogen knows by name:
its name in model strings, wire protocol, base URL, the environment variable
its key usually lives in, and its quirks. `Preset::endpoint(api_key)` builds
the `Endpoint`. Both SDK bridges and the gateway build named providers from
this table only, so a name means the same thing everywhere. The SDKs keep
their own name → variable table (they read the key before the engine
loads); a test in each SDK checks it against the engine's (`PRESETS` in the
Python module, `presets()` in the Node addon).

| Name | Provider | Base URL | Key |
|---|---|---|---|
| `openai` | OpenAI (Responses API, §16) | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| `anthropic` | Anthropic (Messages) | `https://api.anthropic.com/v1` | `ANTHROPIC_API_KEY` |
| `gemini` | Google Gemini | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` |
| `openrouter` | OpenRouter | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| `deepseek` | DeepSeek | `https://api.deepseek.com/v1` | `DEEPSEEK_API_KEY` |
| `mistral` | Mistral AI | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` |
| `xai` | xAI | `https://api.x.ai/v1` | `XAI_API_KEY` |
| `groq` | Groq | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` |
| `cerebras` | Cerebras | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` |
| `together` | Together AI | `https://api.together.xyz/v1` | `TOGETHER_API_KEY` |
| `fireworks` | Fireworks AI | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` |
| `qwen` | Qwen (Alibaba Cloud Model Studio, international) | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | `DASHSCOPE_API_KEY` |
| `moonshot` | Moonshot AI (Kimi) | `https://api.moonshot.ai/v1` | `MOONSHOT_API_KEY` |
| `zai` | Z.ai (GLM) | `https://api.z.ai/api/paas/v4` | `ZAI_API_KEY` |
| `ollama` | Ollama | `http://localhost:11434/v1` | none |
| `lmstudio` | LM Studio | `http://localhost:1234/v1` | none |

Quirks: `deepseek` and `moonshot` get earlier reasoning back as
`reasoning_content` on assistant messages (their thinking models need it
when they call tools); `mistral` and `cerebras` are sent no
`stream_options` (Cerebras documents a 400 for fields it does not support;
for Mistral this is caution), so their streams carry no token counts; `xai`,
`groq`, `together` and `fireworks` accept `parallel_tool_calls`. Everything
else keeps the OpenAI-compatible defaults.

Rules: a hosted provider without a key fails before sending (`invalid_request`)
and the message names the variable, never a value. A `base_url` overrides the
preset's (the China endpoints of Qwen, Kimi and GLM, a proxy); the key stays
the provider's. `openai_compatible` and `anthropic_compatible` (both SDKs, the
gateway) take any `base_url` for servers with no name here. Model names are
passed through unchanged; Criogen keeps no list of models.

Checked against scripted local servers (`tests/presets.rs`, both SDKs'
provider tests); not yet live against each provider (manual, like
`tests/live`). Not covered here, each with its own design note when built:
Azure OpenAI, Amazon Bedrock, Google Vertex AI. The OpenAI Responses API is
§16.

## 5. Streaming

- Our own incremental **SSE parser** (~150 lines, WHATWG rules), tested with
  random chunk cuts (property tests).
- Decoders produce `Start`, `Delta{index, …}`, `BlockEnd`, `Usage`,
  `Finish`, `Warning`. The shared **assembler** builds the 0002 message and
  emits `message_delta` / `message_completed` (0002 §8). Tool-call arguments
  are parsed **once, at block end**. LangChain re-parses partial JSON on every
  chunk, which is quadratic ([research](../research/langchain-core.md)).
- **Unknown event types are ignored**, so new provider events don't break us.
  In-band `error` events are classified as `ModelError` with
  `dispatch=Streaming{emitted}`.
- A stream that ends without a terminal event is a `server` error, unless a
  finish reason was already seen.
- **Timeouts:** connect 10 s; first byte = per-attempt timeout; **idle
  between chunks 60 s** (LiteLLM has none, so stalled streams hang); total per
  attempt 120 s.
- A failed stream after deltas were emitted triggers `message_aborted` and a
  retry with a new message id (0003 §5.2). This is safe for us because tools
  only run after `message_completed`.

## 6. Errors, retries and fallback

The classification order and table are in research §10.8: transport failure,
then in-band code, then HTTP status, then message substrings (used only to refine a
400/413 into `context_overflow` or `content_filter`). Response headers are kept
so `Retry-After` / `retry-after-ms` are honored. LiteLLM's Rust layer drops them.

Retry and fallback live in the core (0003 §5.3), with these refinements from
the research:

- `dispatch=NotSent` errors: retry or fall back immediately, no backoff.
- `rate_limited` / `overloaded` with a fallback configured: move to the fallback
  without sleeping.
- `fallback_on` may include `context_overflow` (a larger-context model) and
  `content_filter` (another vendor), in addition to the 0003 defaults.
- An optional pre-call context check: if the catalog knows `max_input_tokens`
  and a cheap estimate exceeds it, raise `context_overflow` before sending
  (no cost incurred).
- **Not in the library:** cooldowns, load balancing, deployment health. These
  are gateway concerns (§7), and users can also point an endpoint at an
  external gateway that has them.

## 7. The gateway (`criogen-gateway`)

A thin HTTP layer around the same library. Modes 3 and 4 (architecture §1).

```mermaid
flowchart LR
    C["Client<br/>(OpenAI SDK, Anthropic SDK, curl)"] --> R["axum router<br/>/v1/chat/completions<br/>/v1/messages<br/>/v1/models"]
    R --> IN["inbound codec<br/>wire → 0002"]
    IN --> PL["plugins (optional)<br/>keys · budgets · rate limits · logging"]
    PL --> P["criogen-providers<br/>retry · fallback · protocol adapters"]
    P --> UP[(Upstream providers)]
    P --> OUT["outbound codec<br/>0002 → client wire"]
    OUT --> C
    R -. "same protocol in and out<br/>+ no plugins rewriting the body" .-> RELAY["relay fast path<br/>(stream bytes through)"]
    RELAY --> UP
```

- **Inbound codecs** are the server-side mirror of each protocol: decode a
  client request into 0002 and encode the 0002 response into the client's wire
  format. A client can speak OpenAI while the upstream is Anthropic, and vice
  versa.
- **Relay fast path.** When the inbound and upstream protocols are the same
  and no plugin needs to rewrite the body, the gateway streams bytes through
  unchanged, observing usage from the stream as it passes. This is the main
  lever for low gateway overhead (LiteLLM's Rust layer relays for Anthropic,
  research §3.3).
- **Stateless by default.** Configuration comes from code (mode 3) or a YAML
  file (mode 4). Optional plugins, all off by default:
  - virtual API keys,
  - budgets and rate limits (in-memory, or Redis/Postgres plugins),
  - usage logging **off the critical path**: events go to a bounded queue and
    exporters drain it (idea from llmgateway, research §6),
  - sticky routing per session to keep provider prompt caches warm
    (llmgateway idea).
- **SSRF guard.** User-supplied URLs (image URLs, custom base URLs from
  clients) are validated against private address ranges unless allowed
  (llmgateway idea).
- **Targets, not claims:**
  - p50 added latency < 1 ms on the relay path and < 3 ms with translation;
  - memory < 50 MB idle.

  These are measured with a reproducible benchmark against the LiteLLM proxy
  before anything is published.

## 8. Testing (research §10.12)

1. **Golden translation tests**, one per mapping row (0002 ⇄ wire, both
   directions).
2. **Recorded SSE streams** replayed with random chunk cuts, compared with the
   non-streamed result.
3. **wiremock** for each error-classification row, both `Retry-After` forms,
   connect refused (`NotSent`), stalled stream (idle timeout), and streams
   without a terminal event.
4. **An error-body corpus** of real payloads from OpenAI, Anthropic, OpenRouter,
   vLLM, Ollama and llama.cpp.
5. **Live contract tests** (env-gated, nightly) against two gateways and Ollama.
   They assert invariants (valid transcript, parseable arguments, usage
   present), never exact text.
6. **Gateway compatibility:** the official OpenAI and Anthropic SDKs run against
   the gateway in CI.

## 9. Decisions on the research's open questions

| # | Question (research §10.13) | Decision |
|---|---|---|
| Q1 | New error kinds? | **Add `quota_exhausted`** (billing/quota; provider_fatal, never retried; a fallback to *another vendor* is allowed) and **`invalid_response`** (2xx with an unparseable body; not retried because it was already billed). 0003 §4 is updated |
| Q2 | Neutral reasoning setting? | **Yes:** `settings.reasoning = {effort: "low" \| "medium" \| "high"}` or `{budget_tokens: n}`, mapped per protocol; `extras.<provider>` overrides it |
| Q3 | System messages in the middle of an Anthropic conversation? | Leading system messages go to the top-level `system`. A later one becomes a user-turn text block prefixed `System note:`, with a `warning`. `strict_system=True` rejects it as `invalid_request` instead |
| Q4 | Usage semantics | `input_tokens` = all input processed **including** cache reads and writes; `cache_*_tokens` are subsets. 0002 §1 is updated |
| Q5 | OpenAI Responses API in v1? | **No.** It's the first protocol after the two v1 protocols, because new OpenAI features land there |

## 10. Model catalog

- **Source:** LiteLLM's `model_prices_and_context_window.json` (MIT, Copyright
  (c) 2023 Berri AI): 4,040 models across 139 providers.
- **What we ship:** a generated **subset** (tens of KB) for the providers we
  support. It contains limits, prices, and capabilities as **tri-state**
  values: `true` / `false` / absent = *unknown*. A missing flag does not mean
  "unsupported" (research §6.1). A `THIRD_PARTY_NOTICES` entry is added when
  the data is first used.
- **Capability resolution:** user override > endpoint preset override >
  protocol base capabilities combined with the exact catalog entry > a
  conservative default. **No name-pattern guessing.** For an unknown
  capability we attempt the feature and let the provider's
  `invalid_request` decide.
- **Uses:** default `max_tokens` for Anthropic, the pre-call context check,
  choosing the structured-output strategy (0004 §6), and cost in usage
  metadata.
- **Fresher data:** users can load a newer LiteLLM JSON at runtime
  (`Catalog::from_litellm_json`) without waiting for a release.

## 11. Implementation order

1. Foundation: `ModelError` + classifier, transport (timeouts, dispatch
   tracking, Retry-After), SSE parser, assembler. Tested with synthetic streams.
2. `openai_chat` with presets: covers gateways and local models.
3. `anthropic_messages`, native.
4. Structured output, images and files, catalog subset, capability resolution.
5. Python/TS `Gateway` bindings (mode 2).
6. `criogen-gateway`: router, inbound codecs, relay path, YAML config,
   benchmark vs LiteLLM proxy (modes 3–4).
7. Plugins (keys, budgets, logging, sticky routing), then `openai_responses`
   and Gemini.

## 12. The gateway as built (first slice)

Task 7 of the roadmap: §7's gateway, the part that lets any OpenAI client
use every provider Criogen speaks, with keys and usage records. The
relay fast path, `/v1/messages` inbound and the plugins beyond keys come
in later slices (§12.6).

### 12.1 Shape

`crates/criogen-gateway` (workspace member): a library that builds an
axum `Router`, and a binary `criogen-gateway` that serves one from a
TOML file. New dependencies, in this crate only: axum 0.8 (the server),
toml (the config file; TOML instead of §7's YAML because the maintained
YAML crates are in flux, and TOML is what Rust tools already use).

```rust
let gateway = Gateway::builder()
    .route("support", openai_model)                 // a name clients ask for
    .route_with_fallbacks("triage", [gemini_model, ollama_model])
    .key("team-a", secret, ["support"])             // virtual keys (optional)
    .usage(sender)                                  // usage records, off the request path
    .build()?;
axum::serve(listener, gateway.router()).await?;
```

```toml
# gateway.toml — `criogen-gateway --config gateway.toml --listen 127.0.0.1:4000`
[[routes]]
name = "support"
models = [{ provider = "openai", model = "gpt-5-mini", api_key_env = "OPENAI_API_KEY" },
          { provider = "ollama", model = "llama3.2" }]

[[keys]]
name = "team-a"
key_env = "GATEWAY_KEY_TEAM_A"
routes = ["support"]
```

### 12.2 Endpoints (OpenAI wire format)

| Endpoint | Does |
|---|---|
| `POST /v1/chat/completions` | Decodes the request into 0002 (messages, tools, tool choice, `response_format`, settings), calls the route's model, encodes the answer; `stream: true` sends `chat.completion.chunk` SSE events and `data: [DONE]`, with usage in the last chunk when `stream_options.include_usage` is set |
| `GET /v1/models` | The routes the key may use |
| `GET /health` | `ok` (no key needed) |

The upstream can speak another protocol: a client using the OpenAI SDK
reaches an Anthropic model through the Anthropic Messages adapter.

### 12.3 Keys

Without keys the gateway is open, and the binary refuses to listen on a
non-loopback address unless `--allow-open` is given. With keys, every
request needs `Authorization: Bearer <key>`; keys are compared by SHA-256
digest (the gateway keeps only digests) and name the routes they may use.
Unknown key: 401; route not allowed: 404 (the route is not revealed).

### 12.4 Fallback

A route lists models; the first is tried, and on a transient or
provider error (§6's fallback kinds: `rate_limited`, `overloaded`,
`server`, `timeout`, `network`, `quota_exhausted`) the next one, once
each. A stream falls back only before its first byte reaches the client.
Retries within a model are the client's business (OpenAI SDKs retry).

### 12.5 Errors and usage records

Errors use OpenAI's shape, `{"error": {"message", "type", "code"}}`, with
`code` the 0003 kind and a matching HTTP status (`rate_limited` 429,
`auth` 502 since the upstream key is the gateway's, `invalid_request` 400,
`context_overflow` 400, `timeout` 504, other upstream failures 502).
Messages never include upstream keys (0003 redaction).

Each request produces a `UsageRecord` (key name, route, provider/model
that answered, tokens, latency, outcome, fallbacks used): **no content**.
Records go to a bounded channel with `try_send`; a full channel drops the
record and counts it (`dropped_usage_records`), never slowing a request
(§7's off-the-critical-path rule).

### 12.6 Not built yet

The relay fast path, `/v1/messages` and Responses inbound, budgets, rate
limits, sticky routing, a model catalog endpoint with prices, the
benchmark against the LiteLLM proxy (roadmap task 10).

### 12.7 Tests

Gateway tests run the server on `127.0.0.1:0` with `FakeModel` upstreams
and call it with Criogen's own OpenAI-compatible client
(`ProviderModel`), so both codecs are exercised end to end, plus raw
HTTP for the wire details (SSE framing, `[DONE]`, error bodies, keys).
A manual check runs the official `openai` Python SDK against the gateway
with real OpenAI and Gemini upstreams.

## 13. The gateway, second slice: `/v1/messages`, rate limits, budgets

Roadmap task 18. §12 stays as built; this adds an Anthropic-format
endpoint and per-key limits. The relay fast path (§7) gets its own slice:
it bypasses the provider layer, and its only reason is latency, which §7
says must be measured against the LiteLLM proxy before it is claimed.

### 13.1 `POST /v1/messages` (Anthropic wire format)

Clients written for the Anthropic Messages API (the official SDKs, Claude
tooling) use the gateway unchanged; the upstream can be any provider, as
with `/v1/chat/completions`. The key may also come as `x-api-key`.

- **Request.** `model` (the route), `max_tokens` (required, as Anthropic
  requires it), `system` (text or text blocks), `messages` (`user` /
  `assistant`; content as text or blocks: `text`, `image` with a base64 or
  URL source, `tool_use`, `tool_result` with text or blocks and
  `is_error`), `tools` (`name`, `description`, `input_schema`),
  `tool_choice` (`auto`, `any`, `tool` with `name`, `none`),
  `temperature`, `stop_sequences`, `stream`. Other fields (`metadata`,
  `top_k`, `thinking`, …) are ignored, as an OpenAI-compatible upstream
  could not honour them.
- **Response.** `{"id": "msg_…", "type": "message", "role": "assistant",
  "model": <route>, "content": [text and tool_use blocks], "stop_reason":
  "end_turn" | "max_tokens" | "stop_sequence" | "tool_use",
  "stop_sequence": null, "usage": {"input_tokens", "output_tokens"}}`.
- **Streaming.** Server-sent events with event names, in Anthropic's
  order: `message_start`, then per block `content_block_start`,
  `content_block_delta` (`text_delta` or `input_json_delta`),
  `content_block_stop`, then `message_delta` (stop reason, output tokens)
  and `message_stop`. A failure after the stream started is an `error`
  event. Fallback works as in §12.4 (before the first byte only).
- **Errors** use Anthropic's shape, `{"type": "error", "error": {"type",
  "message"}}`, with the same HTTP statuses as §12.5; `type` follows the
  status (`invalid_request_error` 400, `authentication_error` 401,
  `not_found_error` 404, `rate_limit_error` 429, `overloaded_error` 503,
  `api_error` otherwise).

### 13.2 Rate limits (per key)

```toml
[[keys]]
name = "team-a"
key_env = "GATEWAY_KEY_TEAM_A"
routes = ["support"]
requests_per_minute = 60
tokens_per_minute = 100000
```

Token buckets that refill continuously (a full minute's allowance at
most). A request needs one request token and a non-negative token
balance; its tokens (input + output) are charged when the answer is
complete, so one request may take the balance below zero and the next
ones wait. Refused: 429, code `key_rate_limited`, with `Retry-After` (whole
seconds until a request would pass). OpenAI and Anthropic SDKs honour it.

### 13.3 Budgets (per key)

```toml
budget = { tokens = 5000000, period = "month" }   # "day", "month" or "total"
```

Input plus output tokens per UTC calendar day or month, or in total.
Checked before a request, charged after it (a request in flight may go
past the budget; the next one is refused). Refused: 429, code
`budget_exceeded`, with `Retry-After` until the period ends (none for
`total`). Budgets in money need prices per model (§10's catalog) and
come later.

Limits and spending live in memory: they start over when the gateway
restarts, and two gateway processes count separately. This is stated in
the guide; shared counters (Postgres, Redis) are a later plugin (§7).
`Gateway::spent(key)` reports a key's spending in the current period.

### 13.4 Usage records

A refused request also produces a `UsageRecord`, with zero tokens, an
empty `model` and `outcome` `key_rate_limited` or `budget_exceeded`.

### 13.5 Tests

On the paused clock: buckets refill at the configured rate; a refused
request carries `Retry-After`; tokens charged after an answer hold back
the next request; a budget refuses the request after the one that spent
it, and a new period (day) starts over. `/v1/messages`: Criogen's own
Anthropic client (`ProviderModel` with the Anthropic protocol) against the
gateway, so both codecs run end to end (text, tools, tool results,
streaming with tool calls), plus raw HTTP for event names, error bodies,
`x-api-key` and the missing `max_tokens`. Checked by hand (2026-10-22)
with the official `anthropic` Python SDK 2.1.0 through the gateway to a
fake OpenAI-compatible upstream: text, tool use, tool results, streamed
text and tool calls (`messages.stream`), and a rate-limit 429 that the SDK
raises as `RateLimitError` with `Retry-After`.

## 14. Gemini thought signatures on the OpenAI-compatible endpoint

The `gemini` preset reaches Gemini through Google's OpenAI-compatible
endpoint (§4). Gemini 3 models sign their reasoning: the first tool call
of each step carries `extra_content: {"google": {"thought_signature":
…}}`, and a later request that sends the call back without it fails with
400 (Google's thought-signature documentation). Without handling this, a
Gemini 3 agent fails at its second model call.

- **Kept.** `openai_chat` keeps any tool call's `extra_content` (whole and
  streamed) in the block's extras under the endpoint's name, and sends it
  back unchanged on the same endpoint. Other endpoints never see it
  (extras are keyed by provider).
- **Placeholder.** History from another model (a fallback from OpenAI to
  Gemini, a transcript written by hand) has no signature. With the new
  quirk `thought_signatures` (on in the `gemini` preset), an assistant
  message whose tool calls carry none gets Google's documented
  placeholder, `skip_thought_signature_validator`, on its first tool call.
  Google calls it a last resort that lowers quality; it is used only when
  the real signature does not exist.
- **Tests.** Decoding keeps the field (whole and streamed); encoding sends
  it back on the same endpoint and not on another; the placeholder
  appears only for the `gemini` quirk and only without a real signature.
  Live (2026-10-22, `gemini-3.8-flash`): every tool scenario of
  `tests/live` passes; without this change the first fails with
  "Function call is missing a thought_signature".
- **The gateway server** (§12–13) speaks OpenAI and Anthropic formats to
  its clients, which have no field for the signature, so a gateway route
  to Gemini 3 relies on the placeholder. Carrying signatures through the
  gateway (as `extra_content` on its OpenAI endpoint) is a later
  change.

## 15. Hardening from the security audit (October 2026)

**Providers.** Redirects are not followed (a redirected POST would carry the
prompt and non-standard auth headers such as `x-api-key` to another host; a
3xx is an `invalid_request` that says so). Credentials (the key, and header
values whose names look like credentials) are not sent over plain `http://`
to a host that is not this machine unless `Endpoint::allow_insecure_http`
(SDK: `allow_insecure_http` / `allowInsecureHttp`; gateway config:
`allow_insecure_http`); building the model fails otherwise. A body over
`HttpConfig::max_response_bytes` (64 MiB) fails with `invalid_response`,
which also bounds the SSE parser's line buffer and the assembled message.
`Debug` of `Endpoint`, `WireRequest`, `Transport` and `ProviderModel` shows
neither keys nor header values nor prompts. Error text is scrubbed of the
configured secrets and of key-shaped runs (`sk-`, `AIza`, `ghp_`, `AKIA`, …)
anywhere in the text, and no longer panics on non-ASCII after a URL.
`extras` cannot set `model`, `stream` or `messages` (warning
`extras_ignored`).

**Gateway.** A request's tokens (a third of its body bytes plus its
`max_tokens`, or 8192 without one) are reserved when it is admitted and
replaced by the real usage when the answer completes; a request dropped by
its client, or a stream that fails after starting, keeps the reservation.
Requests in flight are limited (default 1024 overall; `max_concurrent` per
key): over the limit is 429 `concurrency_limited` with `Retry-After: 1`.
`max_tokens` is lowered to a cap (default 200000). A stream's queue is
bounded (256 events); a client that stops reading is cut off and recorded
as `client_stalled`. Clients are shown the text of upstream errors they can
act on (invalid request, context overflow, content filter, rate limit,
timeout) and only the kind of the rest; the `x-criogen-model` header is
sent only with `expose_model_header`.

**Telemetry** (design 0009): with content capture off (now the default in
Rust too), error text, run metadata identities and free fields are not
exported either, only error codes.

## 16. The OpenAI Responses protocol (`openai_responses`, 2026-11-10)

Why: from GPT-5.4 on, OpenAI's Chat Completions refuses tool calls together
with a reasoning effort (research providers-2026 §1.4), so an agent on
`openai:` got newer models without reasoning. OpenAI's new features
(reasoning items, hosted tools) land in the Responses API only.

**Where.** `Protocol::OpenAiResponses(ResponsesOptions)`,
`POST {base_url}/responses`, `Authorization: Bearer …`.
`Endpoint::openai(key)` (the `openai` preset, in every SDK and the gateway)
now speaks it; `Endpoint::openai_chat(key)` keeps Chat Completions for
anyone who needs the old wire. `Endpoint::openai_responses_compatible(name,
base_url, key)` is for other servers that implement `/responses` (vLLM,
Ollama, OpenRouter, Azure's v1 API). Compatible servers in general keep
`openai_chat`: most of them implement only that.

**Privacy (rule 10).** `store: false` is always sent unless
`ResponsesOptions::store` is set: OpenAI keeps nothing of the conversation,
and `previous_response_id` is never used. The transcript is ours and is sent
in full each turn, as with the other protocols. Because nothing is stored,
reasoning has to travel with the transcript: the request asks for
`include: ["reasoning.encrypted_content"]` (`ResponsesOptions::
encrypted_reasoning`, default on) and the reasoning items come back opaque.

**Request mapping (0002 → Responses).**

| 0002 | Responses |
|---|---|
| system message | `instructions` when it is the first message; later system messages become `{"role": "developer"}` input items |
| user text / image (URL, base64) / file (id, base64) / audio | `input_text` / `input_image` (`image_url`, data URL) / `input_file` (`file_id`, `file_data`) / dropped with a warning |
| assistant text | `{"role": "assistant", "content": [{"type": "output_text", "text"}]}`; text and calls keep their order (no `block_order_lost`) |
| tool call | `{"type": "function_call", "call_id", "name", "arguments"}`; an invalid call is sent with its raw arguments, so its error result still matches |
| reasoning block from this endpoint | `{"type": "reasoning", "id", "summary": [summary_text…], "encrypted_content"}` from the block's extras (`extras[<endpoint>].reasoning`); reasoning from another provider is dropped (`reasoning_dropped`) |
| `provider` block from this endpoint | the item as it came (hosted tool calls such as `web_search_call`) |
| tool result | `{"type": "function_call_output", "call_id", "output"}`: a string, or `input_text` / `input_image` parts when the result has images (no `tool_result_images_moved`) |
| tools | `{"type": "function", "name", "description", "parameters"}`; hosted tools are dropped with a warning, as before |
| tool choice | `auto` (omitted) / `none` / `required` / `{"type": "function", "name"}` |
| `max_tokens`, `temperature`, `parallel_tool_calls` | `max_output_tokens`, `temperature`, `parallel_tool_calls` |
| `stop` | not supported by the API: dropped, warning `stop_dropped` |
| reasoning effort | `reasoning: {"effort", "summary": "auto"}`; a token budget is not supported: warning `reasoning_setting_dropped` |
| structured output (native) | `text: {"format": {"type": "json_schema", "name": "final_output", "schema", "strict": true}}` |
| `extras.openai` / `extras.<endpoint>` | merged last; `model`, `stream`, `input` cannot be set (`extras_ignored`) |

**Response mapping.** Output items in order: `reasoning` → a reasoning block
(text = the summaries joined, extras keep `id` and `encrypted_content`);
`message` → a text block per `output_text` part, a refusal as text with
`extras.openai.refusal` and finish `content_filter`; `function_call` → a
tool call (`call_id` is the id); any other item (hosted tool calls) → a
`provider` block. Finish: `completed` → `tool_calls` when there is a call,
else `stop`; `incomplete` with `max_output_tokens` → `length`, with
`content_filter` → `content_filter`; `failed` → the error classified as
usual. Usage: `input_tokens`, `output_tokens`,
`input_tokens_details.cached_tokens`, `output_tokens_details.
reasoning_tokens`.

**Streaming.** Semantic events (`response.created`, `…output_item.added`,
`…output_text.delta`, `…refusal.delta`, `…function_call_arguments.delta`,
`…reasoning_summary_text.delta`, `…reasoning_text.delta`,
`…output_item.done`, `response.completed` / `incomplete` / `failed`,
`error`). Blocks are keyed by `output_index` (and `content_index` inside a
message). A reasoning item's opaque fields arrive at `output_item.done` as
extras. The stream is done at `completed`, `incomplete` or `failed`; a
`failed` response or an `error` event is a classified `ModelError`.

**Tests.** Golden request bodies and decoding (`tests/openai_responses.rs`),
streamed and whole answers against the scripted local server, a reasoning
round trip, and the preset switch. Checked live once by hand
(`tests/live`), never in CI (rule 7).

## 17. Azure OpenAI and Microsoft Foundry (2026-11-10)

Why: one of the three enterprise routes (research providers-2026 §3, gap 3).
Hospitals often have Azure contracts, data residency in their region, and
policies that forbid long-lived keys.

**The API.** Azure's v1 API needs no `api-version`:
`https://<resource>.openai.azure.com/openai/v1/` (or
`https://<resource>.services.ai.azure.com/openai/v1/` for Foundry). It speaks
the Responses API and Chat Completions; the model name is the **deployment
name**. Foundry's non-OpenAI models (DeepSeek, Llama, Mistral, Grok) answer on
Chat Completions only.

**Endpoints (Rust).** `Endpoint::azure(resource_or_url, api_key)` speaks
Responses (§16); `Endpoint::azure_chat(…)` speaks Chat Completions. The first
argument is a resource name (`my-res` → `https://my-res.openai.azure.com/
openai/v1`) or a URL; `/openai/v1` is added when missing. The key goes in the
`api-key` header, not `Authorization` (`Endpoint::key_header`, a new field:
`None` means `Authorization: Bearer`). The name in messages and errors is
`azure`.

**Entra ID (no key).** `AzureModel` holds a credential and renews a bearer
token five minutes before it expires, as `KaggleModel` does (0020):

| Credential | How the token is obtained |
|---|---|
| `AzureCredential::client_secret(tenant, client_id, secret)` | `POST https://login.microsoftonline.com/<tenant>/oauth2/v2.0/token`, form body, `grant_type=client_credentials` |
| `AzureCredential::managed_identity(client_id?)` | the instance metadata service, `GET http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource=…` with `Metadata: true` (no credential is sent; the address is link-local) |
| `AzureCredential::token(bearer)` | a token obtained elsewhere (the Azure CLI, a sidecar); used as is, not renewed |

The scope is `https://cognitiveservices.azure.com/.default`
(`AzureModel::scope` changes it, e.g. to `https://ai.azure.com/.default`).
Token answers are bounded like every response (rule 11); `expires_in` is
capped at 24 hours and a token is never kept past it. The secret and the
token never appear in `Debug`, errors or logs (rule 10); the token endpoint
is https only, except on this machine (tests).

**From the environment.** `AzureModel::from_env(deployment)`:
`AZURE_OPENAI_ENDPOINT` (resource name or URL) is required; then
`AZURE_OPENAI_API_KEY`, else `AZURE_TENANT_ID` + `AZURE_CLIENT_ID` +
`AZURE_CLIENT_SECRET`. Managed identity is chosen explicitly only
(`AZURE_USE_MANAGED_IDENTITY=1`, or the SDK option), because probing the
metadata address from a laptop would hang until a timeout.

**SDKs and gateway.** Model string `"azure:<deployment>"` and `Azure(...)` /
`azure(...)` with `endpoint`, `api_key`, `tenant_id`, `client_id`,
`client_secret`, `managed_identity`, `api` (`"responses"` default, or
`"chat"`). Gateway: `provider = "azure"` with `base_url` and `api_key_env`,
or the tenant variables.

**Tests.** Against scripted local servers: the `api-key` header and path,
token minting and renewal, the managed-identity request shape, secrets kept
out of `Debug` and errors, URL forms. Not checked live (no Azure account in
the project); the status page says so until it is.

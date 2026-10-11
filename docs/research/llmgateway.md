# Research: LLM Gateway (theopenco/llmgateway), architecture study

Status: research note, not a decision. It is one input to the LLM gateway
comparison, alongside the LiteLLM notes.

> **License note: read this before using anything in this document.**
>
> `theopenco/llmgateway` is licensed under the **GNU AGPLv3**. The exception is
> everything under `ee/`, which is under a separate **commercial license**
> (`LICENSE:1-9`). AgentBuild is meant to be permissively licensed, so it
> **cannot copy, port, translate, or closely paraphrase** any llmgateway code.
> A Rust rewrite of AGPL TypeScript can still be a derivative work. AGPL's
> network clause also means that running a modified copy as a service obliges
> you to publish the source.
>
> This note is therefore an **architecture-level study only**. It covers which
> components exist, how a request moves between them, which features exist,
> and which design ideas are worth considering. The ideas in section 6 are
> written in our own words as general techniques. They are not descriptions
> of how llmgateway implements them. Nothing under `ee/` was read; only the
> directory names (`admin`, `audit`, `guardrails`) were listed.
>
> **How AgentBuild may use it:** only as an *external service*, meaning a
> separately deployed, unmodified llmgateway (or the hosted
> `api.llmgateway.io`) that AgentBuild calls over its OpenAI- or
> Anthropic-compatible HTTP API, like any other provider endpoint. AgentBuild
> must never vendor it, link it, or embed it.

**Source studied:** commit `ae99ddf` ("feat: add hourly prompt cache
analytics"), cloned at `research/repos/llmgateway/`. Paths below are relative
to that root. Most facts come from the repo's own prose (`README.md`,
`AGENTS.md`, `.agents/skills/*/SKILL.md`, `apps/docs/content/(gateway)/**`),
from manifests (`package.json`, `docker-compose.yml`, `infra/`), and from
file and symbol names. Source files were opened only to confirm structure.

---

## TL;DR

1. **It is a whole SaaS product, and the gateway is one part of it.** The
   monorepo has 9 apps and 10 packages, and the database schema has 145
   Postgres tables. The LLM request path (`apps/gateway`) is about 81k
   non-test lines of TypeScript. The dashboard, billing, management API,
   consumer chat app, coding-plan site, provider portal and mobile app add
   roughly 400k more.
2. **Chat Completions is the internal hub.** `/v1/messages` (Anthropic),
   `/v1/responses` (OpenAI Responses), the AI SDK protocol and parts of
   `/v1/images` convert the incoming request to chat-completions form. They
   then re-dispatch it *in-process* to `/v1/chat/completions` and convert the
   result back. Every surface therefore shares one routing, billing and
   logging path.
3. **The catalog has two levels and lives in code.** A *model*
   (canonical id, family, aliases) holds a list of *provider mappings*. Each
   mapping has the upstream model id, prices, limits and fine-grained
   capability flags. A separate *provider registry* holds credentials,
   regions, service tiers, cache idle-TTL and data-policy facts. The catalog
   has 53 providers, about 416 model entries and 739 mappings, and is written
   as TypeScript, not JSON.
4. **Routing is metrics-driven.** Each candidate (provider, region) is scored
   on price (adjusted for expected cache hits), uptime, throughput and
   latency, using a time-decayed metrics window. Further layers add sticky
   preference, session pinning for prompt-cache locality, a little
   exploration, rate-limit-aware re-routing, and cross-provider retry or
   fallback that depends on the error class.
5. **The hot path never reads the request log.** Logs are pushed to a Redis
   list. A separate worker batch-inserts them into Postgres, does billing,
   and builds the hourly aggregates that feed back into routing. Config
   reads go through a cache with a last-known-good copy kept in Redis.
6. **What AgentBuild should take from it is ideas only:** the
   model→mapping→provider catalog shape, a per-mapping "wire dialect" switch,
   error classes that drive fallback, session-sticky routing for prompt
   caching, and keeping usage and cost off the critical path. Sections 6 and
   7 cover these.

---

## 1. Monorepo layout

pnpm workspaces plus Turbo (`pnpm-workspace.yaml`, `turbo.json`). The
workspace globs include `ee/*`. Roles are summarized from `README.md`,
`AGENTS.md` ("Architecture Overview") and each app's `package.json`. LOC
counts non-test `.ts/.tsx` files.

| Workspace | Role | ~LOC | Essential to a gateway? |
|---|---|---:|---|
| `apps/gateway` | LLM data plane: `/v1/*` inference endpoints, routing, provider calls, streaming, usage and cost logging. Hono + Zod/OpenAPI on Node, port 4001. In production it is served as `api.llmgateway.io`. | 81k | **Yes, this is the core** |
| `apps/api` | Management/control-plane API: users, orgs, projects, keys, billing (Stripe), analytics. Hono, Better Auth with passkeys, port 4002 (`internal.llmgateway.io`). | 115k | Only for multi-tenant SaaS |
| `apps/worker` | Background loops: drain the log queue, batch-insert logs, credit and billing settlement, auto top-up, hourly stats aggregation (which feeds routing), retention cleanup, key expiry, model sync and verification, video jobs, alerts (`apps/worker/src/worker.ts`, e.g. `processLogQueue` at :2111, `batchProcessLogs` at :1108). | 15k | Partly (log sink and metrics) |
| `apps/ui` | Next.js dashboard. | 138k | Product |
| `apps/playground` | "Lounge", a consumer chat app built on Next.js. | 62k | Product |
| `apps/code` | Landing pages and dashboard for "DevPass" coding plans. | 28k | Product |
| `apps/airside` | Self-serve portal where inference providers file models and prices. | 18k | Product |
| `apps/docs`, `apps/mobile` | Fumadocs site; React Native Lounge app. | 4k / 27k | Product |
| `packages/models` | The static model and provider catalog (section 3). | 29k | **Yes** |
| `packages/actions` | Request and cost logic shared by gateway and worker: request-body preparation per provider, endpoint and header construction, provider scoring, cost calculation, image URL fetching guarded against SSRF, provider-key crypto. | 17k | **Yes** |
| `packages/db` | Drizzle schema (145 tables), migrations, cached-query helpers. | 18k | Partly |
| `packages/cache` | Redis clients (main and "storage"), response cache, log queue, SWR helper. | 0.5k | Yes |
| `packages/shared`, `logger`, `instrumentation` | Shared utilities and UI, logger, OpenTelemetry and metrics. | 33k / 0.3k / 0.8k | Partly |
| `packages/benchmarks`, `scripts`, `canvas` | Benchmarks, ops scripts, chart templates. | — | No |

How the pieces connect:

- The gateway and the API are two independent Hono services. They talk to
  each other only through the shared Postgres and Redis.
- The worker is the only process that consumes the log queue
  (`packages/cache/src/redis.ts:22-29`, Redis `LPUSH`/`LPOP` on a
  per-environment list).
- `docker-compose.yml` for local development runs Postgres 17, a cache and
  queue Redis, and a separate **"storage" Redis** with AOF persistence. The
  storage Redis holds Responses-API state (30-day retention) and the gateway
  response cache.
- `apps/gateway/package.json` depends on `@llmgateway/guardrails`. That is
  a workspace package resolved from `ee/` (we did not read it). The AGPL
  "core" gateway therefore builds against an `ee/`-licensed package.

## 2. Request flow through `apps/gateway`, at the component level

**Server shell** (`apps/gateway/src/app.ts`, `serve.ts`). The gateway is a
Node HTTP server running a Hono app, with a separate metrics app.
Global middleware runs in this order (`app.ts:110-132`):

1. tracing
2. request lifecycle
3. request logger
4. CORS
5. **backpressure**: a per-pod cap on concurrent inference requests. Excess
   requests are shed immediately with a retryable 529 (`lib/backpressure.ts`).
6. rejection logging
7. **per-organization endpoint rate limit**: rolling 60-second window, with
   per-endpoint RPM defaults listed in `resources/rate-limits.mdx`.

Routes are mounted under `/v1` at `app.ts:379-397`. The AI SDK protocol is
mounted at `/v*/ai` (`app.ts:407`) and an MCP server at `/mcp`
(`app.ts:411`). Shutdown is graceful: streaming handlers register "pending
work" so the billing tail finishes before exit (`lib/pending-work.ts`,
`.agents/skills/gateway/SKILL.md`).

**The chat-completions pipeline** (`chat/chat.ts`, about 15.8k lines, plus
around 80 helpers in `chat/tools/` and `lib/`). Its stages, in order:

1. **Authentication.** The bearer token is taken from `Authorization` or
   `x-api-key` (`lib/extract-api-token.ts`). It is looked up by keyed
   HMAC-SHA-256 fingerprint, so key secrets are never stored
   (`features/api-keys.mdx`). Key, project and organization rows come through
   cached queries (`lib/cached-queries.ts`). Checks include key expiry,
   archived projects, organization usability, per-key spend limits
   (`lib/api-key-usage-limits.ts`) and organization spend limits. The
   counters for these checks live in Redis, never in the log table.
2. **Request validation.** Zod/OpenAPI schemas (`chat/schemas/completions.ts`)
   validate the body. Gateway-specific inputs are extracted as well: the
   `x-source`/Referer/User-Agent source attribution, the `X-No-Fallback`,
   `x-debug` and `X-LLMGateway-*` headers, the session id, the unified
   `reasoning` controls, the `web_search` tool and `plugins`.
3. **Model resolution.** `parse-model-input` and `resolve-model-info` turn
   the `model` string into a catalog entry. The string can be a bare id,
   `provider/model`, `provider/model:region`, `custom-name/model`,
   `dynamic/<route>`, or the pseudo-models `auto`, `smart` and `custom`.
   Deactivated mappings are filtered out at this point.
4. **Policy and capability filtering.** Candidates are dropped when they
   fail: IAM rules on the key, user or team (`lib/iam.ts`); project mode
   (`api-keys` / `credits` / `hybrid`, `packages/db/src/schema.ts:1462`),
   which decides whose credentials may be used; compliance and data-policy
   requirements; plan restrictions; or a capability the request needs (vision,
   tools, a tool-choice mode, JSON schema, reasoning, web search, documents,
   context size). Validation lives in `validate-model-capabilities.ts`. Every
   dropped candidate gets a reason, which is reported in routing metadata
   (`provider-filter-reasons.ts`).
5. **Guardrails and content filters.** Guardrails apply to enterprise
   organizations only (`chat.ts:3112`) and can block, redact or warn. Optional
   content-filter classifiers also run.
6. **Provider selection.** This step covers auto/smart model choice, dynamic
   route evaluation, and weighted scoring of (provider, region) candidates
   (`packages/actions/src/compute-provider-scores.ts`, routing metrics in
   `lib/provider-metrics-for-routing.ts`). It also applies sticky preference
   and session pins (`lib/smart-routing-session.ts`, `features/sessions.mdx`),
   re-routes away from providers that are over their rate limit
   (`chat.ts:5221`), and swaps out providers with low uptime when a
   specific provider was requested (`chat.ts:5415`).
7. **Credential selection.** The key comes from the organization's provider
   key (BYOK, encrypted at rest in `packages/actions/src/provider-key/`), or
   from platform environment keys. Environment variables may hold
   comma-separated keys, which are used primary-first with health-aware
   failover (`lib/round-robin-env.ts`, `lib/api-key-health.ts`).
8. **Response-cache lookup** (optional, per project). The key is derived
   from the parsed request. Cached streaming responses are replayed chunk
   by chunk with their original timing, are logged with **zero cost**, and
   can be bypassed per request (`chat.ts:7391-7970`,
   `features/caching/gateway-caching.mdx`).
9. **Request transformation.** `prepareRequestBody` in `packages/actions`
   converts the OpenAI-style request into the wire dialect the mapping
   declares. `getProviderEndpoint` and `getProviderHeaders` produce the URL
   and authentication (including GCP and AWS token flows). This step also
   injects prompt-cache markers for providers that need explicit
   breakpoints and forwards a keyed hash of the session id as
   `prompt_cache_key` where supported.
10. **Upstream call.** All calls go through `fetch` on a tuned undici
    dispatcher, with long keep-alive and an in-process DNS cache to cut
    time-to-first-token (`lib/upstream-dispatcher.ts:1-12`). Each call has
    separate streaming and non-streaming timeouts and is combined with the
    client-abort signal (`lib/timeout-config.ts`). Cancellation is
    propagated upstream only for providers flagged `cancellation`.
11. **Response normalization.** Non-streaming responses go through
    `parse-provider-response.ts`. Streaming responses go through
    `transform-streaming-to-openai.ts`, which re-emits SSE chunks in OpenAI
    format, with AWS eventstream parsing for Bedrock. Finish reasons are
    mapped to OpenAI values and to an internal "unified" finish-reason enum
    (`lib/logs.ts`). Usage is extracted from the provider, or estimated with
    a tokenizer when it is missing (`extract-token-usage.ts`,
    `estimate-tokens.ts`). Reasoning text and signatures are extracted. An
    opt-in "response-healing" plugin repairs malformed JSON output.
12. **Retry and fallback.** Errors are classified into retryable types:
    5xx, timeouts, connection failures, upstream 429s, and provider
    account or mapping problems. Ordinary client 4xx errors are passed
    through without a retry (`get-finish-reason-from-error.ts`,
    `retry-with-fallback.ts`). A retry goes either to the same key with a
    delay, or to the next best provider. The default is 2 cross-provider
    retries (`retry-with-fallback.ts:8-16`). Retry is disabled when the
    request pins a provider, sends `X-No-Fallback`, is in a sticky session,
    or replays provider-bound reasoning such as OpenAI encrypted reasoning
    or Gemini thought signatures. Streams can only fail over before the
    first output token.
13. **Cost and logging.** `calculateCosts` (`packages/actions/src/costs.ts`)
    uses decimal math over the mapping's prices: token, cached-read and
    cache-write prices, tiers, peak pricing, per-request, per-image and
    per-search prices, and service-tier multipliers. `insertLog` builds a
    log row and **publishes it to the Redis queue** (`lib/logs.ts:368-477`).
    Failed attempts are logged as `retried` and linked to the final log.
    Routing metadata (candidates, scores, reasons) is attached to the log
    and exposed in response metadata.

**The other surfaces** reuse this pipeline. `/v1/messages` (`anthropic.ts:1135`),
`/v1/responses` (`responses.ts:489`), the AI SDK protocol (`aisdk.ts:140`) and
image generation via chat models (`images.ts:696`) all make an internal
`app.request("/v1/chat/completions")` call. Trusted-origin headers tag the hop
(`lib/api-origin.ts`). Responses-API conversation state
(`previous_response_id`) lives in the storage Redis. Embeddings, rerank, OCR,
speech, transcription, video, moderation, search and realtime
(a WebSocket server, `realtime/server.ts`) have their own handlers.

## 3. `packages/models`: the catalog

**Shape**, from field names in `packages/models/src/models.ts` and
`providers.ts`:

- **`ModelDefinition`** (`models.ts:907`) holds: a canonical `id`, `name`,
  `aliases`, `family`, `description`, `releasedAt`, flags (`free`,
  `stability`, output modalities, system-role support), and a list of
  `providers` mappings.
- **`ProviderModelMapping`** (`models.ts:291`) is one per (model, provider)
  and is where nearly everything lives:
  - **Identity:** `providerId` and `externalId` (the upstream model name),
    plus optional region or regions.
  - **Pricing:** decimal strings per token, covering input, output, cached
    read, cache write (5m and 1h), audio, image and character. There are
    also per-request, per-second, per-image, per-OCR-page and web-search
    prices, `pricingTiers` by context length, `peakPricing` by UTC hours,
    and service-tier multipliers.
  - **Limits:** `contextSize`, `maxOutput`, `maxTemperature`, `maxN`.
  - **Capabilities:** about 40 flags. Some are booleans (`streaming`,
    `vision`, `tools`, `jsonOutputSchema`, `reasoning`, `webSearch`).
    Others are curated lists that routing relies on (`supportedToolChoices`,
    `reasoningEfforts`, `serviceTiers`, `supportsAssistantPrefill`,
    `supportsDeveloperRole`). The rest are quirk switches (thinking-param
    style, reasoning output mode, JSON-healing need).
  - **Wire dialect:** `apiFormat`, one of `provider-native`,
    `openai-chat-completions`, `openai-responses` or `google-vertex`
    (`models.ts:74-81`).
  - **Lifecycle:** `deprecatedAt`, `deactivatedAt` and a test-policy flag.
    Mappings are never deleted, only deactivated (`AGENTS.md`).
- **`ProviderDefinition`** (`providers.ts:201`) holds: the environment-variable
  credential config (required and optional variables, exclusive groups), a
  cancellation-support flag, `regionConfig` (endpoint templates per region),
  service tiers, prompt-cache idle-TTL facts, an "encrypted reasoning" flag
  (meaning the session must stay on this provider), a priority, and legal and
  data-policy facts (headquarters, retention, training use) for compliance
  filtering.

**Compared with LiteLLM's catalog JSON:**

| Aspect | llmgateway `packages/models` | LiteLLM `model_prices_and_context_window.json` |
|---|---|---|
| Format | TypeScript modules, type-checked and compiled into the gateway. The worker syncs them into DB tables (`model`, `model_provider_mapping`). | One flat JSON file loaded at runtime, which users can override. |
| Granularity | Two levels: model → [provider mappings]. One canonical id spans providers, so routing can pick among them. | One flat entry per deployment key (often prefixed, e.g. `bedrock/...`) with a `litellm_provider` field. Cross-provider grouping is implicit. |
| Provider metadata | A separate provider registry: credentials, regions, tiers, cache TTL, data policy. | Mostly in code, outside the JSON. |
| Pricing | Rich: tiers by context, peak hours, cache write TTLs, service-tier multipliers. Stored as decimal strings. | Per-token floats, with cache read and write fields and some tiered fields. |
| Capabilities | Curated per mapping, including routing-critical lists such as tool-choice modes and reasoning efforts. | `supports_*` booleans per entry. |
| Ownership | Edited only by maintainers. New providers come only through their Airside portal (`README.md`, "Adding a provider"). | Community PRs. |

## 4. API surfaces and providers

**Inference endpoints** (`app.ts:379-411`):

- `/v1/chat/completions`: OpenAI-compatible, the main hub.
- `/v1/messages`: Anthropic-compatible. The docs say it is aimed at Claude
  Code with `ANTHROPIC_BASE_URL` (`features/anthropic-endpoint.mdx`).
- `/v1/responses`: OpenAI Responses API with stored state.
- `/v1/embeddings`, `/v1/images` (generations and edits), `/v1/audio/speech`,
  `/v1/audio/transcriptions`, `/v1/videos`, `/v1/moderations`, `/v1/rerank`,
  `/v1/ocr` and `/v1/search`.
- `/v1/realtime`: mints client secrets, plus a WebSocket realtime server.
- `/v1/systemone`: typed "decision" models.
- `/v1/models`, plus the account endpoints `/v1/key` and `/v1/credits`.
- `/v*/ai`: the Vercel AI SDK gateway protocol.
- `/mcp`: an MCP server over Streamable HTTP for usage and cost queries and
  generation.
- `/docs`: Swagger UI.

**Providers.** The registry has 53 provider ids (`providers.ts:288` onward):
llmgateway (pseudo), openai, anthropic, google-ai-studio, google-vertex,
vertex-openai, vertex-anthropic, aws-bedrock, aws-mantle, azure,
azure-ai-foundry, azure-anthropic, groq, cerebras, xai, deepseek, alibaba,
mistral, moonshot, zai, minimax, baidu, bytedance, tencent, xiaomi,
perplexity, together-ai, fireworks, deepinfra, nebius, novita, runpod,
inference.net, atlascloud, canopywave, nanogpt, embercloud, meta,
meta-contributor, sakana, elevenlabs, runware, reve, glacier, quartz, scx-ai,
scx-ai-gp, gonka24, ranoai, consensusprotocol, atria, typesafe, and custom.
Enterprise organizations can also add their own OpenAI-compatible
"custom providers" (`features/custom-providers.mdx`). The native wire
families in use are OpenAI Chat Completions (and compatible APIs), OpenAI
Responses, Anthropic Messages (direct, Bedrock, Vertex, Azure), Google Gemini
(AI Studio and Vertex), the AWS Bedrock eventstream, and a few one-offs such
as Perplexity's agent API.

## 5. Tech stack, runtime, deployment, weight

- **Runtime:** Node.js. The gateway and API are compiled with plain `tsc`
  and run as `node dist/serve.js`, with no bundler (`AGENTS.md`).
- **Libraries:** Hono and `@hono/zod-openapi`, Zod 3, undici, ws, ioredis
  and node-redis, decimal.js, OpenTelemetry, PostHog, and the MCP SDK
  (`apps/gateway/package.json`).
- **State:** PostgreSQL through Drizzle (145 tables in
  `packages/db/src/schema.ts`), which is the source of truth for tenants,
  keys, logs and aggregates. **Redis ×2**: one for cache, counters and the
  log queue, and one persistent "storage" Redis for response state and the
  response cache.
- **Queue:** a plain Redis list, consumed by polling worker loops. There is
  no external broker.
- **Deployment:**
  - A unified Debian image (`infra/unified.dockerfile`) runs Postgres,
    Redis and nine apps under supervisord (`infra/supervisord.conf`).
  - A split multi-stage image (`infra/split.dockerfile`) has one target
    per app.
  - Compose files for both layouts, and a Helm chart (`infra/helm/llmgateway`).
  - Self-host guides for AWS, GCP and Azure (`self-host/*.mdx`).
- **Footprint:** the self-host guide says every deployment is built from
  the same pieces: the gateway, API, UI and worker (all stateless), plus
  PostgreSQL, Redis and provider keys (`self-host/index.mdx`). Gateway API
  keys are created through the API and UI. You also need the `AUTH_SECRET`
  and `GATEWAY_API_KEY_HASH_SECRET` secrets (`README.md`). This is far
  heavier than a library-style provider layer.

**Essential gateway features versus product features.**

- **Essential:** the model and provider catalog, request and response
  translation, streaming normalization, credential selection, routing and
  fallback, usage extraction, cost calculation, and the log sink.
- **Product:** organizations and teams, SSO/SCIM, IAM rules, plans and credits
  (Stripe, DevPass, chat plans), referrals, Airside provider filings, the
  dashboard, the Lounge chat and mobile apps, notifications, guardrails (ee),
  audit (ee), the admin dashboard (ee), data streams and benchmarks. Much of
  `chat.ts` handles plan, compliance and billing rules that are mixed into
  the request path.

## 6. Design ideas for AgentBuild's Rust provider layer

These are written as general techniques we could build independently. None
of them describes llmgateway's code.

1. **Separate *model*, *deployment* and *provider*.** Use a canonical model
   id that maps to a list of deployments (provider, upstream id, prices,
   limits, capabilities), plus a provider registry for credentials,
   endpoints and regions. Routing then becomes a choice among deployments of
   one model. Ship the catalog as data (TOML or JSON) behind a typed Rust
   loader. That combines LiteLLM's ability to override entries with type
   safety.
2. **Make the wire dialect a property of the deployment.** The dialect is
   one of a few families, such as `openai_chat`, `openai_responses`,
   `anthropic_messages` and `gemini`. A new OpenAI-compatible host is then
   catalog data, not new code. This lets AgentBuild keep a small number of
   codec implementations behind one trait.
3. **Use one internal request representation and thin codecs.** Convert
   inbound shapes to one internal shape, and convert back at the edges, so
   routing, logging and costing run once. AgentBuild should design its own
   neutral internal representation. llmgateway uses the OpenAI chat
   completions shape for this, which is lossy for Anthropic thinking and
   cache blocks and for the Responses API.
4. **Treat capabilities as curated lists that routing can act on**, not
   only booleans. Examples are tool-choice modes, reasoning-effort levels and
   assistant prefill. Filter candidates *before* calling a provider, and
   record why each one was dropped.
5. **Let error classes drive fallback.** Classify failures into
   transient/server, rate-limit, credential/account, mapping-gap and client
   error. Fall back only on the non-client classes, and stay conservative:
   misreading client errors as provider faults causes pointless fallbacks and
   distorts health data.
6. **Know when fallback is unsafe.** Never fail over once streamed output
   has reached the caller. Never fail over for requests that replay
   provider-bound opaque state, such as encrypted reasoning or thought
   signatures. Honor an explicit pin or a "no fallback" flag.
7. **Make sessions sticky for prompt-cache locality.** For agent loops, pin
   a session to the deployment chosen first, and switch only when health
   requires it. Prompt-cache savings usually exceed small price differences.
   Pass a session-derived cache key upstream where the API supports one.
   This matters directly for AgentBuild, whose workloads are multi-turn agent
   loops.
8. **Score with cache awareness when choosing.** Estimate the effective
   input price from the expected cache-hit rate and output ratio, not from
   list price. Weight recent health more heavily than older data. Use
   hysteresis (a margin before switching) so routing does not flap.
9. **Keep usage and cost accounting off the hot path.** Emit a usage event
   (tokens, cost, deployment, attempt chain) to a sink: a channel, a
   callback, or OpenTelemetry. Do not write synchronously to a database.
   Price with fixed-point decimals. For agent runs, attach the full
   attempt and fallback chain to the step trace.
10. **Degrade gracefully.** Shed load quickly with a retryable status
    instead of letting requests hang. Keep a last-known-good copy of
    configuration so that losing the control-plane store does not stop
    inference. Tune connection reuse and DNS caching for time-to-first-token.
11. **Guard against SSRF.** Fetch user-supplied content URLs (images, files)
    only through an SSRF guard: https only, no private, link-local or
    metadata addresses, and no redirects. Fail the request rather than
    passing the URL upstream.

**Things to avoid:**

- **A 15k-line request handler** that mixes billing, plans, compliance and
  routing. AgentBuild should keep these as composable layers or middleware
  with narrow traits.
- **Tenancy, billing and plan logic in the provider layer.** AgentBuild is
  a runtime, not a SaaS. Quotas and keys belong to whoever hosts it.
- **Requiring Postgres, Redis and a worker** just to make a provider call.
  The provider layer should run in-process with no external services, and
  persistence should be optional.
- **Lossy hub translation.** Do not force every surface through one
  vendor's wire shape when the IR can be neutral.
- **Unbounded catalog growth with quirk flags.** Prefer a small set of
  dialect codecs with explicit, versioned quirk tables, and test them against
  recorded fixtures.

## 7. Comparison: llmgateway vs LiteLLM

| | llmgateway (verified at `ae99ddf`) | LiteLLM (brief) |
|---|---|---|
| License | AGPLv3 core; `ee/` is commercial. The gateway depends on the `ee` guardrails package. | MIT core; enterprise directory under a separate license |
| Language and runtime | TypeScript on Node (Hono) | Python (SDK and FastAPI proxy) |
| Form | Hosted SaaS plus self-hostable services. No embeddable SDK. | Library (SDK) plus proxy server |
| Scope | Gateway plus a full product: dashboard, billing, orgs, SSO, consumer chat, coding plans, provider portal, MCP | SDK and proxy, with keys, budgets, spend tracking and an admin UI |
| API surfaces | OpenAI Chat, Anthropic Messages, OpenAI Responses, embeddings, images, audio, video, rerank, OCR, moderation, search, realtime, AI SDK protocol, MCP | Mainly the OpenAI-compatible surface, plus Anthropic and Responses passthroughs |
| Providers | 53 registry ids, about 416 models, 739 mappings. Closed to community provider PRs. | 100+ providers, community contributions |
| Catalog | Typed TS model → mappings, plus a provider registry | One flat JSON price and capability map |
| Routing | Weighted metrics scoring, sticky sessions, exploration, auto/smart model routing, dynamic route graphs | Router with strategies and fallbacks |
| Required infrastructure | Postgres + 2× Redis + gateway + worker (+ API and UI) | None for the SDK; the proxy is optional, with Postgres/Redis optional |
| Gateway size | About 81k LOC in `apps/gateway`, plus 17k in `actions` and 29k in `models` | — |
| Usable by AgentBuild | **Only as an external HTTP service, unmodified** | Its license allows study, and porting with attribution |

## Appendix: REA static analysis of `apps/gateway`

**What was run:**

```
rea analyze-javascript-application <repo>/apps/gateway --format json --max-heap-mb 4096
```

The command took about 57 seconds. Its output is at
`research/rea-evidence/llmgateway-gateway.json` (76 MB, git-ignored), and a
summary is at `research/rea-evidence/llmgateway-gateway.summary.txt`. The
summary was produced by a small throwaway Python script. That script lived in
the session scratchpad and was not committed.

**What REA determined:**

- **Files:** an inventory of 429 parsed files with 0 parse failures. 215 are
  non-test sources: `chat` 78, `lib` 56, `realtime` 15, `aisdk` 12,
  `responses` 9, `anthropic` 6, and so on. About 625k AST nodes were visited.
- **Per-file static import facts:** 1,298 references in 212 files.
  - For non-test code, the import targets were: internal modules 419 (as
    file and specifier pairs), `@llmgateway/shared` 46,
    `@llmgateway/models` 40, `hono` 40, `@llmgateway/logger` 28,
    `@llmgateway/db` 24, `@llmgateway/actions` 20, `@hono/zod-openapi` 17,
    `@llmgateway/cache` 15, `@llmgateway/instrumentation` 6,
    `@modelcontextprotocol/sdk` 5, and `@llmgateway/guardrails` 2.
  - **Cross-directory imports** (`@/` alias) form a hub on `lib`:
    `chat→lib` 52, `realtime→lib` 20, `search→lib` 20,
    `moderations→lib` 16, `speech→lib` 15. Several endpoint modules
    (`search`, `moderations`, `speech`) also import from `chat`, which
    confirms that chat is the shared core.
  - **External packages imported by non-test code:** hono,
    @hono/zod-openapi, @hono/swagger-ui, zod, undici, ws, decimal.js,
    dotenv, @opentelemetry/api, @modelcontextprotocol/sdk.
  - `chat/chat.ts` alone has 119 import references.
- **Endpoint literals:** 27 were discovered, nearly all from `app.request`
  calls in tests. They include `/v1/chat/completions`, `/v1/messages`,
  `/v1/responses`, `/v1/embeddings`, `/v1/images/*`, `/v1/models`,
  `/v1/moderations` and `/v1/credits`. REA also found the production
  in-process `app.request` hops.

**What REA could not determine (an honest account):**

- **Most cross-file relationships were dropped.** The expanded
  application-graph relationships for 791 nodes (388 of them non-test)
  exceeded the result budget, so only 626 graph-level `imports` edges were
  expanded.
- **About half of the files have no per-file import facts.** 217 of 429
  assets carry no `references` list, including `videos.ts`, `fetch-provider.ts`
  and `gemini-session.ts`. A grep cross-check finds about 2,387 import
  statements in 401 importing files. REA's import view therefore covers
  roughly 54% of imports.
- **The route table is missing.** REA did not reconstruct the Hono route
  table (`v1.route(...)` mounts), the middleware order, or the
  call-by-call request flow. Its semantic graph is truncated, and its
  `process.env` coverage found only 5 keys, whereas the gateway reads many.
- **TypeScript is treated as plain JS.** Types, interfaces and the
  `@/` path alias were not resolved semantically; 203 module references
  stayed "unresolved". The Electron-oriented summary fields (IPC, preload,
  windows) are all zero, which is expected for a server app.

**Conclusion:** REA was useful for an inventory and a partial,
package-level import picture. All architectural claims in this note rest on
the repo's own documentation, manifests and file and symbol structure, not
on REA's graph.

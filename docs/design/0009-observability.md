# 0009: Observability: OpenTelemetry export and Langfuse

Status: **Implemented** (`agentbuild-telemetry`, core observers and run
metadata, agent event payloads). User request, 2026-10-11: "seamless
connectivity to Langfuse". Verified against Langfuse's ingestion source
([research](../research/langfuse.md)); no live Langfuse test yet (no
Docker on the dev machine; `examples/langfuse.rs` does it with real keys).

As built, differences from the first draft below, all from the source
study:

- Input and output are sent as OpenAI-style chat JSON (`{role, content,
  tool_calls}` / `{role: "tool", tool_call_id}`), which Langfuse renders as a
  conversation; reasoning blocks are omitted and media shown as `[image]`.
- `usage_details` uses Langfuse's keys: `input` and `output` exclude cached
  input and reasoning output (`input_cached_tokens`, `input_cache_creation`,
  `output_reasoning_tokens`); `total` covers all. `gen_ai.usage.*` carries
  our raw counts for other backends.
- Warnings, retries (`attempt_failed`) and custom events are exported as
  short `event` observations (level `WARNING` for warnings and retries),
  not OTLP span events, which Langfuse does not store.
- The root span carries the first model input and the last model output,
  which Langfuse shows as the trace's input and output.
- Tool calls that return an error to the model are level `WARNING` (the
  model may recover); node failures are `ERROR`.

Builds on the events of [0005 §6](0005-core-runtime-extensions.md)
and the agent events of [0004 §8](0004-agent-loop.md).

## Goals

1. **Langfuse in one line.** `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY`
   / `LANGFUSE_BASE_URL` set, one call to enable, and every run appears in
   Langfuse as a trace: workflow nodes as spans, model calls as
   *generations* (model, input, output, token usage), tool calls as *tool*
   observations, failures marked as errors.
2. **Not Langfuse-only.** Langfuse ingests standard OpenTelemetry (OTLP)
   since v3.22 and has deprecated its own ingestion API. We export OTLP, so
   the same exporter feeds Jaeger, Grafana Tempo, Datadog, Arize Phoenix or an
   OpenTelemetry Collector. Langfuse is a preset (endpoint, auth, attributes).
3. **Never slows or breaks a run.** Exporting happens off the critical path:
   events go into a bounded queue; a background task batches and sends.
   When the queue is full or Langfuse is down, spans are dropped and counted,
   and the run carries on. Telemetry errors never fail a run.
4. **Sensitive data stays in.** Content capture can be turned off (only
   names, timings, usage and status are sent) or passed through a mask
   function first. This is required for health data (Axon Health); Langfuse
   also offers self-hosting and a HIPAA region, which the endpoint setting
   covers.
5. **Light.** No OpenTelemetry SDK dependency: OTLP over HTTP/JSON is a
   small, stable format. The exporter is its own crate,
   `agentbuild-telemetry`, so the engine does not depend on HTTP.

## 1. Core changes (0005 §6 extension)

- **Observers.** `RunOptions::observe(observer)` adds an `EventObserver`
  (`fn on_event(&self, event: &Event)`) that receives every event of the run,
  **in addition to** the optional `event_channel` subscriber. Observers are
  called synchronously when an event is emitted, so they must not block (the
  exporter only pushes into its queue). Any number of observers may be
  attached; a run with an observer builds events even without a channel.
- **Run metadata.** `RunOptions::metadata(json)` attaches a JSON object to
  the run; it is reported in `run_started.data.metadata`. The telemetry
  crate reads these keys: `name` (trace name), `user_id`, `session_id`,
  `tags` (list of strings), `environment`, `release`; everything else
  becomes trace metadata.

## 2. Agent event payloads (0004 §8 extension)

So that model calls and tool calls can be rebuilt as observations, the agent
events carry (built only when someone listens):

| Event | Data |
|---|---|
| `model_request` | `provider`, `model`, `settings`, `tools` (names), `messages` (the full request, instructions included) |
| `message_completed` | `message_id`, `message` (the assistant message), `finish_reason`, `usage`, `model` (as reported by the provider) |
| `tool_call_started` | `call_id`, `tool`, `args` |
| `tool_call_finished` | `call_id`, `tool`, `status`, `duration_ms`, `content` (the result blocks), `error` |

## 3. Mapping events to spans

| AgentBuild | OTLP span | Langfuse |
|---|---|---|
| run segment (`run_started` … `run_finished`) | root span `run` (or `metadata.name`) | trace + root observation (`agent` type for agents, else `chain`) |
| node (`node_started` … `node_finished`/`failed`/`cancelled`) | child span named after the node | `span` observation |
| model call (`model_request` … `message_completed`) | child of the node span | `generation`: model, input messages, output message, usage, finish reason |
| tool call (`tool_call_started` … `tool_call_finished`) | child of the node span | `tool` observation: input args, output content, status |
| `attempt_failed`, `warning`, `transcript_repaired` | span events on the node span | shown as events |
| custom events from user nodes | span events on the node span | |

- **Trace id** = the run id (a UUIDv7 is 128 bits, exactly a trace id), so
  every segment of a run, across suspend/resume, lands in the same trace.
- **Span ids** are derived deterministically (hash of run id + scope + a
  sequence), so a re-exported run produces the same ids.
- Times come from the events' `ts_ms`. Failed, cancelled or timed-out
  spans get status `ERROR` with the error message, and level `ERROR` in
  Langfuse.
- Attributes follow the OpenTelemetry GenAI conventions (`gen_ai.*`) plus
  explicit `langfuse.*` attributes, which Langfuse prefers.

## 4. Exporter

- `OtlpExporter::new(OtlpConfig { endpoint, headers, service_name,
  capture_content, mask, queue_capacity, batch_size, flush_interval })`
  spawns a background task on the current Tokio runtime.
- `exporter.observer()` returns the `EventObserver` to pass to
  `RunOptions::observe`.
- Spans are buffered until their end event, then queued. The task sends a
  batch when `batch_size` spans are ready or `flush_interval` passes. Failed
  sends (network, 429, 5xx) are retried twice with backoff, then dropped.
- `exporter.flush().await` waits until everything queued is sent;
  `shutdown().await` flushes and stops. Short-lived programs must call one.
- `exporter.stats()`: spans exported, dropped (queue full), failed (after
  retries).
- **Langfuse preset:** `Langfuse::from_env()` / `Langfuse::new(public,
  secret).base_url(url)` builds the config: endpoint
  `{base_url}/api/public/otel/v1/traces`, `Authorization: Basic
  base64(public:secret)`, `x-langfuse-ingestion-version: 4`. Default base
  URL `https://cloud.langfuse.com`. `LANGFUSE_HOST` is accepted as an older
  name for `LANGFUSE_BASE_URL`.

## 5. Edge cases

| # | Case | Behaviour |
|---|---|---|
| T1 | Langfuse unreachable | Spans dropped after retries; `stats().failed` counts them; the run is unaffected |
| T2 | Queue full (very chatty runs, slow network) | New spans dropped and counted, never blocking a node |
| T3 | Run cancelled or failed mid-node | Open spans are closed at `run_finished` with status `ERROR` |
| T4 | Suspended run resumed hours later in another process | Same trace id; the second segment is a second root span in the trace |
| T5 | `capture_content = false` | No messages, args or results leave the process; names, timings, usage, status, error codes do |
| T6 | Mask function panics | The content is replaced by `"[mask failed]"`; nothing unmasked is sent |
| T7 | Program exits without `flush` | Unsent spans are lost (documented; same as Langfuse's SDKs) |
| T8 | Huge payloads (long transcripts) | Strings longer than `max_attribute_chars` (default 100,000) are truncated with a marker |

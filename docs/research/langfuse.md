# Langfuse: how it ingests OpenTelemetry

Studied 2026-10-11 from the Langfuse source (MIT outside its `ee/`
directories; commit `cb2cebae`, checked out sparsely in the git-ignored
`research/repos/langfuse`) and its documentation. Read-only study: no
Langfuse code is used in Criogen. Input to
[design 0009](../design/0009-observability.md).

## Transport

- `POST {base}/api/public/otel/v1/traces`, OTLP over HTTP with JSON or
  protobuf (content type matched by substring), optional gzip. No gRPC.
- Auth: HTTP Basic `base64(public_key:secret_key)`.
- `x-langfuse-ingestion-version: 4` writes straight to the v4 store; without
  it, non-SDK exporters go through a slower path (up to ~10 minutes of delay
  for new cloud organisations). Values above 4 are rejected.
- JSON is taken as-is: ids are kept as given (send lowercase hex), 64-bit
  integers may be strings or numbers, attributes must be a list of
  `{key, value}`. A malformed batch is rejected whole with 400; accepted
  batches return 200 and are processed asynchronously (no per-span errors
  come back). Default body limit 512 MiB.

## Traces and observations

- A span without a parent (or marked `langfuse.internal.as_root`) creates
  the trace; several roots in one trace id simply update the same trace,
  which fits Criogen's suspend/resume segments.
- With ingestion version 4, trace fields (name, user, session, tags) are
  stored per observation from each span's own attributes, so they must be on
  every span.
- Observation type comes from `langfuse.observation.type` (span, generation,
  event, embedding, agent, tool, chain, retriever, guardrail, evaluator),
  otherwise from `gen_ai.operation.name`, otherwise generation if a model is
  named, otherwise span.
- `langfuse.observation.input` / `output` are stored as strings and parsed as
  JSON for display. The UI shows a chat view for `[{role, content}]`
  (OpenAI style, `tool_calls` allowed), `{messages: [...]}` or a single
  message.
- Usage: `langfuse.observation.usage_details` is a JSON object string used
  verbatim. Its `input` and `output` **exclude** cached input and reasoning
  output, which have their own keys: `input_cached_tokens`,
  `input_cache_creation`, `output_reasoning_tokens`; `total` covers all.
  (When only `gen_ai.usage.*` is sent, Langfuse subtracts those itself.)
- Level from `langfuse.observation.level`, else ERROR when the span status
  is error; status message from `langfuse.observation.status_message`, else
  the status message.
- `completion_start_time` must be an ISO 8601 date; observations that fail
  validation in the worker are dropped silently.
- **OTLP span events are not stored** (they are read only to reconstruct
  GenAI input/output). Criogen therefore exports warnings, retries and
  custom events as short `event` observations.
- Unknown attributes go to the observation's `metadata.attributes`, except
  for scopes named `langfuse-sdk*`.

## What Criogen does with this

See [0009](../design/0009-observability.md) §3 and the "as built" notes
there.

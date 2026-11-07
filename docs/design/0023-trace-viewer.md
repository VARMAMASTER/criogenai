# 0023: Trace viewer: every run on a local page

Status: approved by the user's request (2026-11-07, roadmap three, task 22).

## Why

A developer needs to see what an agent did: which model calls and tool calls,
in what order, how long each took, how many tokens, what it cost, and where
it failed. Today that needs Langfuse or another tracing backend. The viewer
gives it with one command on the developer's own machine: no account, no
server, nothing leaving the computer.

## Design: an OTLP receiver with a page

Criogen already exports every run as OpenTelemetry spans (design 0009). The
viewer **receives those spans** instead of adding a second way of recording
runs:

```
agent / workflow ──(OTLP/HTTP JSON, the existing exporter)──▶ criogen-trace
                                                               ├─ stores spans (SQLite or memory)
                                                               ├─ /api/runs, /api/runs/{id}
                                                               └─ /  the page
```

So the viewer shows exactly what Langfuse would get, works for Rust, Python
and TypeScript alike, and any other OTLP JSON producer can send to it too.

## The crate

`crates/criogen-trace` (library and binary `criogen-trace`), axum and sqlx
(both already in the tree through the gateway and storage):

- `POST /v1/traces`: OTLP/HTTP with a JSON body (`Content-Type:
  application/json`); protobuf gets 415 with a message saying to send JSON
  (Criogen's exporter always does). Body limit 16 MiB. Spans are grouped by
  trace id: one trace is one run (0009).
- `GET /api/runs?limit&before&q`: newest first: id, name, start, duration,
  status, model calls, tool calls, tokens (input, output), cost, models used.
- `GET /api/runs/{trace_id}`: every span: id, parent, name, kind
  (run / node / generation / tool / event), start, end, status, attributes
  (model, tokens, input and output only when the exporter captured content).
- `GET /`: the page, embedded in the binary (one HTML file, no outside
  scripts, fonts or images; a strict Content-Security-Policy).
- `DELETE /api/runs` clears everything.

Storage: SQLite (`~/.criogen/traces.db` by default, `--db` to change,
`--memory` for none). Retention: the newest 10 000 runs and 14 days by
default (`--keep-runs`, `--keep-days`), pruned on insert.

Cost: from a price file (`--prices prices.json`: USD per million tokens by
`provider:model` or model name, the same shape as `estimate_cost`). Without
one, cost is not shown; tokens always are. Criogen ships no price list (prices
change; a stale price is worse than none).

## Security (rules 10–12)

- Listens on `127.0.0.1:4318` by default. Another address needs
  `--allow-remote` **and** a token (`--token-env NAME`): then every request
  needs `Authorization: Bearer <token>`, compared in constant time. The
  token is never logged.
- Content is whatever the exporter sends: by default no prompts or outputs
  (0009's capture switch is off), so the viewer shows structure, timing and
  tokens only. `capture_content` turns content on for local debugging.
- Everything received is data: attribute values are bounded (64 KB each,
  500 spans per run accepted, 100 000 spans in total in memory mode), ids are
  checked as hex, the page sets every value as text (no HTML from a span ever
  renders), and SQL is parameterised. (As built: memory mode is bounded by
  the same retention, at most `keep_runs` × 500 spans, rather than a
  separate span count.)

## The page

- **Runs**: a table that refreshes every 2 s: time, name, duration, status,
  model calls, tool calls, tokens, cost; a search box (name, model, run id).
- **A run**: a waterfall: each span a bar on the run's time axis, indented by
  parent, coloured by kind, failures red; a summary line (time in model calls,
  in tools, in Criogen, like the playground's split); click a span for its
  attributes, tokens and (if captured) input and output.
- Light and dark, keyboard navigable, works offline.

## SDKs

- Python: `python -m criogen.trace` (or `criogen.trace.serve(port=…,
  db=…)`), and `Telemetry.local()` (OTLP to `http://127.0.0.1:4318`).
- TypeScript: `npx criogen trace`, `serveTraces({...})`, `Telemetry.local()`.
- Rust: `criogen-trace` binary; `criogen_trace::Viewer` as a library (an
  axum `Router`), `OtlpConfig::local()`.

The SDKs run the same Rust server inside the engine's runtime; nothing is
reimplemented.

## Not in this slice

Protobuf OTLP, comparing two runs side by side (evals, 0024, will), sharing a
run, a hosted viewer (task 32).

## Tests

Rust: the real exporter sending to the viewer (end to end), the API's
summaries and details, grouping, bounds (body size, spans, attribute length),
415 for protobuf, retention, remote refusal without a token and token
checking, HTML escaping (a span named `<script>` is shown as text), the page
served with its CSP. SDKs: start the viewer, run an agent with
`Telemetry.local()`, read the run back from `/api/runs`. The page: a browser
test (Playwright, as the playground's) that opens a run and its waterfall.

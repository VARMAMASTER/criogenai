# Benchmarks

What Criogen itself costs, measured as described in
[design 0014](design/0014-benchmarks.md): every model answers at once, so
these are the framework's own costs, added on top of a real model's time
(typically 0.3–5 s per call). One machine, one run; compare numbers only
within this page.

**Run of 2026-10-16** on an AMD Ryzen 5 4600H (6 cores / 12 threads),
15 GB RAM, Windows 11, Rust 1.99.0 (release builds), Python 3.13.14,
Node 22.14.0, LangGraph 1.2.14. Medians and 95th percentiles of 300–2000
runs after a warm-up.

## The Rust engine, agents, storage and gateway

`cargo run -p criogen-bench --release`

| Scenario | Runs | Median per run | p95 per run | Median per unit |
|---|---|---|---|---|
| workflow: 10 nodes in a row | 2000 | 214.5 µs | 325.7 µs | 21.4 µs per step |
| workflow: 100 parallel nodes + join | 500 | 395.6 µs | 504.0 µs | 3.9 µs per node |
| agent: one model call | 2000 | 54.5 µs | 68.6 µs | 54.5 µs per run |
| agent: 10 tool calls + 2 model calls | 1000 | 505.0 µs | 626.9 µs | 505.0 µs per run |
| thread: send, in memory | 1000 | 74.9 µs | 129.8 µs | 74.9 µs per turn |
| thread: send, SQLite (saved per step) | 300 | 1.92 ms | 2.54 ms | 1.92 ms per turn |
| model call, direct (baseline) | 2000 | 0.5 µs | 0.7 µs | 0.5 µs per call |
| gateway: chat completion over HTTP | 2000 | 142.3 µs | 186.8 µs | 142.3 µs per request |
| gateway: streamed chat completion | 2000 | 219.8 µs | 306.9 µs | 219.8 µs per request |

- A conversation turn saved to SQLite costs about 2 ms (several
  checkpoints per turn, each a committed SQLite transaction); in memory,
  75 µs.
- The gateway adds about 0.14 ms to a request on localhost (0.22 ms
  streamed), including the HTTP client's own work. Design 0006 §7's
  target for the translating path is under 3 ms.

## The SDK bridges

`python benches/python_bridge.py`, `node benches/typescript_bridge.mjs`

| Scenario | Runs | Median per run | p95 per run |
|---|---|---|---|
| python: agent run, no tool | 2000 | 254.8 µs | 409.1 µs |
| python: agent run, 1 sync Python tool | 2000 | 560.0 µs | 881.2 µs |
| python: agent run, 1 async Python tool | 2000 | 717.2 µs | 1.24 ms |
| typescript: agent run, no tool | 2000 | 109.4 µs | 162.3 µs |
| typescript: agent run, 1 sync JS tool | 2000 | 298.8 µs | 503.1 µs |
| typescript: agent run, 1 async JS tool | 2000 | 290.2 µs | 472.5 µs |

Compared with the same run in Rust (54.5 µs), a call from Python costs
about 0.2 ms more and from TypeScript about 0.05 ms more. One tool call
written in Python adds about 0.3 ms (0.46 ms for an async tool, which
runs on the caller's event loop); in JavaScript about 0.19 ms. Both
include the extra model call the tool result causes.

## The same workflows in LangGraph

`research/.venv/Scripts/python benches/compare_langgraph.py`

| Scenario | Runs | Median per run | p95 per run | Median per unit |
|---|---|---|---|---|
| workflow: 10 nodes in a row (invoke) | 2000 | 2.44 ms | 3.56 ms | 244.4 µs per step |
| workflow: 10 nodes in a row (ainvoke) | 2000 | 5.19 ms | 7.29 ms | 518.8 µs per step |
| workflow: 100 parallel nodes + join (invoke) | 500 | 40.28 ms | 51.18 ms | 394.9 µs per node |
| workflow: 100 parallel nodes + join (ainvoke) | 500 | 34.06 ms | 44.43 ms | 333.9 µs per node |

With trivial nodes on this machine, Criogen's Rust core spends
21.4 µs per step against LangGraph's 244 µs (`invoke`), and 3.9 µs per
parallel node against 334 µs (`ainvoke`, LangGraph's faster mode here).

What this does **not** show: workflows written in Python on Criogen
(SDK workflows are not built yet; each node would add roughly the Python
bridge cost above), real model latency (which dominates any agent run),
or memory use. Once a model takes a second to answer, the difference
between 20 µs and 250 µs per step is negligible. It matters for
workflows with many steps, wide fan-outs, or many concurrent runs.

## Reproducing

```sh
cargo run -p criogen-bench --release
research/.venv/Scripts/python benches/compare_langgraph.py   # needs langgraph
cd sdks/python && maturin develop --release && cd ../.. && python benches/python_bridge.py
bash scripts/test-typescript.sh && node benches/typescript_bridge.mjs   # builds the release addon
```

Add `quick` to any of them for a smoke test with fewer runs.

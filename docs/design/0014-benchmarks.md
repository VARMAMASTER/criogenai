# 0014: Benchmarks

Status: **Accepted** (task 10 of the roadmap in `docs/progress.md`; the user
gave full design freedom). Rule 6 of `AGENTS.md`: no performance claims
without benchmarks in the repository. This is that benchmark, and the only
source for numbers we publish.

## 1. What is measured

**Criogen's own cost**, not a model's. Every scenario uses a model that
answers at once (no network, no timer), so what is left is the engine,
the agent loop, storage, the gateway and the SDK bridges. Real model calls
take hundreds of milliseconds to seconds; these numbers say how much we
add on top.

| Scenario | Shows |
|---|---|
| workflow: 10 nodes in a row | cost per step (scheduling, state merge, events off) |
| workflow: 100 parallel nodes + join | cost per node when they run at once |
| agent: one model call | the agent loop around one model call |
| agent: 10 tool calls + 2 model calls | cost per tool call (validation, concurrency, results) |
| thread: send, in memory / SQLite | a conversation turn with saving (SQLite: per step, `fsync` as SQLite does) |
| gateway: plain / streamed over HTTP | what the gateway adds to a request on localhost |
| python / typescript: agent run, no tool / one tool | the SDK bridges: a run, and one Python or JavaScript tool call |

## 2. Method

- `crates/criogen-bench` (release build): each scenario warms up,
  then is timed run by run; the median and the 95th percentile are
  reported. `quick` runs a tenth of the iterations for a smoke test (the
  `package` step of `scripts/ci-local.sh` runs it so the benchmark keeps
  working; its numbers are not recorded).
- `benches/python_bridge.py` and `benches/typescript_bridge.mjs` time the
  SDKs (release builds of their native modules) the same way with
  `FakeModel` at zero delay.
- `benches/compare_langgraph.py` builds the same two workflow shapes in
  LangGraph with trivial nodes and times `invoke` and `ainvoke`.
- Results go to `docs/benchmarks.md` with the machine, the versions and
  the date. One machine's numbers; they are compared only with numbers
  from the same run.

## 3. Rules for using the numbers

- Say what was measured: "per step with trivial nodes on <machine>", not
  "N times faster".
- The LangGraph comparison is Criogen's Rust core against LangGraph in
  Python. A workflow written in Python on Criogen would add the bridge
  cost per node (the Python bridge rows); SDK workflows do not exist yet.
- Not measured yet: the gateway against other gateways (LiteLLM proxy),
  Postgres, memory use, many concurrent threads.

## 4. Found while building it

`FakeModel` with zero delay still called `tokio::time::sleep`, which
waits for the next timer tick (about 1 ms on Windows): SDK runs looked
1.5 ms slower than they are. It now yields instead of sleeping when the
delay is zero. Two telemetry tests relied on that slowness (all spans in
one batch) and now check retries per batch.

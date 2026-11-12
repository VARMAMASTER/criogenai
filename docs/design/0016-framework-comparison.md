# 0016: Comparing CriogenAI, LangGraph and CrewAI

Status: **Accepted** (the user asked for a benchmark of CriogenAI against
LangChain/LangGraph and CrewAI on speed, reliability, memory and resource
use, with proof, and a page on the website where anyone can explore the
results and check them on their own machine). Extends
[0014](0014-benchmarks.md), whose rules for using numbers still apply.

## 1. Principles

- **Same work, same language.** All three frameworks run in Python 3.13,
  in the way their own documentation shows. CriogenAI runs through its
  Python SDK (agents, and workflows from [0015](0015-sdk-workflows.md)),
  not its Rust core, so the comparison is Python against Python.
- **Same simulated model.** No network and no API keys. Each framework gets
  an adapter for one scripted model: the same replies (tool calls, then an
  answer), the same configurable delay. Tools are the same Python
  functions. The adapters are as thin as each framework allows, and their
  source is part of the benchmark.
- **One process per measurement.** Every (scenario, framework) pair runs in
  a fresh child process of its own virtual environment, so memory and CPU
  belong to that pair alone. Each framework has its own venv (their
  dependencies conflict), which also gives the install footprint.
- **Defaults first.** Reliability tests use each framework's defaults; where
  a framework documents an option that changes the outcome, the result
  says so (`note`), and the option is not silently turned on.
- **Everything recorded.** The raw results file holds the machine, the
  versions, the command, every number and every reliability log. The
  website shows exactly that file; nothing is typed in by hand.

## 2. Scenarios

| id | kind | What runs | Shows |
|---|---|---|---|
| `agent_call` | speed | agent, one model call, no tools, model delay 0 | framework cost per agent turn |
| `agent_tools` | speed | agent, model asks for 5 tools in one turn, then answers; delay 0, tools instant | cost of a turn with tool calls |
| `agent_latency` | speed | as `agent_tools`, model 200 ms per call, each tool 300 ms of I/O | wall time: are the 5 tools run at once (≈0.7 s) or one by one (≈1.9 s)? |
| `workflow_chain` | speed | 10 Python nodes in a row, trivial | cost per step |
| `workflow_fanout` | speed | 20 parallel Python nodes of 100 ms I/O + join | parallel nodes (≈0.1 s if parallel) |
| `concurrent_1` / `_100` / `_1000` | load | N agent runs at once in one process (model 100 ms, one tool of 50 ms) | wall time, peak memory, memory per run, CPU time |
| `startup` | footprint | `import` in a fresh interpreter, 20 times | cold import time |
| `install` | footprint | size and package count of each venv | what you install |

Workflows use LangGraph's `StateGraph` and CrewAI's `Flow`
(`@start` / `@listen`, `and_` for the join); agents use LangChain's
`create_agent` and CrewAI's `Agent` + `Task` + `Crew`.

## 3. Reliability tests

Each test injects the same fault through the shared model or tools and
records `pass`, `fail` or `partial`, with a reason and the log.

| id | Fault | Pass means |
|---|---|---|
| `model_transient` | the model raises a rate-limit error once, then answers | the run completes without the caller's code retrying |
| `tool_exception` | a tool raises | the model sees the error and the run completes with an answer |
| `bad_arguments` | the model calls a tool with a wrong argument type | the tool function never sees the bad arguments, and the run completes |
| `crash_resume` | the process is killed while the 2nd of 2 tools runs (the 1st finished) | a new process continues the same conversation from storage, and the finished tool is not run again |
| `runaway_loop` | the model asks for a tool forever | the run stops by itself (an error, or a forced final answer) within 100 model calls (`partial`: it stops, but later; `fail`: it does not stop within 5 minutes) |

## 4. The results file

`bench/results/<date>-<machine>.json`, schema `criogenai-compare/1`:

```json
{
  "schema": "criogenai-compare/1",
  "recorded": "2026-10-21T10:00:00Z",
  "command": "python benches/compare/run.py",
  "machine": { "cpu": "AMD Ryzen 5 4600H", "cores": 6, "threads": 12, "ram_gb": 15.4, "os": "Windows 11" },
  "python": "3.13.14",
  "frameworks": {
    "criogenai": { "label": "CriogenAI", "version": "0.1.0" },
    "langgraph":  { "label": "LangGraph", "version": "1.2.14", "also": { "langchain": "1.4.4" } },
    "crewai":     { "label": "CrewAI", "version": "…" }
  },
  "scenarios": [
    { "id": "agent_tools", "kind": "speed", "title": "Agent with 5 tool calls", "detail": "…", "unit": "run" }
  ],
  "results": [
    {
      "scenario": "agent_tools", "framework": "crewai", "status": "ok",
      "runs": 200, "median_ms": 0.0, "p95_ms": 0.0, "min_ms": 0.0, "max_ms": 0.0,
      "wall_ms": 0.0, "cpu_ms": 0.0,
      "baseline_rss_mb": 0.0, "peak_rss_mb": 0.0,
      "samples_ms": [],
      "note": null, "error": null
    }
  ],
  "reliability": [
    { "test": "crash_resume", "framework": "langgraph", "outcome": "partial",
      "reason": "resumed from the checkpoint; both tools ran again", "log": "…", "note": null }
  ],
  "footprint": [
    { "framework": "crewai", "import_ms": 0.0, "packages": 0, "install_mb": 0.0 }
  ]
}
```

- `status`: `ok`, `unsupported` (the framework has no way to do it; `note`
  says why) or `error` (it failed; `error` holds the message).
- `samples_ms`: up to 200 per-run times (evenly picked when there are
  more), so the page can draw the distribution.
- Speed: `median_ms`, `p95_ms`, `min_ms`, `max_ms` are per run, over
  `runs` runs after a warm-up; `wall_ms` is the time of all of them.
  Load (`concurrent_N`): a batch of N runs at once, run three times (each
  in a fresh process; a short CPU-bound batch is noisy on a busy machine).
  The batch with the median wall time is reported: `wall_ms` is that
  batch, the per-run figures are the latencies of the N runs inside it,
  and `batch_walls_ms` lists all three wall times, so the spread shows. `cpu_ms` is user + system CPU time per measured run (speed) or for
  the batch (load). Memory is the process's peak resident set
  (`peak_rss_mb`) and the resident set after imports and set-up
  (`baseline_rss_mb`); `rss_per_run_kb` (load only) is the growth divided
  by N.
- `model_us`: the cost of one call of the framework's simulated-model
  adapter on its own, measured in the same process, so readers can see the
  model adapters are not what differs.

## 5. Running it

```sh
python benches/compare/setup.py      # one venv per framework (pinned versions)
python benches/compare/run.py        # all scenarios and tests → bench/results/…json
python benches/compare/run.py quick  # fewer runs, for a smoke test
```

The orchestrator uses only the standard library; the per-framework
adapters live in `benches/compare/frameworks/`. The website's benchmark
lab reads the committed results file and also accepts a file from
`run.py` dropped into the page, shown beside ours, computed in the
browser.

## 6. Rules for the numbers

0014 §3, and: name the scenario and the machine with every number; show
the losing cases as plainly as the winning ones; a reliability `fail` is
reported with the framework's documented way to fix it, if one exists.

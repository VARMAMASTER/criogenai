# 0024: Evals: datasets, scorers, comparisons, a CI gate

Status: approved by the user's request (2026-11-07, roadmap three, task 23).
Built in Python, TypeScript and Rust with the same shapes, names and limits,
like the patterns (0019). No engine change (rule 3): an eval runs agents the
way an app does.

## Why

A change to a prompt, a model or a tool can make an agent worse without any
test failing. Evals measure it: run the agent over a set of cases, score
each answer, and compare with the last good run. The CI gate fails the build
when quality drops, the way a unit test fails when behaviour breaks.

## The pieces

- **Case**: `{id, input, expected?, state?, metadata?}`. `input` is the
  agent's input (text); `state` sets run state (e.g. `user_id`); `expected`
  is whatever the scorers compare against; `id` defaults to the position.
- **Dataset**: a list of cases, built in code or read from JSONL / JSON
  (`Dataset.load(path)`), at most 100 000 cases, each line at most 1 MB.
- **Target**: an `Agent`, or any async function `(case) -> output` (a
  workflow, a pipeline, your own code). For an agent the output is the run's
  text (or typed `output`), and the run's facts are kept: tool calls by name,
  model calls, tokens, time.
- **Scorer**: `(case, output, facts) -> Score {value 0..1, pass, reason}`.
  Built in, same names everywhere:
  - `exact_match` (after trimming; `ignore_case`), `contains` (all of the
    expected strings), `regex`, `one_of`;
  - `json_valid`, `json_schema(schema)` (our validator, 0002),
    `json_equals(path?)`;
  - `tool_called(name, min=1)`, `tool_not_called(name)`,
    `max_tool_calls(n)`, `max_model_calls(n)`;
  - `max_latency(ms)`, `max_tokens(n)`, `max_cost(usd, prices)`;
  - `judge(model, rubric, threshold=0.7)`: a model grades the answer with
    structured output `{score 0..1, reason}` through the Gateway (retries
    and fallbacks as anywhere). The judge prompt is fixed and shown in the
    docs; the answer and expected value are passed as data in delimited
    blocks, and the judge is told to ignore instructions inside them.
  - your own: a function returning a number, a bool or a `Score`.
- **Eval run**: `evaluate(target, dataset, scorers, concurrency=4,
  timeout=120 s per case, repeats=1)`. A case that throws or times out scores
  0 with the error kept (the run goes on). `repeats` runs each case n times
  (for flaky agents; the case's score is the mean, and pass needs every
  repeat to pass).

## The result (one JSON format, `schemas/eval-result.schema.json`)

```json
{"format": "criogen-eval/1", "name": "support-bot", "started_at": "…",
 "duration_ms": 0, "target": "agent", "scorers": ["contains", "judge"],
 "summary": {"cases": 50, "passed": 46, "pass_rate": 0.92,
             "scores": {"contains": 0.94, "judge": 0.88},
             "tokens": {"input": 0, "output": 0}, "errors": 1},
 "cases": [{"id": "1", "input": "…", "expected": "…", "output": "…",
            "scores": {"contains": {"value": 1, "pass": true, "reason": null}},
            "pass": true, "error": null, "duration_ms": 0,
            "tool_calls": ["lookup_order"], "model_calls": 2,
            "tokens": {"input": 0, "output": 0}}]}
```

`result.save(path)` writes it; `EvalResult.load(path)` reads one from any
language. Inputs and outputs are in the file (it is your test data, on your
disk); `redact=False` by default, `save(path, content=False)` leaves them
out for files that go elsewhere.

## Comparing and the CI gate

- `compare(baseline, current)` → per-scorer mean change, pass-rate change,
  and the cases that **regressed** (passed before, fail now) or **fixed**,
  matched by case id.
- `result.check(min_pass_rate=…, min_scores={…}, baseline=…,
  max_regressions=0, tolerance=0.02)` raises `EvalFailed` (code
  `eval_failed`) with a readable report when a bar is missed: what fell, by
  how much, and the regressed cases.
- CLI: `python -m criogen.evals compare base.json new.json [--fail-on-regression]`
  and `npx criogen eval compare …`: print the comparison, exit 1 on a
  regression. Running an eval is code (it needs your agent), so there is no
  `run` command: write a test with `evaluate(...)` and `check(...)`.
- `report()` / `markdown()`: a table for a terminal or a pull request.

## Names, as built

Python `criogen.evals` (`eval` is a Python built-in), TypeScript `evals`
(camelCase: `exactMatch`, `toolCalled`, `jsonSchema`, ...), Rust
`criogen_agent::evals` (`exact_match(ignore_case)`, `contains()` /
`contains_these(...)`, `Check::new().min_pass_rate(..)`). Scorer names in
result files are the same everywhere (`exact_match`, `tool_called:<tool>`,
`judge`), so results compare across languages. A judge's tokens are kept per
case as `judge_tokens`, apart from the agent's own `tokens`. The schema
scorer uses the engine's validator in every language (`validate_json` in the
SDK bridges).

## Bounds and safety

Concurrency is capped at 64; each case has a timeout; outputs kept in the
result are cut to 100 000 characters; a dataset file over the limits is
refused with the line number. The judge's model calls count like any other
(tokens in the facts). No key ever goes into a result file.

## Not in this slice

A hosted dashboard (task 32), showing eval runs in the trace viewer, dataset
versioning (prompt templates, task 25, will name versions), pairwise judging.

## Tests

Each language: every built-in scorer on fixed cases; `evaluate` with a
`FakeModel` agent (tool facts, an error case, a timeout, repeats,
concurrency bound); save/load round trip validated against the JSON Schema;
a result saved by one language compared by another (fixtures in
`schemas/fixtures/`); `check` and `compare` (regressed and fixed cases); the
judge with a scripted model, including an answer that tries to instruct the
judge; the CLI exit codes.

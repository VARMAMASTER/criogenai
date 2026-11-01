# Playground: Criogen and LangChain, side by side

Type one message; the same job runs in Criogen and in LangChain (with
LangGraph), each in its own process. The page shows each framework's answer, a
timeline of the first run, live CPU and memory charts, and a comparison table.

## What makes it fair

- **The same model server.** Both frameworks talk to one local fake model
  (`mock_model.py`, OpenAI wire format) with **their own real clients**: each
  builds the request, sends it over HTTP, parses the reply and extracts the
  tool calls itself, as with a hosted model. (An earlier version used each
  framework's in-process simulated model; Criogen's ran in Rust and
  LangChain's in Python, which was not like for like.) Only the model's
  *answers* are scripted, identically; no real model and no internet.
- **The same tools**: plain synchronous functions with the same name,
  argument and description, each waiting a fixed time (a database call).
- **Each framework's standard agent, with its defaults**: LangChain's
  `create_agent`, Criogen's `Agent`.
- **The same measurement, from outside**: `psutil` samples each worker's CPU
  and memory every 100 ms, the same code for both; the fake model server runs
  in its own process, so it is charged to neither.
- **Order cancels out**: "Benchmark" runs A, B, B, A (each framework twice, a
  new process each time) and averages the passes, so going first or second,
  or a busy moment, does not favour one.
- **A new process per measurement**, so one framework's leftovers never help
  the other.

## What is still not identical (so read numbers with this in mind)

- Each framework uses its **default** way of calling the model. Checked by
  recording the requests the mock server received: Criogen sends
  `stream: true` (with `stream_options`), so the reply comes as many small
  chunks to parse; LangChain's `ainvoke` sends `stream: false` and gets the
  whole reply at once. If anything this costs Criogen.
- Defaults also differ in what each does for you (for example Criogen
  validates tool arguments and retries transient model errors). We compare
  defaults, not feature sets, and have not measured how much any one default
  contributes.
- The model is a **fake**: a real model adds seconds that both frameworks wait
  for equally, which makes "typical run" nearly the same for both (as it will
  be in production); what separates them is memory, CPU and how they behave
  under load.
- One computer and one run per click. Repeat before believing a small
  difference. The recorded benchmarks (`docs/benchmarks.md`) are the reference.

## Run it

```sh
# once: the two environments (Criogen from the newest wheel in target/wheels,
# LangChain 1.4.6 / LangGraph 1.2.16 pinned, plus its OpenAI client)
python benches/compare/setup.py --only criogen
python benches/compare/setup.py --only langgraph

# every time
target/compare/venvs/criogen/Scripts/python.exe playground/server.py   # Windows
target/compare/venvs/criogen/bin/python playground/server.py           # Linux, macOS
# then open http://127.0.0.1:4600
```

## What the buttons do

- **Run both at once**: live and side by side. They share the computer, so
  they disturb each other a little.
- **Benchmark (one after the other)**: runs A, B, B, A (see above), N timed
  runs each time; reports the typical (median) and slow (95th percentile) run
  and runs per second. Use this for numbers you want to quote.
- **Overhead only (no waiting)**: sets both delays to 0, so what is left is the
  frameworks' own cost. With waiting (the default) both spend ~95% of a run
  waiting, and "typical run" comes out about the same, as it would with a real
  model.
- **At the same time**: how many agent runs each framework runs together.

## How it measures

`server.py` starts the fake model server and one `worker.py` per framework in
that framework's own virtual environment and, every 100 ms, reads the worker's CPU and memory from
outside with `psutil` (the real interpreter and anything it started; the
launcher stub a Windows venv puts in front is not counted). The CPU time and
peak memory include importing the framework.

- *Ready*: from the worker's start to the framework imported and the agent
  built (after Python itself started).
- *First run*: the first call, with everything cold.
- *Typical / slow run*: median and 95th percentile of the N timed runs.
- *Peak memory* and *CPU time*: the whole process, from start to exit.

## Limits (read these before quoting a number)

- One computer, one run. Numbers move between runs; repeat before believing a
  small difference. The recorded benchmarks (`docs/benchmarks.md`) are the
  reference; this page is for looking and exploring.
- A simulated model: real models add their own latency, which both frameworks
  wait for equally.
- The server listens on this computer only, runs one job at a time (so
  measurements stay clean), and limits what the page can ask for (up to 200
  runs, 100 at a time, 5 s delays).

## Tests

```sh
target/compare/venvs/criogen/Scripts/python.exe -m unittest discover -s playground
```

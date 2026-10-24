# 0019: Patterns and specialist agents

Status: approved by the user's request (2026-10-24). Built in Python, TypeScript and Rust (same shapes, names and limits).

## Why

People should not have to wire a review loop or a coordinator by hand every
time. Google Cloud's guide "Choose a design pattern for your agentic AI
system" names 12 patterns. This design gives each one a ready-made,
inspectable, extensible form, and adds **specialist agents**: default agents
for common jobs (researcher, summarizer, critic, ...) that users configure,
extend or replace.

## What exists already, and what does not change

Everything below is built from the current SDK: `Agent`, `Workflow` (nodes,
routes, joins, goto, reducers, suspend, cycles), `agent.as_tool`, threads and
storage. **No engine change** (rule 3): scheduling, merging, durability and
limits stay in Rust. A pattern is a function that returns a graph.

## The 12 patterns

| Guide pattern | AgentBuild | Built how |
|---|---|---|
| Single agent | `Agent` | exists |
| ReAct | `patterns.react(model, tools, max_steps)` | an `Agent` with the loop's limits set and a reasoning prompt |
| Sequential | `patterns.sequential(steps)` | chain of nodes, text flows through |
| Parallel | `patterns.parallel(branches, synthesizer=None)` | fan-out nodes, join, optional synthesizer |
| Loop | `patterns.loop(steps, until, max_iterations)` | body, check node, route back or finish; the cap always ends it |
| Review and critique | `patterns.review_and_critique(generator, critic, max_rounds)` | a loop: generate, critique, approve or revise |
| Iterative refinement | `patterns.iterative_refinement(refine, score, threshold, max_iterations)` | a loop that stops at a score |
| Coordinator | `patterns.coordinator(model, specialists)` | an `Agent` whose tools are the specialists (`as_tool`) |
| Hierarchical decomposition | `patterns.hierarchical(model, tree)` | coordinators of coordinators |
| Swarm | `patterns.swarm(agents, start, max_handoffs)` | nodes that `goto` each other; a hand-off cap and a `FINAL` marker end it |
| Human in the loop | `patterns.human_review(step)` | produce, `suspend` with the draft, resume with approve / edit / reject; durable with storage |
| Custom logic | `Workflow` | exists; `Pattern.from_workflow` gives it the same `run` / `compile` |

## Shape

- A **step** is an `Agent`, a `Specialist` (built with the pattern's `model`), a
  function `text -> text` (sync or async), or `(name, step)`.
- Text flows between steps. State keys: `input`, `current`, `output`, `trace`
  (every step's name and output, appended), plus per-pattern keys (`round`,
  `stop_reason`, `approved`).
- Workflow patterns return a `Pattern`: `await p.run(text)` gives a
  `PatternResult(output, trace, state, stop_reason, suspended)`;
  `p.compile(storage)` gives the normal durable `CompiledWorkflow` (threads,
  resume, audit); `p.workflow` is the plain `Workflow`, so users add nodes and
  edges to extend it.
- Agent patterns (`react`, `coordinator`, `hierarchical`) return an `Agent`,
  so they nest as steps of other patterns.

## Bounds (rule 11)

Every loop has a cap: `max_iterations`, `max_rounds`, `max_handoffs`,
`max_steps`, each with a default and a hard ceiling (50). A pattern ends with a
`stop_reason` (`condition`, `max_iterations`, `approved`, `final`,
`max_handoffs`, `rejected`), never by hanging. Step output is data: a swarm
hand-off to an unknown name ends the run with `stop_reason="unknown_handoff"`.

## Specialists

`Specialist(name, description, instructions, tools=(), output=None, limits=None)`
is a recipe for an agent, not an agent. `specialist.build(model, **overrides)`
returns an `Agent`; `specialist.extend(...)` returns a new recipe (instructions
appended or replaced, tools added, other settings changed); `register` and
`get` keep a registry so workflows and configuration files can refer to them
by name. Built in: `researcher`, `summarizer`, `classifier`, `extractor`,
`planner`, `writer`, `critic`, `fact_checker`, `code_reviewer`, `translator`,
`support_agent`. They are prompts and defaults, honestly described as such:
tools come from the user, and nothing is claimed about quality.

## Per language

Python (`agentbuild.patterns`, `specialists`, `web`), TypeScript
(`patterns`, `specialists`, `web` namespaces; camelCase) and Rust
(`agentbuild_agent::patterns`, `specialists`, and `web` behind the cargo
feature `web`). Differences are only what the language forces: Rust has no model
strings or `AGENTBUILD_MODEL` fallback (specialist steps take an `Arc<dyn Model>`),
returns `Result<_, PatternError>` and a `PatternStop` enum, and extends a pattern
with `Pattern::extend(closure)` because a compiled workflow cannot be edited.
The Rust page reader also pins the connection to the address it checked, so DNS
cannot change between the check and the request.

## Not in this slice

A pattern chooser, streaming events per step, and evaluation harnesses.
Documented as planned, not built.

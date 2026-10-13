# Live check

Runs every AgentBuild feature against real model APIs and prints a
PASS/FAIL table per provider. It complements the automated tests, which use
simulated models: this is how we learn what real providers actually do
(error shapes, tool-calling habits, streaming granularity).

Manual only: it needs API keys and costs a few cents per run. It is not a
workspace member and never runs in CI.

```sh
cd tests/live
cargo run                    # every provider with a key
cargo run -- gemini          # one provider
LIVE_ONLY=approval cargo run # only checks whose name contains "approval"
LIVE_VERBOSE=1 cargo run     # print details in full
```

Keys: `OPENAI_API_KEY`, `GEMINI_API_KEY` (environment), or `KEY=value`
lines in `~/.agentbuild-keys.env` (or the file named by
`AGENTBUILD_KEYS_FILE`). Models: `OPENAI_MODEL` (default `gpt-5-mini`),
`GEMINI_MODEL` (default `gemini-2.5-flash`, through Google's
OpenAI-compatible endpoint).

| Check | What must hold |
|---|---|
| chat | correct answer, token usage reported |
| streaming | text arrives in several deltas that equal the final message |
| tools: one call | the tool runs and the answer uses its result |
| tools: several calls | both cities looked up, correct comparison |
| tools: error result | a `ToolError` reaches the model, which calls again |
| tools: argument validation | an out-of-range argument is refused before the tool runs; the model corrects it |
| output (native, tool) | JSON matching the schema, with correct facts |
| approval (approve, reject, edit) | nothing runs before the decision; then exactly what was decided runs; the model describes an edited call correctly |
| agent.stream | text, tool start/finish and message events |
| threads | memory across turns; a SQLite thread survives reopening; a run killed mid-tool continues with `retry` without charging twice |
| agent as a tool | the outer agent delegates to an inner agent, which uses its own tool |
| hooks | `before_model` and `wrap_tool` see every call |
| limits | `OnLimit::End` stops with reason `limit`; the default fails with `limit_exceeded` |
| fallback | a primary with a bad key fails as `auth`; the fallback answers |
| errors | a bad key is `model_failed` / `auth`, and the key is redacted |
| workflow | LLM nodes with a router take the right branch |
| telemetry | spans reach a local OTLP receiver |
| deadline | the run stops at its deadline |

Real models answer differently each time, so the checks look for facts
any correct answer contains, not exact text. A failure that does not
repeat is usually the model's choice; one that repeats is worth a look.

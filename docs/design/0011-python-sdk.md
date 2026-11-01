# 0011: Python SDK (first slice)

Status: **Accepted** (task 5 of the roadmap in `docs/progress.md`; the user
gave full design freedom). Implements the Python side of
[0007](0007-public-api.md) Levels 1, 2 (tools, typed output, limits),
4 (threads, approvals, streaming) and 5 (memory), on the Rust engine.
Workflows in Python (Level 3), hooks and the gateway (Level 0) are later
slices (§8).

## 1. Shape

```
sdks/python/
  Cargo.toml          crate `criogen-python` (cdylib, not a workspace member)
  pyproject.toml      maturin; wheel `criogen`, module `criogen._native`
  src/lib.rs          the bridge: PyO3, one Tokio runtime per process
  python/criogen/  the Python API (pure Python over `_native`)
  tests/              pytest, simulated models only
```

- **One wheel per platform, any Python ≥ 3.9**: PyO3's stable ABI
  (`abi3-py39`).
- **Rule 3 holds**: the agent loop, validation, retries, storage and
  approvals run in Rust. Python holds the user's tools and builds the
  configuration; it never reimplements engine behaviour.
- Not a workspace member: building a Python extension needs Python, and the
  Rust pipeline must not. `scripts/ci-local.sh` gains a `python` step
  (skipped, and said so, without Python or maturin).

## 2. The API

```python
from criogen import Agent, tool, ToolError, Storage, Memory

@tool
async def get_order(order_id: str) -> dict:
    """Look up an order by id."""
    order = await db.find(order_id)
    if order is None:
        raise ToolError(f"No order {order_id}")   # the model sees it and can recover
    return order

agent = Agent(
    model="openai:gpt-5-mini",                     # key from OPENAI_API_KEY
    instructions="You are a support agent.",
    tools=[get_order],
    approve=["issue_refund"],                      # human approval for these tools
    output=Resolution,                             # a Pydantic model or a JSON Schema dict
    storage="sqlite:///support.db",                # or "postgres://…", or Storage(...)
)
result = await agent.run("Where is order A-1001?")
result.text, result.output, result.usage

chat = agent.thread("user-42", actor="user:ada")
reply = await chat.send("Refund A-1001")
if reply.needs_approval:
    reply = await chat.approve()                   # reject(msg), decide({call_id: Decision})
async for event in chat.stream("Anything else?"):
    if event.type == "text":
        print(event.text, end="")
```

| Python | Rust |
|---|---|
| `Agent(model, instructions, tools, approve, output, limits, storage, memory)` | `AgentBuilder` |
| `await agent.run(input, **state)`; `agent.run_sync(...)` | `Agent::run` (`AgentInput::with` for state) |
| `agent.thread(id, actor=None)` | `Agent::thread`, `Thread::actor` |
| `send`, `approve`, `reject`, `decide`, `retry`, `history`, `rewind`, `delete`, `stream`, `send_sync` | the `Thread` methods; `stream` → `ThreadStream` |
| `@tool` (sync or async, type hints → JSON Schema, docstring → description) | `tool_fn`; arguments validated in Rust before the function runs |
| `ToolError(message, retryable=False)` | `ToolError` |
| `Storage(url, encryption_keys=None, audit=False, retention=None)` | `Storage::connect` / `connect_encrypted`, `with_audit`, `Retention` |
| `Memory.by_state("user_id", "users", recall=5, tools=True)` | `Memory::by_state` |
| `FakeModel([...])`, `FakeReply.text(...)`, `FakeReply.tool_call(name, args)` | `FakeModel` (tests and examples) |
| `OpenAI`, `Gemini`, `Ollama`, `Anthropic`, `OpenAICompatible` (or `"provider:model"` strings) | `Endpoint` presets + `ProviderModel` |

**Models as strings**: `"openai:…"`, `"gemini:…"`, `"anthropic:…"`
(keys from `OPENAI_API_KEY`, `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`),
`"ollama:…"` (no key). Objects give full control (base URL, key).

**Tool schemas** come from the function's type hints: `str`, `int`,
`float`, `bool`, `list[...]`, `dict`, `Optional[...]` / `X | None`,
`Literal[...]`, and Pydantic models (via `model_json_schema`, when Pydantic
is installed). A parameter without a default is required. A parameter
annotated `ToolContext` receives the call's context (call id, run id) and
is left out of the schema. Values are converted with JSON on both sides.

**Results** are plain Python: `result.text`, `result.output` (a Pydantic
instance when `output=` is a model), `result.usage` (dict),
`result.messages` (list of dicts), `reply.approvals` (list of
`ApprovalRequest(call_id, tool, args)`), `reply.version`.

## 3. Errors

`CriogenError(code, message, details)` for everything, with subclasses
by code: `ModelError` (`model_failed`), `ToolExecutionError`
(`tool_failed`), `LimitExceededError`, `ThreadBusyError`,
`ThreadSuspendedError`, `StorageError` (`storage_*`, `encryption_*`,
`audit_failed`), `ValidationError` (`invalid_value`, `invalid_input`,
`resume_invalid`, agent build errors). `e.code` is the stable Rust code
(0003), so code that checks codes works across languages.

## 4. The bridge

- **One Tokio runtime** (multi-threaded) per process, started on first
  use. Every Python call that does I/O returns an asyncio future that a
  Rust task completes with `loop.call_soon_threadsafe`. The GIL is never
  held while Rust waits.
- **Python cancellation reaches Rust**: cancelling the asyncio task
  cancels the run's `CancelToken` (the run fails with `cancelled`, a
  thread is still saved, 0008 §17).
- **Tools** run in Python. A sync tool runs on a worker thread
  (`spawn_blocking`, GIL only while Python runs); an async tool is
  scheduled on the caller's event loop (`run_coroutine_threadsafe`) and
  Rust awaits it. The caller's loop travels with the run (its id is in
  the run's metadata, 0010's `NodeContext::metadata` mechanism), so
  concurrent runs on different loops work.
- **Exceptions** in a tool: `ToolError` → a tool error the model sees;
  any other exception follows `Agent(on_tool_exception=...)` (0003 §5.1):
  by default the model sees its type and message (`tool_failed`) and the
  run goes on; `"return_generic"` hides the text from the model; with
  `"fail"` the run fails with `tool_failed` and the original exception is
  the error's `__cause__` (kept only then).
- **Values** cross as JSON (`json.dumps` / `serde_json`): no Python
  objects inside the engine, and the same data the Rust API sees.
- **Sync wrappers** (`run_sync`, `send_sync`) run the async call with
  `asyncio.run` and refuse to run inside a running loop (a clear error,
  not a deadlock).

## 5. Tests and packaging (rule 9)

- `sdks/python/tests`: pytest with `FakeModel` (no real model, rule 7):
  run, tools (sync, async, `ToolError`, crashing tools, validation by the
  engine), typed output, threads with approval across agent instances,
  SQLite storage surviving a new `Agent`, streaming, cancellation, memory,
  errors by code, the sync wrappers.
- `scripts/test-python.sh`: builds the wheel with maturin, installs it
  into a **new** venv, runs a consumer script that uses only the public
  API, then runs the test suite against the installed wheel.
- The manual live check gets a Python run against OpenAI and Gemini
  (`sdks/python/examples/live.py`), never in CI.

## 6. Versions

pyo3 0.29 (abi3; 0.30 came out days before this slice, so the settled 0.29 line), maturin ≥ 1.8 to build. Pydantic is optional (only for
`output=` models and Pydantic tool parameters).

## 7. Decisions

- `approve=[...]` instead of 0007's `hooks=[Approve(...)]`: approval is
  an agent setting in the Rust API (`AgentBuilder::approval`), not a hook.
  0007 is updated to match.
- Events in `stream` are small objects with `type` in `text`,
  `tool_call_started`, `tool_call_finished`, `message`,
  `approval_needed`, `other` (the Rust `AgentEvent` variants).

## 8. Not in this slice

Python workflows (`Workflow`, Level 3), Python hooks, the gateway
(Level 0), custom storage backends written in Python, semantic memory
with Python embedders (Rust `ProviderEmbedder`s can be named by string
later), wheels for other platforms (built by the manual release workflow
later; locally Windows only).

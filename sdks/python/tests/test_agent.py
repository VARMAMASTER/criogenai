"""The Python SDK (design 0011) with simulated models only: runs, tools,
typed output, threads, approvals, storage, streaming, cancellation,
memory, errors."""

import asyncio
import os
import tempfile
import time
import uuid
from typing import List, Literal, Optional

import pytest

from criogen import (
    Agent,
    HookAction,
    ToolResult,
    Embedder,
    Telemetry,
    CriogenError,
    CancelledError,
    Decision,
    FakeModel,
    FakeReply,
    LimitExceededError,
    Memory,
    Retry,
    Storage,
    StorageError,
    ThreadSuspendedError,
    ToolContext,
    ToolError,
    ToolExecutionError,
    ValidationError,
    tool,
)


def run(coroutine):
    return asyncio.run(coroutine)


def tool_results(result):
    return [m for m in result.messages if m["role"] == "tool"]


@tool
def add(a: int, b: int) -> int:
    """Adds two numbers."""
    return a + b


@tool
async def double(n: int, ctx: ToolContext) -> dict:
    """Doubles a number."""
    await asyncio.sleep(0.01)
    return {"value": n * 2, "call_id": ctx.call_id}


def test_sync_and_async_tools_run_and_their_results_reach_the_model():
    agent = Agent(
        FakeModel([FakeReply.tool_calls([("add", {"a": 2, "b": 3}), ("double", {"n": 4})]), "5 and 8"]),
        tools=[add, double],
    )
    result = agent.run_sync("go")
    assert result.text == "5 and 8"
    assert result.tool_calls == 2
    results = tool_results(result)
    assert results[0]["status"] == "success"
    assert results[0]["content"][0]["value"] == 5
    doubled = results[1]["content"][0]["value"]
    assert doubled["value"] == 8
    assert doubled["call_id"] == results[1]["call_id"]  # ToolContext injected


def test_the_schema_comes_from_type_hints():
    @tool(read_only=True)
    def search(
        query: str,
        limit: int = 5,
        tags: Optional[List[str]] = None,
        order: Literal["new", "old"] = "new",
        exact: bool = False,
        ctx: ToolContext = None,
    ) -> list:
        """Searches."""
        return []

    schema = search.schema
    assert schema["required"] == ["query"]
    props = schema["properties"]
    assert props["query"] == {"type": "string"}
    assert props["limit"] == {"type": "integer"}
    assert props["tags"] == {"anyOf": [{"type": "array", "items": {"type": "string"}}, {"type": "null"}]}
    assert props["order"] == {"enum": ["new", "old"]}
    assert props["exact"] == {"type": "boolean"}
    assert "ctx" not in props
    assert search.description == "Searches."
    assert search.spec()["read_only"] is True
    with pytest.raises(TypeError):

        @tool
        def bad(x: object) -> None:
            pass


def test_tool_errors_and_exceptions_reach_the_model_unless_told_to_fail():
    @tool
    def lookup(order_id: str) -> str:
        """Looks up an order."""
        if order_id == "missing":
            raise ToolError("No such order", retryable=False)
        raise KeyError(order_id)

    agent = Agent(
        FakeModel([FakeReply.tool_call("lookup", {"order_id": "missing"}), "Sorry, not found."]),
        tools=[lookup],
    )
    result = agent.run_sync("where is it")
    assert result.text == "Sorry, not found."
    error = tool_results(result)[0]
    assert error["status"] == "error"
    assert "No such order" in str(error["content"])

    # An exception the tool did not handle reaches the model too (by default).
    script = [FakeReply.tool_call("lookup", {"order_id": "x"}), "The order system failed."]
    result = Agent(FakeModel(script), tools=[lookup]).run_sync("go")
    assert result.text == "The order system failed."
    crash = tool_results(result)[0]
    assert crash["status"] == "error" and crash["error"]["code"] == "tool_failed"
    assert "KeyError" in str(crash["content"])

    # "return_generic": the model only learns that the tool failed.
    quiet = Agent(FakeModel(script), tools=[lookup], on_tool_exception="return_generic").run_sync("go")
    hidden = tool_results(quiet)[0]
    assert "KeyError" not in str(hidden["content"])
    assert "KeyError" in hidden["artifact"]["exception"]

    # "fail": the run fails, with the exception as the cause.
    crashing = Agent(FakeModel(script), tools=[lookup], on_tool_exception="fail")
    with pytest.raises(ToolExecutionError) as caught:
        crashing.run_sync("go")
    assert caught.value.code == "tool_failed"
    assert isinstance(caught.value.__cause__, KeyError)
    assert "KeyError" in caught.value.message

    with pytest.raises(ValidationError):
        Agent(FakeModel([]), on_tool_exception="ignore")


def test_a_transient_model_error_is_retried_by_the_agent():
    agent = Agent(FakeModel(["fine now"], fail_first=1))
    result = agent.run_sync("hello")
    assert result.text == "fine now"


def test_a_by_turn_model_serves_many_runs_at_once():
    @tool
    def lookup(order_id: str) -> str:
        "Looks up an order."
        return f"order {order_id} shipped"

    model = FakeModel([FakeReply.tool_call("lookup", {"order_id": "A-1"}), "done"], by_turn=True)
    agent = Agent(model, tools=[lookup])

    async def many():
        return await asyncio.gather(*(agent.run("where is it?") for _ in range(20)))

    for result in run(many()):
        assert result.text == "done"
        assert len(tool_results(result)) == 1


def test_an_agent_as_a_tool_of_another():
    researcher = Agent(FakeModel(["The order shipped on Monday."]), instructions="Find facts.")
    writer = Agent(
        FakeModel([FakeReply.tool_call("research", {"input": "When did A-1 ship?"}), "It shipped on Monday."]),
        tools=[researcher.as_tool("research", "Looks things up")],
    )
    result = writer.run_sync("When did my order ship?")
    assert result.text == "It shipped on Monday."
    assert "Monday" in str(tool_results(result)[0]["content"])


def test_fallback_models_and_retry_settings():
    # The main model keeps failing with a transient error: after its retries
    # the fallback answers.
    agent = Agent(
        FakeModel(["never"], fail_first=100),
        fallbacks=[FakeModel(["from the fallback"])],
        model_retry=Retry(max_attempts=2, initial_backoff=0.01, jitter=False),
        max_retry_after=5,
    )
    assert agent.run_sync("hi").text == "from the fallback"
    with pytest.raises(ValidationError):
        Agent(FakeModel([]), fallback_on=["no_such_kind"])


def test_the_engine_validates_arguments_before_the_tool_runs():
    calls = []

    @tool
    def charge(amount: int) -> str:
        """Charges."""
        calls.append(amount)
        return "ok"

    agent = Agent(
        FakeModel([FakeReply.tool_call("charge", {"amount": "lots"}), "I could not."]),
        tools=[charge],
    )
    result = agent.run_sync("charge")
    assert calls == []
    assert "Invalid arguments" in str(tool_results(result)[0]["content"])


def test_typed_output_with_a_json_schema():
    schema = {
        "type": "object",
        "properties": {"category": {"type": "string"}, "refund": {"type": "boolean"}},
        "required": ["category", "refund"],
        "additionalProperties": False,
    }
    agent = Agent(
        FakeModel([FakeReply.tool_call("final_output", {"category": "billing", "refund": True})]),
        output=schema,
    )
    result = agent.run_sync("I was charged twice")
    assert result.output == {"category": "billing", "refund": True}


def test_threads_remember_and_approvals_continue_from_another_agent():
    storage = Storage()
    refunds = []

    @tool(destructive=True)
    def refund(order_id: str) -> str:
        """Refunds an order."""
        refunds.append(order_id)
        return "refunded"

    def make_agent(replies):
        return Agent(FakeModel(replies), tools=[refund], approve=["refund"], storage=storage)

    first = make_agent([FakeReply.tool_call("refund", {"order_id": "A-1"})])

    async def scenario():
        chat = first.thread("t-1", actor="user:ada")
        reply = await chat.send("refund A-1")
        assert reply.needs_approval
        assert reply.approvals[0].tool == "refund"
        assert reply.approvals[0].args == {"order_id": "A-1"}
        assert refunds == []
        with pytest.raises(ThreadSuspendedError):
            await chat.send("hello?")
        # Another agent instance (another process, in real life) approves.
        second = make_agent(["Refunded."])
        reply = await second.thread("t-1", actor="user:boss").approve()
        assert reply.text == "Refunded."
        assert refunds == ["A-1"]
        messages = await second.thread("t-1").messages()
        assert [m["role"] for m in messages] == ["user", "assistant", "tool", "assistant"]
        history = await chat.history()
        assert history[0]["status"] == "completed"
        assert await chat.delete() is True
        assert await chat.delete() is False

    run(scenario())


def test_reject_decide_and_rewind():
    agent = Agent(
        FakeModel(
            [
                FakeReply.tool_call("add", {"a": 1, "b": 1}),
                "Not added.",
                FakeReply.tool_call("add", {"a": 1, "b": 1}),
                "Added with edits.",
            ]
        ),
        tools=[add],
        approve=["add"],
    )

    async def scenario():
        chat = agent.thread("t")
        await chat.send("add")
        assert (await chat.reject("no")).text == "Not added."
        reply = await chat.send("add please")
        call_id = reply.approvals[0].call_id
        reply = await chat.decide({call_id: Decision.edit({"a": 2, "b": 3})})
        assert reply.text == "Added with edits."
        assert "5" in str(reply.messages[-2]["content"])
        with pytest.raises(ValidationError) as caught:
            await chat.decide({"nope": Decision.approve()})
        assert caught.value.code == "resume_invalid"
        oldest = (await chat.history(100))[-1]["id"]
        rewound = await chat.rewind(oldest)
        assert rewound["source"] == "fork"

    run(scenario())


def test_sqlite_threads_survive_a_new_agent_and_storage_has_memory_retention_and_audit():
    path = os.path.join(tempfile.gettempdir(), f"criogen-py-{uuid.uuid4().hex}.db")
    url = f"sqlite:///{path}"
    try:
        first = Agent(FakeModel(["Hello Ada"]), storage=Storage(url, audit=True))
        assert first.thread("t", actor="user:ada").send_sync("I am Ada").text == "Hello Ada"
        storage = Storage(url, audit=True, retention={"keep_checkpoints": 1})
        second = Agent(FakeModel(["You are Ada"]), storage=storage)
        reply = second.thread("t").send_sync("Who am I?")
        assert len(reply.messages) == 4

        async def checks():
            await storage.remember("users/ada", "diet", "vegetarian, no peanuts")
            found = await storage.search("users", "peanuts")
            assert found[0]["key"] == "diet"
            assert (await storage.recall("users/ada", "diet"))["value"] == "vegetarian, no peanuts"
            assert await storage.forget_all("users/ada") == 1
            report = await storage.prune()
            assert report["checkpoints_deleted"] > 0
            records = await storage.audit_records(thread_id="t")
            actions = [r["action"] for r in records]
            assert "run.finished" in actions and "model.called" in actions
            assert records[0]["actor"] == "user:ada"
            assert (await storage.verify_audit())["ok"] is True
            await storage.close()
            with pytest.raises(StorageError) as caught:
                await storage.recall("users/ada", "diet")
            assert caught.value.code == "storage_unavailable"

        run(checks())
        run(first.storage.close())
        # Closed, the file can go (on Windows other programs, such as the
        # virus scanner, may hold it for a moment).
        for suffix in ("", "-wal", "-shm"):
            for attempt in range(50):
                try:
                    if os.path.exists(path + suffix):
                        os.remove(path + suffix)
                    break
                except PermissionError:
                    if attempt == 49:
                        raise
                    time.sleep(0.02)
    finally:
        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(path + suffix)
            except OSError:
                pass


def test_encrypted_storage_refuses_to_open_without_its_keys():
    path = os.path.join(tempfile.gettempdir(), f"criogen-py-enc-{uuid.uuid4().hex}.db")
    url = f"sqlite:///{path}"
    keys = "k1:" + "ab" * 32
    try:
        agent = Agent(FakeModel(["secret noted"]), storage=Storage(url, encryption_keys=keys))
        agent.thread("t").send_sync("my secret is pelican")
        with open(path, "rb") as f:
            assert b"pelican" not in f.read()
        with pytest.raises(StorageError) as caught:
            Storage(url)
        assert caught.value.code == "encryption_key_missing"
    finally:
        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(path + suffix)
            except OSError:
                pass


def test_streaming_shows_text_and_approvals_then_the_saved_reply():
    agent = Agent(
        FakeModel(["Hello there", FakeReply.tool_call("add", {"a": 1, "b": 2}), "Three."]),
        tools=[add],
        approve=["add"],
    )

    async def scenario():
        chat = agent.thread("s")
        stream = chat.stream("hi")
        events = [e async for e in stream]
        text = "".join(e.text for e in events if e.type == "text")
        assert text == "Hello there"
        reply = await stream.reply()
        assert reply.text == "Hello there"

        stream = chat.stream("add")
        types = [e.type async for e in stream]
        assert "approval_needed" in types
        assert (await stream.reply()).needs_approval
        stream = chat.stream_approve()
        events = [e async for e in stream]
        assert any(e.type == "tool_call_finished" and e.status == "success" for e in events)
        assert (await stream.reply()).text == "Three."

    run(scenario())


def test_cancelling_the_python_task_cancels_the_run_and_the_thread_is_saved():
    agent = Agent(FakeModel(["slow answer", "retried"], delay=5.0))

    async def scenario():
        chat = agent.thread("c")
        with pytest.raises(asyncio.TimeoutError):
            await asyncio.wait_for(chat.send("hi"), timeout=0.3)
        # The run stops promptly and is saved as failed (cancelled).
        for _ in range(100):
            history = await chat.history()
            if history and history[0]["status"] == "failed":
                break
            await asyncio.sleep(0.02)
        assert history[0]["status"] == "failed"

    run(scenario())


def test_stream_cancel_ends_with_cancelled():
    agent = Agent(FakeModel(["never"], delay=5.0))

    async def scenario():
        stream = agent.thread("x").stream("hi")
        await asyncio.sleep(0.2)
        stream.cancel()
        with pytest.raises(CancelledError):
            await stream.reply()

    run(scenario())


def test_memory_by_state_recalls_per_user():
    storage = Storage()
    agent = Agent(
        FakeModel([FakeReply.tool_call("remember", {"key": "diet", "value": "vegan"}), "Noted.", "Eat lentils."]),
        storage=storage,
        memory=Memory.by_state("user_id", "users"),
    )
    assert agent.run_sync("I am vegan", user_id="ada").text == "Noted."
    found = run(storage.search("users/ada"))
    assert found[0]["value"] == "vegan"
    result = agent.run_sync("What should I eat?", user_id="ada")
    assert result.text == "Eat lentils."
    with pytest.raises(CriogenError) as caught:
        agent.run_sync("no user")
    assert caught.value.code == "invalid_state"


def test_semantic_memory_finds_facts_by_meaning_and_falls_back_to_keywords():
    storage = Storage()
    memory = Memory.shared("facts").semantic(Embedder.fake(synonyms={"automobile": "car"}))
    agent = Agent(
        FakeModel(
            [
                FakeReply.tool_call("remember", {"key": "vehicle", "value": "The customer drives a red car"}),
                "Noted.",
                FakeReply.tool_call("search_memory", {"query": "automobile"}),
                "Found it.",
            ]
        ),
        storage=storage,
        memory=memory,
    )
    assert agent.run_sync("Remember my car").text == "Noted."
    found = agent.run_sync("Which automobile do I have?")
    assert "red car" in str(tool_results(found)[0]["content"])  # no shared word: found by meaning

    broken = Agent(
        FakeModel([FakeReply.tool_call("search_memory", {"query": "red"}), "ok"]),
        storage=storage,
        memory=Memory.shared("facts").semantic(Embedder.fake(failing=True)),
    )
    assert "red car" in str(tool_results(broken.run_sync("search"))[0]["content"])


def test_telemetry_exports_runs_without_their_content():
    import http.server
    import threading

    received = []

    class Collector(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            received.append(self.rfile.read(int(self.headers["Content-Length"])).decode())
            self.send_response(200)
            self.end_headers()

        def log_message(self, *args):
            pass

    server = http.server.HTTPServer(("127.0.0.1", 0), Collector)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        telemetry = Telemetry.otlp(f"http://127.0.0.1:{server.server_port}/v1/traces", service_name="test")
        agent = Agent(FakeModel(["fine"]), telemetry=telemetry)

        async def scenario():
            await agent.run("a private question")
            await telemetry.flush()

        run(scenario())
        assert received, "nothing exported"
        body = "".join(received)
        assert "resourceSpans" in body
        assert "a private question" not in body  # content is left out by default
        assert telemetry.stats()["exported"] > 0
        run(telemetry.shutdown())
    finally:
        server.shutdown()


def test_hooks_change_what_they_may_in_order():
    seen = []

    @tool
    def lookup(order_id: str) -> str:
        "Looks up an order."
        seen.append(order_id)
        return f"order {order_id}: shipped, card 4111-1111"

    @tool
    def weather(city: str) -> str:
        "The weather."
        raise AssertionError("skipped by the hook")

    class Guard:
        name = "guard"

        def on_start(self, ctx):
            return HookAction(state={"runs": (ctx.hook_state or {}).get("runs", 0) + 1})

        async def before_tool(self, call):
            if call.name == "weather":
                return ToolResult("sunny (cached)")
            call.args["order_id"] = call.args["order_id"].upper()
            return call

        def after_tool(self, call, result):
            return result.replace("4111-1111", "[card]") if isinstance(result, str) else None

        def after_model(self, ctx):
            text = ctx.messages[-1]["content"][0].get("text", "") if ctx.messages[-1]["content"] else ""
            if "secret" in text:
                redacted = dict(ctx.messages[-1], content=[{"type": "text", "text": "[redacted]"}])
                return HookAction(message=redacted)
            return None

    ended = []

    class Audit:
        async def on_end(self, ctx, outcome):
            ended.append((outcome, ctx.model_calls))

    agent = Agent(
        FakeModel(
            [
                FakeReply.tool_calls([("lookup", {"order_id": "a-1"}), ("weather", {"city": "Paris"})]),
                "The secret answer",
            ]
        ),
        tools=[lookup, weather],
        hooks=[Guard(), Audit()],
    )
    result = agent.run_sync("where is it?")
    assert seen == ["A-1"]  # before_tool changed the argument
    results = [str(m["content"]) for m in tool_results(result)]
    assert "[card]" in results[0] and "4111" not in results[0]  # after_tool redacted
    assert "sunny (cached)" in results[1]  # skipped the tool
    assert result.text == "[redacted]"  # after_model replaced the reply
    assert ended == [("completed", 2)]


def test_a_hook_can_end_the_run_and_its_exceptions_fail_it_with_the_cause():
    class Budget:
        def before_model(self, ctx):
            if ctx.model_calls >= 1:
                return HookAction(end=True, output="stopped by the budget hook")
            return None

    agent = Agent(FakeModel([FakeReply.tool_call("noop", {}), "never"], by_turn=True), hooks=[Budget()])
    result = agent.run_sync("go")
    assert result.output == "stopped by the budget hook"

    class Broken:
        def before_model(self, ctx):
            raise KeyError("missing setting")

    with pytest.raises(CriogenError) as caught:
        Agent(FakeModel(["x"]), hooks=[Broken()]).run_sync("go")
    assert caught.value.code == "hook_failed"
    assert isinstance(caught.value.__cause__, KeyError)
    with pytest.raises(TypeError):
        Agent(FakeModel(["x"]), hooks=[object()])

    # In a tool hook it is the tool's crash: with "fail", the run fails with it as the cause.
    class Leaky:
        def after_tool(self, call, result):
            raise ValueError("cannot redact")

    @tool
    def ping() -> str:
        "Pings."
        return "pong"

    crashing = Agent(
        FakeModel([FakeReply.tool_call("ping", {}), "done"]), tools=[ping], hooks=[Leaky()], on_tool_exception="fail"
    )
    with pytest.raises(ToolExecutionError) as failed:
        crashing.run_sync("go")
    assert isinstance(failed.value.__cause__, ValueError)


def test_limits_errors_and_the_sync_wrappers():
    looping = Agent(
        FakeModel([FakeReply.tool_call("add", {"a": 1, "b": 1})] * 5),
        tools=[add],
        limits={"model_calls": 2},
    )
    with pytest.raises(LimitExceededError) as caught:
        looping.run_sync("loop")
    assert caught.value.code == "limit_exceeded"

    async def inside_a_loop():
        with pytest.raises(RuntimeError, match="cannot run inside an event loop"):
            looping.run_sync("x")

    run(inside_a_loop())
    with pytest.raises(TypeError):
        Agent(FakeModel([]), tools=[lambda: 1])
    with pytest.raises(ValueError):
        Agent("nope")


def test_models_by_name_read_keys_from_the_environment_and_never_show_them():
    from criogen import OpenAI
    from criogen.models import model_spec

    os.environ["OPENAI_API_KEY"] = "sk-test-not-real"
    try:
        spec = model_spec("openai:gpt-5-mini")
        assert spec == {"kind": "openai", "model": "gpt-5-mini", "api_key": "sk-test-not-real"}
        assert "sk-test" not in repr(OpenAI("gpt-5-mini"))
        assert model_spec("ollama:llama3.2")["kind"] == "ollama"
    finally:
        del os.environ["OPENAI_API_KEY"]


def test_cancel_reaches_a_stream_that_another_task_is_reading():
    agent = Agent(FakeModel(["never"], delay=5.0))

    async def scenario():
        stream = agent.thread("y").stream("hi")

        async def read():
            return [e.type async for e in stream]

        reader = asyncio.ensure_future(read())
        await asyncio.sleep(0.2)  # the reader now waits inside next()
        started = asyncio.get_running_loop().time()
        stream.cancel()
        await asyncio.wait_for(reader, timeout=2.0)
        with pytest.raises(CancelledError):
            await stream.reply()
        assert asyncio.get_running_loop().time() - started < 2.0

    run(scenario())


def test_every_catalog_code_raises_its_family_class():
    """schemas/error-codes.json (design 0013): the scripts pass its path."""
    import json
    import pathlib

    import criogen
    from criogen.errors import error_for

    path = os.environ.get("CRIOGEN_ERROR_CATALOG") or str(
        pathlib.Path(__file__).resolve().parents[3] / "schemas" / "error-codes.json"
    )
    catalog = json.load(open(path, encoding="utf-8"))
    classes = {
        "model": criogen.ModelError,
        "tool": criogen.ToolExecutionError,
        "limit": criogen.LimitExceededError,
        "busy": criogen.ThreadBusyError,
        "suspended": criogen.ThreadSuspendedError,
        "storage": criogen.StorageError,
        "validation": criogen.ValidationError,
        "cancelled": criogen.CancelledError,
        "other": criogen.CriogenError,
    }
    checked = 0
    for entry in catalog["codes"]:
        family = entry["family"]
        if family is None:
            continue
        error = error_for(entry["code"], "x")
        assert type(error) is classes[family], (entry["code"], type(error).__name__, family)
        assert error.code == entry["code"]
        checked += 1
    assert checked > 20


def test_concurrent_runs_get_their_own_hook_exception():
    # Two runs of one agent fail in the same hook at the same moment: each
    # must get the exception raised for it, never the other's (security audit).
    class Flaky:
        name = "flaky"

        async def before_model(self, ctx):
            text = ctx.messages[-1]["content"][0]["text"]
            await asyncio.sleep(0.2 if text == "slow" else 0.02)
            raise ValueError(f"for {text}")

    agent = Agent(FakeModel(["never"]), hooks=[Flaky()])

    async def both():
        return await asyncio.gather(agent.run("slow"), agent.run("fast"), return_exceptions=True)

    slow, fast = run(both())
    assert isinstance(slow.__cause__, ValueError) and str(slow.__cause__) == "for slow"
    assert isinstance(fast.__cause__, ValueError) and str(fast.__cause__) == "for fast"


def test_credentials_are_not_sent_over_plain_http_to_another_machine():
    from criogen import OpenAICompatible

    with pytest.raises(CriogenError) as refused:
        Agent(OpenAICompatible("m", base_url="http://gpu-box:8000/v1", api_key="a-secret-key-1234"))
    assert "plain http" in str(refused.value) and "a-secret-key-1234" not in str(refused.value)
    # Explicitly allowed for a trusted network; this machine and https need nothing.
    Agent(OpenAICompatible("m", base_url="http://gpu-box:8000/v1", api_key="k-12345678", allow_insecure_http=True))
    Agent(OpenAICompatible("m", base_url="http://localhost:11434/v1", api_key="k-12345678"))
    Agent(OpenAICompatible("m", base_url="https://gpu-box/v1", api_key="k-12345678"))


def test_nested_type_definitions_are_hoisted_so_the_engine_checks_them():
    class Patient:
        """Stands in for a Pydantic model: it only needs these two."""

        @staticmethod
        def model_json_schema():
            return {
                "type": "object",
                "properties": {"name": {"type": "string"}, "address": {"$ref": "#/$defs/Address"}},
                "required": ["name"],
                "$defs": {"Address": {"type": "object", "properties": {"city": {"type": "string"}}}},
            }

        @staticmethod
        def model_validate(value):
            return value

    @tool
    def admit(patient: Patient) -> str:
        "Admits a patient."
        return "admitted"

    # `$defs` sit at the root, where `#/$defs/Address` is resolved.
    assert admit.schema["$defs"]["Address"]["type"] == "object"
    assert "$defs" not in admit.schema["properties"]["patient"]
    # A wrong nested value reaches the model as invalid arguments.
    result = Agent(
        FakeModel([FakeReply.tool_call("admit", {"patient": {"name": "Ada", "address": {"city": 5}}}), "ok"]),
        tools=[admit],
    ).run_sync("go")
    assert tool_results(result)[0]["error"]["code"] == "invalid_arguments"

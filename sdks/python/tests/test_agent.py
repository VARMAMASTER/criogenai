"""The Python SDK (design 0011) with simulated models only: runs, tools,
typed output, threads, approvals, storage, streaming, cancellation,
memory, errors."""

import asyncio
import os
import tempfile
import uuid
from typing import List, Literal, Optional

import pytest

from agentbuild import (
    Agent,
    AgentBuildError,
    CancelledError,
    Decision,
    FakeModel,
    FakeReply,
    LimitExceededError,
    Memory,
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


def test_tool_errors_reach_the_model_and_crashes_fail_the_run_with_the_cause():
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

    crashing = Agent(FakeModel([FakeReply.tool_call("lookup", {"order_id": "x"})]), tools=[lookup])
    with pytest.raises(ToolExecutionError) as caught:
        crashing.run_sync("go")
    assert caught.value.code == "tool_failed"
    assert isinstance(caught.value.__cause__, KeyError)
    assert "KeyError" in caught.value.message


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
    path = os.path.join(tempfile.gettempdir(), f"agentbuild-py-{uuid.uuid4().hex}.db")
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

        run(checks())
    finally:
        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(path + suffix)
            except OSError:
                pass


def test_encrypted_storage_refuses_to_open_without_its_keys():
    path = os.path.join(tempfile.gettempdir(), f"agentbuild-py-enc-{uuid.uuid4().hex}.db")
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
    with pytest.raises(AgentBuildError) as caught:
        agent.run_sync("no user")
    assert caught.value.code == "invalid_state"


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
    from agentbuild import OpenAI
    from agentbuild.models import model_spec

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

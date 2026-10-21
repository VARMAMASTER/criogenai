"""Workflows in Python (design 0015), run by the Rust engine. Simulated
models only."""

import asyncio

import pytest

from agentbuild import (
    END,
    Agent,
    AgentBuildError,
    CancelledError,
    FakeModel,
    Storage,
    ValidationError,
    Workflow,
    goto,
    suspend,
)


def run(coroutine):
    return asyncio.run(coroutine)


def test_a_linear_workflow_with_sync_and_async_nodes():
    wf = Workflow()

    @wf.node
    def clean(state):
        return {"text": state["text"].strip().lower()}

    @wf.node
    async def count(state, ctx):
        await asyncio.sleep(0.01)
        return {"words": len(state["text"].split()), "counted_at": ctx.step}

    wf.start("clean").edge("clean", "count").edge("count", END)
    result = wf.compile().run_sync({"text": "  Hello Big World "})
    assert result.state["text"] == "hello big world"
    assert result.state["words"] == 3
    assert result.state["counted_at"] == 1
    assert result.steps == 2
    assert not result.suspended


def test_routes_parallel_nodes_joins_and_reducers():
    wf = Workflow(reducers={"log": "append"})
    wf.add_node("classify", lambda s: {"log": ["classify"]})
    wf.add_node("search_web", lambda s: {"log": ["web"], "web": 1})
    wf.add_node("search_docs", lambda s: {"log": ["docs"], "docs": 2})
    wf.add_node("answer", lambda s: {"answer": s["web"] + s["docs"]})
    wf.add_node("refuse", lambda s: {"answer": None})
    wf.start("classify")
    wf.route("classify", lambda s: "ok" if s["question"] else "empty", {"ok": "fan", "empty": "refuse"})
    wf.add_node("fan", lambda s: None)
    wf.edge("fan", "search_web").edge("fan", "search_docs")
    wf.join(["search_web", "search_docs"], "answer")
    wf.edge("answer", END).edge("refuse", END)
    app = wf.compile()
    result = app.run_sync({"question": "why?"})
    assert result.state["answer"] == 3
    # Both parallel writes to `log` kept, in node order.
    assert result.state["log"] == ["classify", "docs", "web"] or result.state["log"] == ["classify", "web", "docs"]
    assert len(result.state["log"]) == 3
    assert app.run_sync({"question": ""}).state["answer"] is None


def test_goto_chooses_the_next_node():
    wf = Workflow()
    wf.add_node("check", lambda s: goto("big" if s["n"] > 10 else "small", {"checked": True}))
    wf.add_node("big", lambda s: {"size": "big"})
    wf.add_node("small", lambda s: {"size": "small"})
    wf.start("check").goto("check", ["big", "small"]).edge("big", END).edge("small", END)
    app = wf.compile()
    assert app.run_sync({"n": 50}).state == {"n": 50, "checked": True, "size": "big"}
    assert app.run_sync({"n": 1}).state["size"] == "small"


def approval_workflow():
    wf = Workflow()

    @wf.node
    def approve(state, ctx):
        if ctx.resume is None:
            return suspend({"refund": state["amount"]}, {"status": "waiting"})
        return {"approved": ctx.resume == "yes", "status": "decided"}

    paid = []

    @wf.node
    def pay(state):
        if state["approved"]:
            paid.append(state["amount"])
        return {"paid": state["approved"]}

    wf.start("approve").edge("approve", "pay").edge("pay", END)
    return wf, paid


def test_a_node_pauses_for_a_person_and_resumes():
    wf, paid = approval_workflow()
    app = wf.compile()

    async def scenario():
        result = await app.run({"amount": 40})
        assert result.suspended
        assert result.interrupts[0].node == "approve"
        assert result.interrupts[0].payload == {"refund": 40}
        assert result.state["status"] == "waiting"
        resumed = await app.resume(result.snapshot, {"approve": "yes"})
        assert resumed.state["paid"] is True
        assert paid == [40]
        with pytest.raises(AgentBuildError) as caught:
            await app.resume(result.snapshot, {"pay": "yes"})
        assert caught.value.code == "resume_invalid"

    run(scenario())


def test_threads_keep_paused_runs_and_another_handle_resumes():
    storage = Storage(audit=True)
    wf, paid = approval_workflow()

    async def scenario():
        thread = wf.compile(storage=storage).thread("refund-1", actor="user:ada")
        turn = await thread.run({"amount": 25})
        assert turn.suspended and turn.version > 0
        assert (await thread.interrupts())[0].payload == {"refund": 25}
        with pytest.raises(AgentBuildError) as caught:
            await thread.run({"amount": 1})
        assert caught.value.code == "thread_suspended"
        # Another compiled copy (another process, in real life).
        other = wf.compile(storage=storage).thread("refund-1", actor="user:boss")
        turn = await other.resume({"approve": "yes"})
        assert turn.state["paid"] is True and paid == [25]
        assert (await thread.state())["status"] == "decided"
        history = await thread.history()
        assert history[0]["status"] == "completed"
        records = await storage.audit_records(thread_id="refund-1")
        assert [r["actor"] for r in records if r["action"] == "run.resumed"] == ["user:boss"]
        assert await thread.delete() is True

    run(scenario())


def test_a_failing_node_keeps_its_exception_and_a_thread_retries_it():
    wf = Workflow()
    calls = {"prepare": 0, "send": 0}
    flaky = {"down": True}

    @wf.node
    def prepare(state):
        calls["prepare"] += 1
        return {"prepared": True}

    @wf.node
    def send(state):
        calls["send"] += 1
        if flaky["down"]:
            raise ConnectionError("mail server down")
        return {"sent": True}

    wf.start("prepare").edge("prepare", "send").edge("send", END)
    app = wf.compile(storage=Storage())

    async def scenario():
        thread = app.thread("mail-1")
        with pytest.raises(AgentBuildError) as caught:
            await thread.run({})
        assert caught.value.code == "node_failed"
        assert caught.value.details["node"] == "send"
        assert isinstance(caught.value.__cause__, ConnectionError)
        flaky["down"] = False
        turn = await thread.retry()
        assert turn.state["sent"] is True
        assert calls == {"prepare": 1, "send": 2}

    run(scenario())


def test_an_agent_as_a_node():
    agent = Agent(FakeModel(["Refund approved."]))
    wf = Workflow(reducers={"messages": "upsert_by_id"})
    # Messages merge by id (upsert_by_id), so each one carries an id.
    question = {"id": "m1", "role": "user", "content": [{"type": "text", "text": "Refund?"}]}
    wf.add_node("ask", lambda s: {"messages": [question]})
    wf.add_node("agent", agent)
    wf.start("ask").edge("ask", "agent").edge("agent", END)
    result = wf.compile().run_sync({})
    assert [m["role"] for m in result.state["messages"]] == ["user", "assistant"]
    assert result.state["messages"][-1]["content"][0]["text"] == "Refund approved."


def test_compile_reports_every_problem_and_routers_must_be_plain_functions():
    wf = Workflow()
    wf.add_node("a", lambda s: None)
    wf.add_node("orphan", lambda s: None)
    wf.edge("a", "missing")
    with pytest.raises(ValidationError) as caught:
        wf.compile()
    issues = caught.value.details["issues"]
    assert len(issues) >= 3, issues  # no entry, unknown node, unreachable / dead end
    assert any("missing" in i for i in issues)

    async def router(state):
        return "x"

    wf2 = Workflow()
    wf2.add_node("a", lambda s: None).start("a").route("a", router, {"x": END})
    with pytest.raises(TypeError):
        wf2.compile()
    with pytest.raises(ValueError):
        Workflow().add_node("a", lambda s: None).add_node("a", lambda s: None)


def test_a_node_returning_the_wrong_thing_fails_clearly_and_cancel_stops_a_run():
    wf = Workflow()
    wf.add_node("bad", lambda s: 42).start("bad").edge("bad", END)
    with pytest.raises(AgentBuildError) as caught:
        wf.compile().run_sync({})
    assert caught.value.code == "node_failed"
    assert isinstance(caught.value.__cause__, TypeError)

    slow = Workflow()

    @slow.node
    async def wait(state):
        await asyncio.sleep(10)

    slow.start("wait").edge("wait", END)
    app = slow.compile()

    async def scenario():
        with pytest.raises(asyncio.TimeoutError):
            await asyncio.wait_for(app.run({}), timeout=0.3)

    run(scenario())
    with pytest.raises(AgentBuildError) as caught:
        slow.compile().run_sync({}, deadline=0.2)
    assert caught.value.code == "deadline_exceeded"
    assert CancelledError  # exported

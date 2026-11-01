"""Package smoke test (design 0011 §5): run against the installed wheel by
scripts/test-python.sh, using only the public API. Simulated models only."""

import asyncio
import os
import tempfile
import uuid

import criogen
from criogen import END, Agent, FakeModel, FakeReply, Storage, ToolError, ValidationError, Workflow, suspend, tool


@tool
async def lookup(order_id: str) -> dict:
    """Looks up an order."""
    if order_id != "A-1":
        raise ToolError(f"No order {order_id}")
    return {"id": order_id, "status": "shipped"}


async def main():
    assert criogen.__file__ and "site-packages" in criogen.__file__, criogen.__file__
    path = os.path.join(tempfile.gettempdir(), f"criogen-consumer-{uuid.uuid4().hex}.db")
    storage = Storage(f"sqlite:///{path}", audit=True)
    agent = Agent(
        FakeModel([FakeReply.tool_call("lookup", {"order_id": "A-1"}), "It shipped."]),
        tools=[lookup],
        approve=["lookup"],
        storage=storage,
    )
    chat = agent.thread("consumer", actor="user:test")
    reply = await chat.send("Where is A-1?")
    assert reply.needs_approval
    stream = chat.stream_approve()
    kinds = [event.type async for event in stream]
    assert "tool_call_finished" in kinds
    assert (await stream.reply()).text == "It shipped."
    assert (await storage.verify_audit())["ok"]
    try:
        await chat.decide({"nope": {"type": "approve"}})
    except ValidationError as error:
        assert error.code == "resume_invalid"
    # A workflow that pauses for a person, then resumes (design 0015).
    wf = Workflow()
    wf.add_node("ask", lambda s, ctx: suspend({"refund": s["amount"]}) if ctx.resume is None else {"ok": ctx.resume})
    wf.start("ask").edge("ask", END)
    app = wf.compile()
    paused = await app.run({"amount": 40})
    assert paused.suspended and paused.interrupts[0].payload == {"refund": 40}
    assert (await app.resume(paused.snapshot, {"ask": True})).state["ok"] is True
    # Patterns and specialists (design 0019): a one-line pipeline and a review loop.
    from criogen import patterns, specialists

    flow = patterns.quick("researcher > writer", model=FakeModel(["notes", "article"]))
    piped = await flow.run("topic")
    assert [t["step"] for t in piped.trace] == ["researcher", "writer"] and piped.stop_reason == "done"
    reviewed = await patterns.review_and_critique(
        Agent(FakeModel(["draft"])), Agent(FakeModel(["APPROVED"]))
    ).run("write")
    assert reviewed.stop_reason == "approved" and "researcher" in specialists.available()
    print("python package test: ok", criogen.__version__)
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(path + suffix)
        except OSError:
            pass


asyncio.run(main())

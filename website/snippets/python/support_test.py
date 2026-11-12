# The tutorial's tests (docs: tutorial/support-agent): pytest support_test.py
# [show]
import asyncio

from criogenai import FakeModel, FakeReply, Storage
from support_agent import build_agent


def test_a_refund_waits_for_a_person() -> None:
    async def scenario() -> None:
        model = FakeModel([
            FakeReply.tool_call("issue_refund", {"order_id": "A-1001", "amount": 20, "reason": "broken"}),
            "I have refunded 20.",
        ])
        agent = build_agent(model, Storage())  # in memory: nothing left behind
        chat = agent.thread("t-1")

        reply = await chat.send("Please refund 20 on A-1001", customer_id="c-7")
        assert reply.needs_approval
        assert reply.approvals[0].args["amount"] == 20

        reply = await chat.approve()
        assert reply.text == "I have refunded 20."
        assert reply.tool_calls == 1

    asyncio.run(scenario())


def test_an_unknown_order_goes_back_to_the_model() -> None:
    model = FakeModel([FakeReply.tool_call("get_order", {"order_id": "Z-9"}), "Which order?"])
    agent = build_agent(model, Storage())
    result = agent.run_sync("Where is Z-9?", customer_id="c-7")
    tool_message = result.messages[2]
    assert tool_message["status"] == "error"
    assert "No order Z-9" in tool_message["content"][0]["text"]
    assert result.text == "Which order?"
# [/show]

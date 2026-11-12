# Docs: guides/testing. Checked with mypy; every block runs with pytest.
import asyncio

from criogenai import Agent, FakeModel, FakeReply, Retry, Storage, tool


@tool
def get_order(order_id: str) -> dict:
    """Look up an order."""
    return {"status": "shipped"}


# [show script]
def test_the_agent_looks_the_order_up() -> None:
    model = FakeModel([
        FakeReply.tool_call("get_order", {"order_id": "A-1001"}),  # 1st model call
        "Your order has shipped.",                                  # 2nd model call
    ])
    agent = Agent(model, tools=[get_order])
    result = agent.run_sync("Where is A-1001?")

    assert result.text == "Your order has shipped."
    assert (result.model_calls, result.tool_calls) == (2, 1)
    tool_message = result.messages[2]  # user, assistant (the call), tool, assistant
    assert tool_message["tool"] == "get_order" and tool_message["status"] == "success"
# [/show script]


# [show by_turn]
def test_many_conversations_at_once() -> None:
    # by_turn: the reply depends on the model call's place in its run, not on
    # how many calls came before; one model serves any number of runs.
    model = FakeModel([FakeReply.tool_call("get_order", {"order_id": "A-1"}), "Shipped."], by_turn=True)
    agent = Agent(model, tools=[get_order], storage=Storage())

    async def scenario() -> None:
        replies = await asyncio.gather(*(agent.thread(f"t-{i}").send("Where?") for i in range(20)))
        assert all(r.text == "Shipped." for r in replies)

    asyncio.run(scenario())
# [/show by_turn]


# [show failures]
def test_a_rate_limit_is_retried() -> None:
    # The first two calls fail with rate_limited (transient), the third answers.
    model = FakeModel(["Hello."], fail_first=2)
    agent = Agent(model, model_retry=Retry(max_attempts=3, initial_backoff=0.01))
    assert agent.run_sync("Hi").text == "Hello."


def test_the_fallback_answers_when_the_first_model_is_down() -> None:
    down = FakeModel(["never"], fail_first=100, name="primary")
    backup = FakeModel(["From the backup."], name="backup")
    agent = Agent(down, fallbacks=[backup], model_retry=Retry(max_attempts=1))
    result = agent.run_sync("Hi")
    assert result.text == "From the backup."
    assert result.answered_by == {"provider": "fake", "model": "backup"}
# [/show failures]

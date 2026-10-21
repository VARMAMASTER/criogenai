from typing import Literal, Optional

from pydantic import BaseModel

# [show]
from agentbuild import Agent, FakeModel, FakeReply, ToolContext, ToolError, tool


@tool(destructive=True)
def issue_refund(order_id: str, amount: float, note: Optional[str] = None, ctx: Optional[ToolContext] = None) -> dict:
    """Refund an order."""
    # Arguments arrive already checked against the schema built from the type hints.
    if amount > 200:
        raise ToolError("Refunds over 200 need a manager")
    return {"refunded": amount}


class Resolution(BaseModel):
    category: Literal["billing", "shipping", "other"]
    refund: bool


agent = Agent(
    "openai:gpt-5-mini",
    instructions="You are a support agent.",
    tools=[issue_refund],
    output=Resolution,  # result.output is a Resolution
    limits={"model_calls": 10, "tool_calls": 20},
)

# In tests: scripted replies, no network, the same answer every time.
test_agent = Agent(
    FakeModel([FakeReply.tool_call("issue_refund", {"order_id": "A-1001", "amount": 24}), "Refunded."]),
    tools=[issue_refund],
)
result = test_agent.run_sync("Refund my order")
assert result.text == "Refunded."
# [/show]

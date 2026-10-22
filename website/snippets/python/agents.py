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


def subagents() -> None:
    # [show subagents]
    from agentbuild import Agent

    researcher = Agent("openai:gpt-5-mini", instructions="Find facts about orders. Be precise.")
    support = Agent(
        "openai:gpt-5-mini",
        instructions="You are a support agent.",
        # The researcher becomes a tool: it takes {"input": text} and answers.
        tools=[researcher.as_tool("research", "Looks up facts about an order")],
    )
    # [/show subagents]


def hooks() -> None:
    # [show hooks]
    from agentbuild import Agent, HookAction, HookContext, ToolCall, ToolError

    class Guard:
        """Blocks large refunds, hides card numbers, stops long runs."""

        def before_tool(self, call: ToolCall) -> None:
            if call.name == "issue_refund" and call.args["amount"] > 500:
                raise ToolError("Refunds over 500 need a manager.")  # the model sees this

        def after_tool(self, call: ToolCall, result: object) -> object:
            # The model sees the returned value instead (None keeps the result).
            return result.replace("4111-1111-1111-1111", "[card]") if isinstance(result, str) else None

        def before_model(self, ctx: HookContext) -> Optional[HookAction]:
            if ctx.model_calls >= 10:
                return HookAction(end=True, output="Stopped after 10 model calls.")
            return None

    support = Agent("openai:gpt-5-mini", tools=[issue_refund], hooks=[Guard()])
    # [/show hooks]

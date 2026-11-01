import asyncio

# [show]
from criogen import Agent, ToolError, tool


@tool
async def get_order(order_id: str) -> dict:
    """Look up an order by id."""
    if order_id != "A-1001":
        raise ToolError(f"No order {order_id}")  # the model sees this and can recover
    return {"status": "shipped", "eta": "tomorrow"}


agent = Agent(
    "openai:gpt-5-mini",  # key from OPENAI_API_KEY
    instructions="You are a support agent. Be brief.",
    tools=[get_order],
)


async def main() -> None:
    result = await agent.run("Where is order A-1001?")
    print(result.text)


asyncio.run(main())
# [/show]

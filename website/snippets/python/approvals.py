from agentbuild import tool


@tool
def issue_refund(order_id: str, amount: float) -> str:
    """Refund an order."""
    return "refunded"


async def main() -> None:
    # [show]
    from agentbuild import Agent, Decision, Storage

    agent = Agent(
        "openai:gpt-5-mini",
        tools=[issue_refund],
        approve=["issue_refund"],  # a person decides before it runs
        storage=Storage("sqlite:///support.db"),
    )

    chat = agent.thread("ticket-7", actor="user:ada")
    reply = await chat.send("Order A-1001 arrived broken, please refund it")
    if reply.needs_approval:
        for request in reply.approvals:
            print(request.tool, request.args)

    # Hours later, in another process: the approval waited in the database.
    reply = await agent.thread("ticket-7", actor="user:dr-rao").approve()
    # Or: await chat.reject("Refund policy says no")
    #     await chat.decide({call_id: Decision.edit({"order_id": "A-1001", "amount": 12})})
    # [/show]
    _ = Decision.approve()

from criogenai import tool


@tool
def issue_refund(order_id: str, amount: float) -> str:
    """Refund an order."""
    return "refunded"


async def main() -> None:
    # [show]
    from criogenai import Agent, Decision, Storage

    agent = Agent(
        "openai:gpt-5-mini",
        tools=[issue_refund],
        approve=["issue_refund"],  # a person decides before it runs
        storage=Storage("sqlite://support.db"),
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


async def per_call(agent: "Agent") -> None:
    from criogenai import Decision

    chat = agent.thread("ticket-9", actor="user:dr-rao")
    # [show decide]
    reply = await chat.send("Refund order A-1001 and email the customer")
    decisions = {}
    for request in reply.approvals:
        if request.tool == "issue_refund" and request.args["amount"] > 100:
            # Run it with other arguments; the model is told what actually ran.
            decisions[request.call_id] = Decision.edit({**request.args, "amount": 100})
        elif request.tool == "send_email":
            # Do not run it; this text is the tool's result.
            decisions[request.call_id] = Decision.respond("Not sent: the customer opted out of email.")
        else:
            decisions[request.call_id] = Decision.approve()
    reply = await chat.decide(decisions)  # one decision per pending call, or resume_invalid
    # [/show decide]


from criogenai import Agent  # noqa: E402

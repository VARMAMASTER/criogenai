from criogen import END, NodeContext, Storage, Workflow, suspend


async def main() -> None:
    # [show]
    wf = Workflow()

    @wf.node
    def classify(state: dict) -> dict:
        return {"category": "billing" if "charge" in state["text"] else "other"}

    @wf.node
    def approve(state: dict, ctx: NodeContext) -> object:
        # Pause for a person; continue when someone answers.
        if ctx.resume is None:
            return suspend({"refund": state["amount"]})
        return {"approved": ctx.resume == "yes"}

    @wf.node
    def answer(state: dict) -> dict:
        return {"reply": "A human will follow up."}

    wf.start("classify")
    wf.route("classify", lambda s: s["category"], {"billing": "approve", "other": "answer"})
    wf.edge("approve", END).edge("answer", END)
    app = wf.compile(storage=Storage("sqlite:///app.db"))  # every graph problem, reported at once

    # Durable: the pause waits in the database, for days if needed.
    turn = await app.thread("ticket-7").run({"text": "I was charged twice", "amount": 40})
    if turn.suspended:
        # Later, from any process:
        await app.thread("ticket-7", actor="user:boss").resume({"approve": "yes"})
    # [/show]

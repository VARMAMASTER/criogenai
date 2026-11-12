from criogenai import END, NodeContext, Storage, Workflow, suspend


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
    app = wf.compile(storage=Storage("sqlite://app.db"))  # every graph problem, reported at once

    # Durable: the pause waits in the database, for days if needed.
    turn = await app.thread("ticket-7").run({"text": "I was charged twice", "amount": 40})
    if turn.suspended:
        # Later, from any process:
        await app.thread("ticket-7", actor="user:boss").resume({"approve": "yes"})
    # [/show]


async def parallel() -> None:
    # [show parallel]
    from criogenai import END, Workflow

    # Two checks run at the same time; "summary" waits for both.
    wf = Workflow(reducers={"findings": "append"})  # both write "findings": append, not replace

    @wf.node
    def intake(state: dict) -> dict:
        return {"findings": []}

    @wf.node
    async def check_allergies(state: dict) -> dict:
        return {"findings": ["no known allergies"]}

    @wf.node
    async def check_interactions(state: dict) -> dict:
        return {"findings": ["no interactions"]}

    @wf.node
    def summary(state: dict) -> dict:
        return {"report": "; ".join(state["findings"])}

    wf.start("intake")
    wf.edge("intake", "check_allergies").edge("intake", "check_interactions")
    wf.join(["check_allergies", "check_interactions"], "summary")
    wf.edge("summary", END)

    result = await wf.compile().run({"patient_ref": "P-1"})
    print(result.state["report"], result.steps)  # 3 steps: intake, both checks, summary
    # [/show parallel]


async def loop() -> None:
    # [show loop]
    from criogenai import END, Workflow, goto

    wf = Workflow()

    @wf.node
    def draft(state: dict) -> dict:
        return {"text": f"draft {state.get('round', 0) + 1}", "round": state.get("round", 0) + 1}

    @wf.node
    def review(state: dict) -> object:
        # A node can choose what runs next, among targets declared below.
        if state["round"] < 3:
            return goto("draft", {"feedback": "shorter"})
        return goto(END)

    wf.start("draft").edge("draft", "review")
    wf.goto("review", ["draft", END])

    # max_steps bounds any loop; deadline (seconds) bounds the whole run.
    result = await wf.compile().run({}, max_steps=20, deadline=30)
    print(result.state["text"])  # "draft 3"
    # [/show loop]


async def with_agents() -> None:
    # [show agents]
    from criogenai import END, Agent, Workflow

    triage = Agent("openai:gpt-5-mini", instructions="Classify the request in one word.")
    writer = Agent("openai:gpt-5-mini", instructions="Write a short, kind reply.")

    wf = Workflow()
    wf.add_node("triage", triage)  # reads "messages", writes the whole transcript back
    wf.add_node("writer", writer)
    wf.start("triage").edge("triage", "writer").edge("writer", END)

    first = {"role": "user", "content": [{"type": "text", "text": "My parcel is late"}]}
    result = await wf.compile().run({"messages": [first]})
    print(result.state["messages"][-1]["content"][0]["text"])
    # [/show agents]

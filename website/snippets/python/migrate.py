# Docs: from-langgraph. Checked with mypy; run in the docs' checks with FakeModel.
from criogenai import tool


@tool
def get_weather(city: str) -> str:
    """The weather in a city."""
    return f"Sunny in {city}"


async def graph() -> None:
    # [show graph]
    from criogenai import END, Workflow

    wf = Workflow(reducers={"log": "append"})  # like an Annotated reducer on a state key

    @wf.node
    def plan(state: dict) -> dict:
        return {"steps": ["search", "write"], "log": ["planned"]}

    @wf.node
    def write(state: dict) -> dict:
        return {"answer": f"Done in {len(state['steps'])} steps", "log": ["written"]}

    wf.start("plan")  # START -> plan
    wf.route("plan", lambda s: "write" if s["steps"] else "stop", {"write": "write", "stop": END})
    wf.edge("write", END)
    app = wf.compile()  # validates the graph; no state class needed

    result = await app.run({"question": "..."})
    print(result.state["answer"], result.state["log"])
    # [/show graph]


async def agent() -> None:
    # [show agent]
    from criogenai import Agent, Storage

    agent = Agent(
        "openai:gpt-5-mini",
        tools=[get_weather],
        instructions="Answer briefly.",
        storage=Storage("sqlite://chat.db"),  # the checkpointer
    )
    # thread_id in the config becomes a thread handle
    reply = await agent.thread("user-1").send("Weather in Paris?")
    print(reply.text)
    # [/show agent]


async def interrupt() -> None:
    # [show interrupt]
    from criogenai import Agent, Storage

    agent = Agent(
        "openai:gpt-5-mini",
        tools=[get_weather],
        approve=["get_weather"],  # pause before this tool runs: no interrupt() call to write
        storage=Storage("sqlite://chat.db"),
    )
    chat = agent.thread("user-1")
    reply = await chat.send("Weather in Paris?")
    if reply.needs_approval:  # like an __interrupt__ in the result
        reply = await chat.approve()  # like resuming with a Command
    # [/show interrupt]


async def store() -> None:
    # [show store]
    from criogenai import Agent, Memory, Storage

    storage = Storage("sqlite://chat.db")
    await storage.remember("users/1", "language", "French")  # namespace as a path, not a tuple
    facts = await storage.search("users/1", "language")

    # Or let the agent recall and save facts itself, per user:
    agent = Agent("openai:gpt-5-mini", storage=storage, memory=Memory.by_state("user_id", "users"))
    await agent.thread("user-1").send("Bonjour", user_id="1")
    # [/show store]

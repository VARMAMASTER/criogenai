async def main() -> None:
    # [show]
    from criogen import Agent, Memory, Storage

    storage = Storage("sqlite://agent.db")
    agent = Agent(
        "openai:gpt-5-mini",
        storage=storage,
        # One memory per user: users/<user_id>. Facts are recalled before
        # each model call; the model gets remember, forget and search tools.
        memory=Memory.by_state("user_id", "users"),
    )

    await agent.thread("monday").send("I'm vegetarian", user_id="42")
    # A later conversation recalls it.
    await agent.thread("friday").send("Dinner ideas?", user_id="42")

    # Your app can read and write memory too.
    await storage.remember("users/42", "allergy", "peanuts")
    facts = await storage.search("users/42", "allergy")
    await storage.forget_all("users/42")  # everything about one user
    # [/show]


def semantic() -> None:
    # [show semantic]
    from criogen import Agent, Embedder, Memory

    memory = Memory.by_state("patient", "patients").semantic(Embedder.openai("text-embedding-3-small"))
    # Or keep everything on your own servers: Embedder.ollama("nomic-embed-text")
    agent = Agent("openai:gpt-5-mini", memory=memory)
    # [/show semantic]

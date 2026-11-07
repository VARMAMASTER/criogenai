from criogen import Storage


async def main(storage: Storage) -> None:
    # [show]
    from criogen import Agent, Collection, Embedder

    handbook = await Collection.open("handbook", embedder=Embedder.openai(), storage=storage)
    await handbook.add(
        [
            {"id": "leave", "text": "Annual leave is 25 days a year.", "metadata": {"section": "hr"}},
            {"id": "expenses", "text": "Expenses are paid within 30 days.", "metadata": {"section": "finance"}},
        ]
    )

    hits = await handbook.search("How many vacation days?", k=3)
    hr_only = await handbook.search("deadlines", k=3, where={"section": "hr"})

    agent = Agent(
        "openai:gpt-5-mini",
        instructions="Answer from the handbook; say which section you used.",
        tools=[handbook.as_tool("search_handbook", "Search the staff handbook.")],
    )
    print((await agent.run("How much leave do I get?")).text)
    # [/show]

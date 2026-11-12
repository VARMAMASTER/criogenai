"""Collections (design 0025): documents searched by meaning, through Python."""

import asyncio
import os
import tempfile

import pytest

from criogenai import Agent, Collection, CriogenError, Document, Embedder, FakeModel, FakeReply, Storage

DOCS = [
    {"id": "leave", "text": "Annual leave is 25 days a year.", "metadata": {"section": "hr"}},
    {"id": "expenses", "text": "Expenses are paid within 30 days of the claim.", "metadata": {"section": "finance"}},
    Document("parking", "Parking permits are issued by the front desk.", {"section": "facilities"}),
]


def embedder():
    return Embedder.fake(synonyms={"vacation": "leave"})


def test_documents_are_found_by_meaning_filtered_and_removed():
    async def main():
        docs = await Collection.open("handbook", embedder=embedder())
        assert await docs.add(DOCS) == 3
        hits = await docs.search("how many vacation days", k=2)
        assert hits[0].id == "leave" and hits[0].metadata == {"section": "hr"}
        assert hits[0].score > hits[1].score
        only = await docs.search("days", k=3, where={"section": "finance"})
        assert [h.id for h in only] == ["expenses"]
        assert await docs.remove("leave") and not await docs.remove("leave")
        assert await docs.count() == 2
        hnsw = await Collection.open("h", embedder=embedder(), index="hnsw")
        await hnsw.add(DOCS)
        assert (await hnsw.search("vacation", k=1))[0].id == "leave"

    asyncio.run(main())


def test_a_collection_survives_reopening_a_database():
    path = os.path.join(tempfile.mkdtemp(), "docs.db")

    async def main():
        storage = Storage(f"sqlite://{path}")
        docs = await Collection.open("handbook", embedder=embedder(), storage=storage)
        await docs.add(DOCS)
        await storage.close()
        storage = Storage(f"sqlite://{path}")
        again = await Collection.open("handbook", embedder=embedder(), storage=storage)
        assert await again.count() == 3
        assert (await again.search("vacation", k=1))[0].id == "leave"
        await storage.close()

    asyncio.run(main())


def test_an_agent_searches_through_the_tool_and_errors_have_codes():
    async def main():
        docs = await Collection.open("handbook", embedder=embedder())
        await docs.add(DOCS)
        agent = Agent(
            FakeModel([FakeReply.tool_call("search_handbook", {"query": "vacation days", "k": 1}), "25 days."]),
            tools=[docs.as_tool("search_handbook", "Search the staff handbook.")],
        )
        result = await agent.run("How much leave?")
        tool = [m for m in result.messages if m["role"] == "tool"][0]
        hits = tool["content"][0]["value"]
        assert hits[0]["id"] == "leave"

        failing = await Collection.open("f", embedder=Embedder.fake(failing=True))
        with pytest.raises(CriogenError) as error:
            await failing.add([{"id": "a", "text": "x"}])
        assert error.value.code == "model_failed"
        with pytest.raises(CriogenError) as error:
            await docs.add([{"id": "", "text": "x"}])
        assert error.value.code == "invalid_value"

    asyncio.run(main())

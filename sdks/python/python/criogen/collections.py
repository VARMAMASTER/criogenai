"""Collections: documents searched by meaning (design 0025).

    from criogen import Agent, Collection, Embedder

    docs = await Collection.open("handbook", embedder=Embedder.openai(), storage=storage)
    await docs.add([{"id": "leave", "text": "Annual leave is 25 days.", "metadata": {"section": "hr"}}])
    hits = await docs.search("How many vacation days?", k=5)
    agent = Agent("openai:gpt-5-mini", tools=[docs.as_tool("search_handbook", "Search the staff handbook.")])

Documents are kept in the storage (encrypted when it is) and indexed in
memory when the collection opens. The index is exact by default; pass
``index="hnsw"`` for very large collections (faster queries, approximate,
slower to open).
"""

import asyncio
import json
from dataclasses import dataclass, field
from typing import Any, Dict, Iterable, List, Mapping, Optional, Union

from . import _native

__all__ = ["Collection", "Document", "Hit", "CollectionTool"]


@dataclass
class Document:
    """A text to search, with an id and metadata."""

    id: str
    text: str
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class Hit:
    """A search result: the document and its cosine similarity to the query."""

    id: str
    text: str
    metadata: Dict[str, Any]
    score: float


class CollectionTool:
    """A collection as an agent tool (``collection.as_tool(...)``)."""

    def __init__(self, collection: "Collection", name: str, description: str):
        self.collection = collection
        self.name = name
        self.description = description

    def __repr__(self) -> str:
        return f"CollectionTool({self.name!r})"


class Collection:
    """Documents searched by meaning. Open one with :meth:`open`."""

    def __init__(self, name: str):
        self.name = name
        self._native = _native.NativeCollection()

    @staticmethod
    async def open(
        name: str,
        *,
        embedder: Any,
        storage: Any = None,
        index: str = "flat",
        ef_search: Optional[int] = None,
    ) -> "Collection":
        """Opens collection ``name`` in ``storage`` (a Storage or a URL; in
        memory without one), building its index from the stored vectors.
        ``embedder``: an :class:`criogen.Embedder`. ``index``: ``"flat"``
        (exact, the default) or ``"hnsw"``."""
        from .storage import Storage

        if isinstance(storage, str):
            storage = Storage(storage)
        if index not in ("flat", "hnsw"):
            raise ValueError('index is "flat" or "hnsw"')
        collection = Collection(name)
        spec = {"name": name, "embedder": embedder._spec, "index": index, "ef_search": ef_search}
        await collection._native.open(
            asyncio.get_running_loop(), storage._native if storage is not None else None, json.dumps(spec)
        )
        return collection

    async def add(self, documents: Iterable[Union[Document, Mapping[str, Any]]]) -> int:
        """Embeds, stores and indexes documents (an existing id is replaced).
        Returns how many the collection holds."""
        docs = [
            {"id": d.id, "text": d.text, "metadata": d.metadata} if isinstance(d, Document) else dict(d)
            for d in documents
        ]
        return int(await self._native.add(asyncio.get_running_loop(), json.dumps(docs)))

    async def search(self, query: str, k: int = 5, where: Optional[Mapping[str, Any]] = None) -> List[Hit]:
        """The ``k`` documents closest in meaning to ``query``, best first;
        ``where`` keeps only those whose metadata has these values."""
        raw = await self._native.search(asyncio.get_running_loop(), query, k, json.dumps(dict(where or {})))
        return [Hit(h["id"], h["text"], h.get("metadata") or {}, h["score"]) for h in raw]

    async def remove(self, id: str) -> bool:
        """Removes a document; ``True`` if it was there."""
        return bool(await self._native.op(asyncio.get_running_loop(), "remove", id))

    async def reload(self) -> int:
        """Rebuilds the index from storage (another process added documents)."""
        return int(await self._native.op(asyncio.get_running_loop(), "reload", None))

    async def count(self) -> int:
        """Documents in the index."""
        return int(await self._native.op(asyncio.get_running_loop(), "count", None))

    def as_tool(self, name: str, description: str) -> CollectionTool:
        """This collection as an agent tool: the model gives a ``query`` (and
        optionally ``k``, up to 20) and gets the hits."""
        return CollectionTool(self, name, description)

    def __repr__(self) -> str:
        return f"Collection({self.name!r})"

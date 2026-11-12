"""Collections: documents searched by meaning, for retrieval and as agent tools.

    from criogenai import Agent, Collection, Embedder

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
    """A text to search, with an id and metadata.

    ``Collection.add`` also takes plain dicts with the same keys.

    Example:
        from criogenai import Document

        doc = Document("leave", "Annual leave is 25 days a year.", {"section": "hr"})
    """

    id: str
    """Unique in its collection (1 to 256 bytes); adding the same id again replaces the document."""
    text: str
    """What is embedded, searched and returned (at most 100,000 characters: split longer texts)."""
    metadata: Dict[str, Any] = field(default_factory=dict)
    """JSON fields kept with the document (at most 8 KB), returned with hits and usable in ``search(where=...)``."""


@dataclass
class Hit:
    """A search result: the document and its cosine similarity to the query."""

    id: str
    """The document's id."""
    text: str
    """The document's text."""
    metadata: Dict[str, Any]
    """The document's metadata."""
    score: float
    """Cosine similarity between the query and the document, from -1 to 1 (higher is closer)."""


class CollectionTool:
    """A collection as an agent tool; made by :meth:`Collection.as_tool`.

    Pass it in ``Agent(tools=[...])``. The model gives a ``query`` (and
    optionally ``k``, 1 to 20, default 5) and gets the hits, each text cut to
    2,000 characters.

    Args:
        collection: The collection to search.
        name: The tool's name, as the model sees it.
        description: What the model reads to decide when to search.
    """

    def __init__(self, collection: "Collection", name: str, description: str):
        self.collection = collection
        self.name = name
        self.description = description

    def __repr__(self) -> str:
        return f"CollectionTool({self.name!r})"


class Collection:
    """Documents searched by meaning. Open one with :meth:`open`.

    Each document's text is turned into a vector by an embedding model and
    stored in a ``Storage`` with the text and metadata. A search embeds the
    query and returns the closest documents. Use it to give an agent a
    knowledge base (:meth:`as_tool`) or to look things up yourself
    (:meth:`search`).

    Args:
        name: The collection's name. Internal: use :meth:`open`, which
            also builds the index.

    Example:
        from criogenai import Collection, Embedder

        docs = await Collection.open("handbook", embedder=Embedder.fake())
        await docs.add([
            {"id": "leave", "text": "Annual leave is 25 days a year.", "metadata": {"section": "hr"}},
            {"id": "parking", "text": "Staff parking is behind building B."},
        ])
        hits = await docs.search("How many days of annual leave?", k=1)
        hits[0].id   # "leave"
    """

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
        """Opens a collection and builds its index from the stored vectors.

        A collection that does not exist yet starts empty. Several
        collections can share one storage; each lives in its own namespace.

        Args:
            name: The collection's name.
            embedder: An :class:`criogenai.Embedder` that turns texts into
                vectors, e.g. ``Embedder.openai()``; use the same one every
                time you open the collection.
            storage: A :class:`criogenai.Storage` or a storage URL (e.g.
                ``"sqlite://docs.db"``). Default: process memory.
            index: ``"flat"`` (the default) compares the query with every
                vector: exact, and opens at once. ``"hnsw"`` is approximate:
                faster queries on large collections, slower to open (it is
                rebuilt each time).
            ef_search: ``"hnsw"`` only: candidates kept while searching
                (at least ``k``). Higher finds more of the true nearest
                documents and is slower. Default 64.

        Returns:
            The open collection.

        Raises:
            ValueError: ``index`` is not ``"flat"`` or ``"hnsw"``.
        """
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
        """Embeds, stores and indexes documents; an existing id is replaced.

        Texts are embedded 64 per call to the embedding model.

        Args:
            documents: :class:`Document` objects or dicts with ``id``,
                ``text`` and optionally ``metadata``.

        Returns:
            How many documents the collection holds now.

        Raises:
            ValidationError: An id is empty or longer than 256 bytes, a text
                is longer than 100,000 characters, or metadata is larger
                than 8 KB (``invalid_value``).
            ModelError: The embedding model failed (``model_failed``).
        """
        docs = [
            {"id": d.id, "text": d.text, "metadata": d.metadata} if isinstance(d, Document) else dict(d)
            for d in documents
        ]
        return int(await self._native.add(asyncio.get_running_loop(), json.dumps(docs)))

    async def search(self, query: str, k: int = 5, where: Optional[Mapping[str, Any]] = None) -> List[Hit]:
        """The ``k`` documents closest in meaning to ``query``, best first.

        Args:
            query: The text to look for.
            k: The most hits to return. Default 5; 0 returns none.
            where: Keeps only documents whose metadata has every one of
                these fields with exactly that value, e.g.
                ``{"section": "hr"}``. The filter is applied to the ``4 * k``
                closest documents, so fewer than ``k`` hits may come back.

        Returns:
            The hits, closest first.

        Example:
            hits = await docs.search("parking", k=3, where={"section": "facilities"})
            for hit in hits:
                print(round(hit.score, 2), hit.text)
        """
        raw = await self._native.search(asyncio.get_running_loop(), query, k, json.dumps(dict(where or {})))
        return [Hit(h["id"], h["text"], h.get("metadata") or {}, h["score"]) for h in raw]

    async def remove(self, id: str) -> bool:
        """Removes a document from the storage and the index.

        Args:
            id: The document's id.

        Returns:
            ``True`` if it was there.
        """
        return bool(await self._native.op(asyncio.get_running_loop(), "remove", id))

    async def reload(self) -> int:
        """Rebuilds the index from storage, e.g. after another process added documents.

        Returns:
            How many documents the index holds.
        """
        return int(await self._native.op(asyncio.get_running_loop(), "reload", None))

    async def count(self) -> int:
        """How many documents the index holds."""
        return int(await self._native.op(asyncio.get_running_loop(), "count", None))

    def as_tool(self, name: str, description: str) -> CollectionTool:
        """This collection as a read-only agent tool.

        The model gives a ``query`` (and optionally ``k``, 1 to 20, default
        5) and gets the hits as JSON, each text cut to 2,000 characters.

        Args:
            name: The tool's name, as the model sees it, e.g.
                ``"search_handbook"``.
            description: What the model reads to decide when to search, e.g.
                ``"Search the staff handbook."``.

        Returns:
            A tool for ``Agent(tools=[...])``.

        Example:
            from criogenai import Agent

            agent = Agent(
                "openai:gpt-5-mini",
                tools=[docs.as_tool("search_handbook", "Search the staff handbook.")],
            )
        """
        return CollectionTool(self, name, description)

    def __repr__(self) -> str:
        return f"Collection({self.name!r})"

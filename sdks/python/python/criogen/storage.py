"""Storage and memory: where threads, long-term memory and the audit trail
live.

The Rust engine does the work; these classes hold the settings. Pass one
:class:`Storage` to every agent and workflow that should share threads and
memory, and give an agent a :class:`Memory` to recall facts across
conversations.
"""

import asyncio
import json
from typing import Any, Dict, List, Optional

from . import _native


class Storage:
    """Keeps threads (every checkpoint), long-term memory and the audit trail.

    Pick the backend with ``url``: process memory (tests and scripts, gone
    when the process ends), a SQLite file (one process) or Postgres
    (several processes or machines; one run per thread across all of
    them). Tables are created on first use. Agents and workflows built
    with the same ``Storage`` share its threads and memory.

    The database is opened when the object is created, so a bad URL or key
    fails here. Call :meth:`close` at shutdown.

    Args:
        url: Where the data lives. ``None`` (the default) keeps it in
            process memory; ``"sqlite://path/to/file.db"`` uses a SQLite
            file; ``"postgres://user:password@host/database"`` uses
            Postgres.
        encryption_keys: Encrypts content at rest (AES-256-GCM) in a
            database given by ``url``. Text such as ``"k2:<64 hex
            digits>,k1:<64 hex digits>"``: each entry is a key id and a
            32-byte key in hex. The first key encrypts new writes; the
            others only decrypt older rows. A database written with keys
            refuses to open without them. Keep the keys outside the
            database (an environment variable or a secret store).
        audit: ``True`` records who did what (model calls, tool calls,
            runs, approvals, rewinds, deletions, memory writes) in a
            hash-chained audit trail, without message content. Read it with
            :meth:`audit_records` and check it with :meth:`verify_audit`.
            Default ``False``.
        retention: How long data is kept, applied each time :meth:`prune`
            runs (nothing is deleted before that). A dict with any of
            ``"thread_ttl_secs"`` (delete threads whose last change is at
            least this many seconds old), ``"keep_checkpoints"`` (keep each
            thread's head and its ``n - 1`` ancestors; older checkpoints and
            abandoned branches go) and ``"drop_branches"`` (``True``: delete
            the branches ``rewind`` leaves behind). Default: keep everything.
        on_busy: What a second call on a thread that is already running
            does. ``"wait"`` (the default) waits its turn, up to
            ``busy_timeout``, then fails with ``thread_busy``; ``"reject"``
            fails at once with ``thread_busy``; ``"interrupt"`` cancels the
            running call (it is saved, so ``retry`` can continue it) and goes
            next.
        busy_timeout: How long ``"wait"`` and ``"interrupt"`` wait for the
            thread, in seconds. Default 60.

    Raises:
        ValidationError: An unknown ``on_busy`` value or unreadable keys
            (``invalid_value``).
        StorageError: The database cannot be opened, or it is encrypted
            and the keys do not match.

    Example:
        from criogen import Agent, FakeModel, Storage

        storage = Storage("sqlite://agent.db", audit=True)
        agent = Agent(FakeModel(["Hello!"]), storage=storage)
        reply = await agent.thread("conversation-1").send("Hi")
        records = await storage.audit_records(thread_id="conversation-1")
        await storage.close()
    """

    def __init__(
        self,
        url: Optional[str] = None,
        *,
        encryption_keys: Optional[str] = None,
        audit: bool = False,
        retention: Optional[Dict[str, Any]] = None,
        on_busy: Optional[str] = None,
        busy_timeout: Optional[float] = None,
    ):
        self.url = url
        spec: Dict[str, Any] = {"url": url, "audit": audit}
        if encryption_keys:
            spec["encryption_keys"] = encryption_keys
        if retention:
            spec["retention"] = retention
        if on_busy:
            spec["on_busy"] = on_busy
        if busy_timeout is not None:
            spec["busy_timeout_secs"] = busy_timeout
        self._native = _native.NativeStorage(json.dumps(spec))

    async def _op(self, op: str, **payload: Any) -> Any:
        return await self._native.op(asyncio.get_running_loop(), op, json.dumps(payload))

    async def close(self) -> None:
        """Closes the database's connections (SQLite releases the file).

        Call it after the last run. Later calls on this storage fail with
        ``StorageError`` (``storage_unavailable``). In-memory storage has
        nothing to close.
        """
        await self._native.close(asyncio.get_running_loop())

    async def remember(
        self, namespace: str, key: str, value: Any, ttl: Optional[float] = None
    ) -> None:
        """Saves a fact in long-term memory, replacing any value under the same key.

        Facts saved here are found by keyword search, also by an agent whose
        memory is semantic (no vector is stored for them).

        Args:
            namespace: A path such as ``"users/42"``: segments separated by
                ``/``, each non-empty and without control characters.
            key: The fact's key within the namespace.
            value: Any JSON value.
            ttl: Seconds until the fact expires. Default: kept until deleted.

        Raises:
            ValidationError: An invalid namespace (``invalid_value``).

        Example:
            await storage.remember("users/42", "language", "French")
            item = await storage.recall("users/42", "language")
            item["value"]   # "French"
        """
        await self._op("memory_put", namespace=namespace, key=key, value=value, ttl_secs=ttl)

    async def recall(self, namespace: str, key: str) -> Optional[Dict[str, Any]]:
        """Reads one fact from long-term memory.

        Args:
            namespace: The namespace path, e.g. ``"users/42"``.
            key: The fact's key.

        Returns:
            The item as a dict (``namespace`` as a list of segments, ``key``,
            ``value``, ``created_at_ms``, ``updated_at_ms`` and, with a time
            to live, ``expires_at_ms``; times in Unix milliseconds), or
            ``None`` if there is no such fact or it has expired.
        """
        return await self._op("memory_get", namespace=namespace, key=key)

    async def search(
        self, namespace: str, text: Optional[str] = None, limit: int = 10
    ) -> List[Dict[str, Any]]:
        """Finds facts in ``namespace`` and every namespace below it, by keywords.

        Args:
            namespace: Where to look: ``"users"`` covers ``"users/42"`` and
                ``"users/43"``; ``""`` searches everything.
            text: Words to match; results come best first, each with a
                ``score`` from 0 to 1. ``None`` returns every item, newest
                first.
            limit: The most items to return. Default 10.

        Returns:
            Items shaped like :meth:`recall`'s.
        """
        return await self._op("memory_search", namespace=namespace, text=text, limit=limit)

    async def forget_all(self, namespace: str) -> int:
        """Deletes every fact in ``namespace`` and below it, e.g. everything about one user.

        Args:
            namespace: The namespace path, e.g. ``"users/42"``.

        Returns:
            How many facts were deleted.
        """
        return await self._op("memory_delete_namespace", namespace=namespace)

    async def prune(self) -> Dict[str, int]:
        """Applies the ``retention`` rules once, and removes expired memory facts.

        Run it on a schedule (a cron job, a background task).

        Returns:
            Counts: ``threads_deleted`` (idle threads deleted),
            ``threads_busy`` (expired threads skipped because a run held
            them), ``checkpoints_deleted`` (from threads that were kept)
            and ``memory_items_deleted`` (expired facts removed).
        """
        return await self._op("prune")

    async def audit_records(
        self,
        thread_id: Optional[str] = None,
        action: Optional[str] = None,
        after_seq: Optional[int] = None,
        limit: int = 100,
    ) -> List[Dict[str, Any]]:
        """Reads the audit trail, oldest first. Needs ``Storage(audit=True)``.

        Each record has ``seq`` (1, 2, 3... without gaps), ``at_ms`` (Unix
        milliseconds), ``action`` (e.g. ``"model.called"``,
        ``"run.finished"``), and when known ``actor``, ``thread_id``,
        ``run_id``, ``subject``, ``outcome`` and ``details``, plus the
        ``prev_hash`` and ``hash`` that chain the records. Records hold ids,
        names, codes and counts, never message content.

        Args:
            thread_id: Only this thread's records.
            action: Only records of this action.
            after_seq: Only records after this ``seq``; pass the last
                ``seq`` you read to get the next page.
            limit: The most records to return. Default 100.

        Raises:
            ValidationError: The audit trail is off (``invalid_value``).

        Example:
            records = await storage.audit_records(thread_id="conversation-1")
            for r in records:
                print(r["seq"], r["action"], r.get("outcome"))
        """
        return await self._op(
            "audit_query", thread_id=thread_id, action=action, after_seq=after_seq, limit=limit
        )

    async def verify_audit(self) -> Dict[str, Any]:
        """Checks the audit trail's hash chain. Needs ``Storage(audit=True)``.

        Returns:
            ``{"ok", "records", "first_broken", "head"}``: ``ok`` is
            ``True`` when every hash and link is right and no ``seq`` is
            missing; ``records`` is how many were checked; ``first_broken``
            is the first wrong record's ``seq`` (else ``None``); ``head`` is
            the last record's ``[seq, hash]``. Copy ``head`` somewhere else
            now and then: a chain rewritten from the start would still
            verify.

        Raises:
            ValidationError: The audit trail is off (``invalid_value``).
        """
        return await self._op("audit_verify")

    def __repr__(self) -> str:
        return f"Storage({self.url!r})"


class Memory:
    """Long-term memory for an agent: facts recalled before each model call, and tools to save them.

    Before each model call, the facts that best match the conversation
    (then the newest) are added to the request, marked as information, not
    instructions; they are not stored in the transcript. The model also
    gets the ``remember``, ``forget`` and ``search_memory`` tools. Facts
    live in the agent's ``storage``.

    Build one with :meth:`by_state` (one namespace per user or customer)
    or :meth:`shared` (one for everybody), and pass it as
    ``Agent(memory=...)``.

    Args:
        spec: The settings. Internal: use :meth:`by_state` or
            :meth:`shared` instead.

    Example:
        from criogen import Agent, FakeModel, Memory, Storage

        storage = Storage()
        await storage.remember("users/42", "diet", "vegetarian")
        agent = Agent(
            FakeModel(["How about a mushroom risotto?"]),
            storage=storage,
            memory=Memory.by_state("user_id", "users"),
        )
        result = await agent.run("Suggest a dinner.", user_id="42")
    """

    def __init__(self, spec: Dict[str, Any]):
        self._spec = spec

    @staticmethod
    def by_state(
        key: str,
        prefix: str,
        recall: int = 5,
        tools: bool = True,
        ttl: Optional[float] = None,
    ) -> "Memory":
        """One namespace per value of a run's state key, e.g. per user: ``prefix/<value>``.

        Pass the value with each call, e.g. ``agent.run(text, user_id="42")``
        or ``thread.send(text, user_id="42")``. A run without the key fails
        (``invalid_state``) instead of seeing another user's facts.

        Args:
            key: The state key that names the namespace (its value is a
                string or a number).
            prefix: The namespace path the value goes under: ``"users"``
                gives ``users/42``.
            recall: How many facts to add before each model call. Default 5;
                0 turns recall off (the tools still work).
            tools: Whether the model gets the ``remember``, ``forget`` and
                ``search_memory`` tools. Default ``True``.
            ttl: Seconds that facts saved by the ``remember`` tool are
                kept. Default: until deleted.

        Returns:
            The memory setting for ``Agent(memory=...)``.
        """
        spec: Dict[str, Any] = {"by_state": key, "prefix": prefix, "recall": recall, "tools": tools}
        if ttl is not None:
            spec["ttl_secs"] = ttl
        return Memory(spec)

    @staticmethod
    def shared(namespace: str, recall: int = 5, tools: bool = True) -> "Memory":
        """One namespace for every run: knowledge shared by all users.

        Args:
            namespace: The namespace path, e.g. ``"team/handbook"``.
            recall: How many facts to add before each model call. Default 5;
                0 turns recall off (the tools still work).
            tools: Whether the model gets the ``remember``, ``forget`` and
                ``search_memory`` tools. Default ``True``.

        Returns:
            The memory setting for ``Agent(memory=...)``.
        """
        return Memory({"shared": namespace, "recall": recall, "tools": tools})

    def semantic(self, embedder: Any) -> "Memory":
        """This memory, searched by meaning with an embedding model.

        Recall and ``search_memory`` then find facts that say the same thing
        in other words. Facts the agent saves with ``remember`` get a vector
        (stored, and encrypted, with the fact); facts saved with
        :meth:`Storage.remember` are found by keywords only. If the
        embedder fails, search falls back to keywords.

        Args:
            embedder: An :class:`criogen.Embedder`, e.g.
                ``Embedder.openai()`` or ``Embedder.ollama()``.

        Returns:
            A new ``Memory``; this one is unchanged.

        Example:
            from criogen import Embedder, Memory

            memory = Memory.by_state("user_id", "users").semantic(Embedder.ollama())
        """
        return Memory({**self._spec, "embedder": embedder._spec})

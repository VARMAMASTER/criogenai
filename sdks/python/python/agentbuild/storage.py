"""Storage and memory (design 0011 §2): where threads, long-term memory and
the audit trail live. The Rust engine does the work; this holds the
settings."""

import asyncio
import json
from typing import Any, Dict, List, Optional

from . import _native


class Storage:
    """Threads, memory and the audit trail. ``url``: ``None`` (process
    memory), ``"sqlite:///path.db"`` or ``"postgres://…"``.

    ``encryption_keys``: ``"k2:<64 hex>,k1:<64 hex>"`` (the first is
    active) encrypts content at rest. ``audit=True`` records who did what.
    ``retention``: ``{"thread_ttl_secs": …, "keep_checkpoints": …}``,
    applied by :meth:`prune`. Agents built with the same ``Storage``
    share it.
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
        Later calls fail with ``StorageError`` (``storage_unavailable``)."""
        await self._native.close(asyncio.get_running_loop())

    async def remember(
        self, namespace: str, key: str, value: Any, ttl: Optional[float] = None
    ) -> None:
        """Saves a fact under ``namespace`` (e.g. ``"users/42"``)."""
        await self._op("memory_put", namespace=namespace, key=key, value=value, ttl_secs=ttl)

    async def recall(self, namespace: str, key: str) -> Optional[Dict[str, Any]]:
        """The item, or ``None``."""
        return await self._op("memory_get", namespace=namespace, key=key)

    async def search(
        self, namespace: str, text: Optional[str] = None, limit: int = 10
    ) -> List[Dict[str, Any]]:
        """Items in ``namespace`` and below matching ``text`` (keywords)."""
        return await self._op("memory_search", namespace=namespace, text=text, limit=limit)

    async def forget_all(self, namespace: str) -> int:
        """Deletes everything in ``namespace`` and below (e.g. one patient)."""
        return await self._op("memory_delete_namespace", namespace=namespace)

    async def prune(self) -> Dict[str, int]:
        """Applies the retention rules once."""
        return await self._op("prune")

    async def audit_records(
        self,
        thread_id: Optional[str] = None,
        action: Optional[str] = None,
        after_seq: Optional[int] = None,
        limit: int = 100,
    ) -> List[Dict[str, Any]]:
        """Audit records, oldest first (needs ``audit=True``)."""
        return await self._op(
            "audit_query", thread_id=thread_id, action=action, after_seq=after_seq, limit=limit
        )

    async def verify_audit(self) -> Dict[str, Any]:
        """Checks the audit trail's hash chain."""
        return await self._op("audit_verify")

    def __repr__(self) -> str:
        return f"Storage({self.url!r})"


class Memory:
    """Long-term memory for an agent: recalled before each model call, and
    ``remember`` / ``forget`` / ``search_memory`` tools for the model."""

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
        """One namespace per value of the run's ``key`` (e.g. per user):
        ``prefix/<value>``. Pass the value with ``run(..., key=value)``."""
        spec: Dict[str, Any] = {"by_state": key, "prefix": prefix, "recall": recall, "tools": tools}
        if ttl is not None:
            spec["ttl_secs"] = ttl
        return Memory(spec)

    @staticmethod
    def shared(namespace: str, recall: int = 5, tools: bool = True) -> "Memory":
        """One namespace for every run."""
        return Memory({"shared": namespace, "recall": recall, "tools": tools})

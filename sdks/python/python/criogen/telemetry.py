"""Tracing (design 0017 §4): every run of an agent exported as an
OpenTelemetry trace, to Langfuse or any OTLP/HTTP collector. Message
content is left out unless ``capture_content=True``."""

import asyncio
import json
from typing import Any, Dict, Optional

from . import _native


class Telemetry:
    """An exporter shared by any number of agents: ``Agent(...,
    telemetry=t)``. Exports in the background; call ``flush()`` before a
    script ends, and ``shutdown()`` when done."""

    def __init__(self, spec: Dict[str, Any]):
        self._native = _native.NativeTelemetry(json.dumps(spec))

    @staticmethod
    def langfuse(
        public_key: Optional[str] = None,
        secret_key: Optional[str] = None,
        base_url: Optional[str] = None,
        capture_content: bool = False,
        service_name: Optional[str] = None,
    ) -> "Telemetry":
        """Langfuse (cloud or self-hosted). Keys default to
        ``LANGFUSE_PUBLIC_KEY`` / ``LANGFUSE_SECRET_KEY`` and the address to
        ``LANGFUSE_BASE_URL``."""
        return Telemetry(
            {
                "kind": "langfuse",
                "public_key": public_key,
                "secret_key": secret_key,
                "base_url": base_url,
                "capture_content": capture_content,
                "service_name": service_name,
            }
        )

    @staticmethod
    def otlp(
        endpoint: str,
        headers: Optional[Dict[str, str]] = None,
        capture_content: bool = False,
        service_name: Optional[str] = None,
    ) -> "Telemetry":
        """Any OTLP/HTTP (JSON) collector, e.g.
        ``http://localhost:4318/v1/traces``."""
        return Telemetry(
            {
                "kind": "otlp",
                "endpoint": endpoint,
                "headers": headers or {},
                "capture_content": capture_content,
                "service_name": service_name,
            }
        )

    @staticmethod
    def local(capture_content: bool = False, url: str = "http://127.0.0.1:4318") -> "Telemetry":
        """The local trace viewer (``python -m criogen.trace``, design 0023).
        Content (prompts, outputs) only with ``capture_content=True``; it
        stays on this computer either way."""
        return Telemetry.otlp(url.rstrip("/") + "/v1/traces", capture_content=capture_content)

    async def flush(self) -> None:
        """Waits until everything sent so far is exported."""
        await self._native.flush(asyncio.get_running_loop())

    async def shutdown(self) -> None:
        """Exports what is left and stops."""
        await self._native.shutdown(asyncio.get_running_loop())

    def stats(self) -> Dict[str, Any]:
        """``exported``, ``failed``, ``dropped`` and the ``last_error``."""
        stats: Dict[str, Any] = json.loads(self._native.stats())
        return stats

    def __repr__(self) -> str:  # never shows keys
        return "Telemetry()"

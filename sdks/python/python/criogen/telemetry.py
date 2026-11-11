"""Tracing: every run of an agent exported as an OpenTelemetry trace, to Langfuse, any OTLP/HTTP collector, or the local trace viewer.

Each run becomes a trace with its model calls, tool calls and steps.
Message content is left out unless ``capture_content=True``.
"""

import asyncio
import json
from typing import Any, Dict, Optional

from . import _native


class Telemetry:
    """An exporter of run traces, shared by any number of agents: ``Agent(..., telemetry=t)``.

    Create it with :meth:`langfuse`, :meth:`otlp` or :meth:`local`. It
    exports in the background and never blocks a run: when its queue (10,000
    events) is full, events are dropped and counted in :meth:`stats`. Call
    :meth:`flush` before a script ends, and :meth:`shutdown` when done.

    Args:
        spec: The exporter's settings. Internal: use :meth:`langfuse`,
            :meth:`otlp` or :meth:`local` instead.

    Raises:
        ValidationError: Missing keys or an endpoint that cannot be used
            (``invalid_value``).

    Example:
        from criogen import Agent, FakeModel, Telemetry

        telemetry = Telemetry.otlp("http://localhost:4318/v1/traces")
        agent = Agent(FakeModel(["Hi!"]), telemetry=telemetry)
        await agent.run("Hello")
        await telemetry.flush()
        print(telemetry.stats())
    """

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
        """Exports to Langfuse (cloud, any region, or self-hosted).

        Pass both keys, or neither: then the keys come from the
        ``LANGFUSE_PUBLIC_KEY`` and ``LANGFUSE_SECRET_KEY`` environment
        variables. The address is ``base_url`` when given, else
        ``LANGFUSE_BASE_URL`` (or ``LANGFUSE_HOST``), else Langfuse Cloud.

        Args:
            public_key: The project's public key (``pk-lf-...``).
            secret_key: The project's secret key (``sk-lf-...``).
            base_url: Langfuse's address, e.g. ``"https://us.cloud.langfuse.com"``
                or ``"http://localhost:3000"``. Always used when given.
                Default: ``LANGFUSE_BASE_URL``, ``LANGFUSE_HOST``, or
                ``"https://cloud.langfuse.com"``.
            capture_content: ``True`` also exports messages, tool arguments and
                results, error messages, and the run's user id, session id and
                metadata. Default ``False``: names, timings, token usage, status
                and error codes are still exported.
            service_name: The ``service.name`` of the exported traces. Default
                ``"criogen"``.

        Returns:
            The exporter.

        Raises:
            ValidationError: Only one key given, or none given and a variable
                is missing (``invalid_value``).

        Example:
            from criogen import Agent, Telemetry

            telemetry = Telemetry.langfuse()   # keys from LANGFUSE_* variables
            agent = Agent("openai:gpt-5-mini", telemetry=telemetry)
        """
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
        """Exports to any OpenTelemetry collector that accepts OTLP over HTTP with JSON.

        Args:
            endpoint: The full traces URL, e.g.
                ``"http://localhost:4318/v1/traces"``.
            headers: Headers sent with every export, e.g. an API key.
            capture_content: ``True`` also exports messages, tool arguments and
                results, error messages, and the run's user id, session id and
                metadata. Default ``False``: names, timings, token usage, status
                and error codes are still exported.
            service_name: The ``service.name`` of the exported traces. Default
                ``"criogen"``.

        Returns:
            The exporter.
        """
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
        """Exports to the local trace viewer (``python -m criogen.trace``).

        Prompts and outputs are shown only with ``capture_content=True``; they
        stay on this computer either way.

        Args:
            capture_content: ``True`` also exports messages, tool arguments and
                results, error messages, and the run's user id, session id and
                metadata. Default ``False``: names, timings, token usage, status
                and error codes are still exported.
            url: The viewer's address. Default ``"http://127.0.0.1:4318"``;
                ``/v1/traces`` is added.

        Returns:
            The exporter.

        Example:
            from criogen import Agent, Telemetry

            agent = Agent("openai:gpt-5-mini", telemetry=Telemetry.local(capture_content=True))
        """
        return Telemetry.otlp(url.rstrip("/") + "/v1/traces", capture_content=capture_content)

    async def flush(self) -> None:
        """Waits until everything sent so far is exported (or has failed)."""
        await self._native.flush(asyncio.get_running_loop())

    async def shutdown(self) -> None:
        """Exports what is left and stops the exporter."""
        await self._native.shutdown(asyncio.get_running_loop())

    def stats(self) -> Dict[str, Any]:
        """Export counters, to check that traces arrive.

        Returns:
            ``{"exported", "failed", "dropped", "last_error"}``: spans the
            backend accepted, spans lost after failed exports, events dropped
            because the queue was full, and the last export error (or ``None``).
        """
        stats: Dict[str, Any] = json.loads(self._native.stats())
        return stats

    def __repr__(self) -> str:  # never shows keys
        return "Telemetry()"

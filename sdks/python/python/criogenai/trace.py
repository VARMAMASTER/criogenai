"""The local trace viewer: every run on a page, on your own computer.

    python -m criogenai.trace                # http://127.0.0.1:4318

    from criogenai import Agent, Telemetry
    agent = Agent("openai:gpt-5-mini", telemetry=Telemetry.local())

Each run's model calls, tool calls and steps on a time axis, with durations,
tokens, cost (from your ``prices``) and failures. Prompts and outputs appear
only with ``Telemetry.local(capture_content=True)``; nothing leaves the
computer either way. It listens on 127.0.0.1 unless given a ``token``.

Command-line options of ``python -m criogenai.trace``: ``--port`` (default
4318), ``--host`` (default 127.0.0.1), ``--db FILE``, ``--memory`` (keep
runs in memory only), ``--keep-runs N``, ``--keep-days N``, ``--prices
FILE`` (a JSON file shaped like :func:`serve`'s ``prices``) and
``--token-env NAME`` (the environment variable holding the token, needed
off 127.0.0.1).
"""

import argparse
import asyncio
import json
import os
import sys
from typing import Any, Dict, Mapping, Optional

from . import _native

__all__ = ["TraceViewer", "serve", "main"]


class TraceViewer:
    """A trace viewer serving in the background; made by :func:`serve`.

    Stop it with :meth:`stop`, or use it with ``async with``.

    Args:
        native: The running server. Internal: use :func:`serve`.
    """

    def __init__(self, native: Any):
        self._native = native

    @property
    def url(self) -> str:
        """The page's address, e.g. ``http://127.0.0.1:4318``."""
        return str(self._native.url)

    async def stop(self) -> None:
        """Stops serving."""
        await self._native.stop(asyncio.get_running_loop())

    async def __aenter__(self) -> "TraceViewer":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.stop()

    def __repr__(self) -> str:
        return f"TraceViewer({self.url!r})"


def serve(
    port: int = 4318,
    *,
    host: str = "127.0.0.1",
    db: Optional[str] = None,
    keep_runs: Optional[int] = None,
    keep_days: Optional[int] = None,
    prices: Optional[Mapping[str, Mapping[str, float]]] = None,
    token: Optional[str] = None,
) -> TraceViewer:
    """Starts the trace viewer in the background and returns once it listens.

    It receives traces at ``/v1/traces`` (OTLP/HTTP JSON, what
    :meth:`Telemetry.local <criogenai.Telemetry.local>` sends) and shows the
    runs at its address.

    Args:
        port: The port to listen on. Default 4318; 0 picks a free one (read
            it from ``viewer.url``).
        host: The address to listen on. Default ``"127.0.0.1"`` (this computer
            only); any other address needs ``token``.
        db: Where runs are kept: a SQLite file, or ``":memory:"`` to keep them
            in memory only. Default ``~/.criogen/traces.db``.
        keep_runs: The newest runs kept. Default 10,000.
        keep_days: Days a run is kept. Default 14.
        prices: USD per million tokens, for the cost column, by
            ``"provider:model"`` or model name, e.g. ``{"openai:gpt-5-mini":
            {"input": 0.25, "output": 2.0}}``. Without prices, cost is not
            shown.
        token: Required as ``Authorization: Bearer <token>`` by the receiver
            and the API; needed to listen on an address other than this
            computer's.

    Returns:
        The running :class:`TraceViewer`.

    Raises:
        ValidationError: Another address without a token, a port in use, or
            unreadable settings (``invalid_value``).

    Example:
        from criogenai import Agent, FakeModel, Telemetry, trace

        async with trace.serve(0, db=":memory:") as viewer:
            telemetry = Telemetry.local(url=viewer.url)
            agent = Agent(FakeModel(["Hi!"]), telemetry=telemetry)
            await agent.run("Hello")
            await telemetry.flush()
            print("open", viewer.url)
    """
    spec: Dict[str, Any] = {
        "listen": f"[{host}]:{port}" if ":" in host else f"{host}:{port}",
        "db": "memory" if db == ":memory:" else (db or "default"),
        "keep_runs": keep_runs,
        "keep_days": keep_days,
        "prices": dict(prices) if prices else None,
        "token": token,
    }
    return TraceViewer(_native.NativeTraceViewer(json.dumps({k: v for k, v in spec.items() if v is not None})))


def main(argv: Optional[list] = None) -> int:
    """The command line, ``python -m criogenai.trace``: serves until Ctrl+C.

    Args:
        argv: The arguments (default ``sys.argv[1:]``); see the module
            description for the options.

    Returns:
        The exit code: 0, or 2 when ``--token-env`` names a variable that is
        not set.
    """
    parser = argparse.ArgumentParser(prog="python -m criogenai.trace", description="CriogenAI's local trace viewer.")
    parser.add_argument("--port", type=int, default=4318)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--db", help="SQLite file (default ~/.criogen/traces.db)")
    parser.add_argument("--memory", action="store_true", help="keep runs in memory only")
    parser.add_argument("--keep-runs", type=int)
    parser.add_argument("--keep-days", type=int)
    parser.add_argument("--prices", help="JSON file: USD per million tokens by model")
    parser.add_argument("--token-env", help="environment variable holding the token (needed off 127.0.0.1)")
    args = parser.parse_args(argv)
    prices = None
    if args.prices:
        with open(args.prices, encoding="utf-8") as f:
            prices = json.load(f)
    token = None
    if args.token_env:
        token = os.environ.get(args.token_env)
        if not token:
            print(f"criogenai.trace: the environment variable {args.token_env} is not set", file=sys.stderr)
            return 2

    async def run() -> None:
        viewer = serve(
            args.port,
            host=args.host,
            db=":memory:" if args.memory else args.db,
            keep_runs=args.keep_runs,
            keep_days=args.keep_days,
            prices=prices,
            token=token,
        )
        print(f"CriogenAI traces: open {viewer.url}  (send runs with Telemetry.local(); Ctrl+C to stop)")
        try:
            await asyncio.Event().wait()
        finally:
            await viewer.stop()

    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())

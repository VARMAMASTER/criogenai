"""The local trace viewer (design 0023): every run on a page, on your own
computer.

    python -m criogen.trace                # http://127.0.0.1:4318

    from criogen import Agent, Telemetry
    agent = Agent("openai:gpt-5-mini", telemetry=Telemetry.local())

Each run's model calls, tool calls and steps on a time axis, with durations,
tokens, cost (from your ``prices``) and failures. Prompts and outputs appear
only with ``Telemetry.local(capture_content=True)``; nothing leaves the
computer either way. It listens on 127.0.0.1 unless given a ``token``.
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
    """A viewer serving in the background; :meth:`stop` ends it."""

    def __init__(self, native: Any):
        self._native = native

    @property
    def url(self) -> str:
        """The page's address."""
        return str(self._native.url)

    async def stop(self) -> None:
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
    """Starts the viewer in the background. ``db``: a SQLite file (default
    ``~/.criogen/traces.db``) or ``":memory:"``. ``prices``: USD per million
    tokens by ``"provider:model"`` or model name, for the cost column.
    ``token``: required to listen on another address than 127.0.0.1."""
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
    parser = argparse.ArgumentParser(prog="python -m criogen.trace", description="Criogen's local trace viewer.")
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
            print(f"criogen.trace: the environment variable {args.token_env} is not set", file=sys.stderr)
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
        print(f"Criogen traces: open {viewer.url}  (send runs with Telemetry.local(); Ctrl+C to stop)")
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

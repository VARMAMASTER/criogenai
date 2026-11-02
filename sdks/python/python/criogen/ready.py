"""``ready()``: one call that says whether Criogen can work here, for start-up
checks and health endpoints. It never raises; it reports. (In Python the
engine is part of the wheel, which pip only installs on systems it runs on,
so if ``import criogen`` worked the engine loads.)"""

import asyncio
import platform
import time
from typing import Any, Dict, Optional


def _engine() -> Dict[str, Any]:
    libc, version = platform.libc_ver()
    return {
        "ok": True,
        "platform": platform.system().lower(),
        "arch": platform.machine(),
        "libc": f"{libc} {version}".strip() or None,
    }


def _describe(error: BaseException) -> Dict[str, Any]:
    return {"error": str(error), "code": getattr(error, "code", None)}


async def ready(model: Any = None, timeout: float = 30.0) -> Dict[str, Any]:
    """Checks the engine and, with ``model``, that the model answers (one tiny
    call, a few tokens). ``{"ok", "engine": {...}, "model": {...}}``."""
    report: Dict[str, Any] = {"ok": True, "engine": _engine()}
    if model is None:
        return report
    from .agent import Agent  # here, so importing this module stays cheap

    name = model if isinstance(model, str) else f"{getattr(model, 'kind', 'model')}:{getattr(model, 'model', '')}"
    started = time.monotonic()
    try:
        agent = Agent(model, limits={"model_calls": 1})
        result = await asyncio.wait_for(agent.run("Reply with the single word OK."), timeout)
        report["model"] = {
            "ok": result.model_calls > 0,
            "model": name,
            "latency_ms": int((time.monotonic() - started) * 1000),
        }
    except Exception as error:  # a report, not a failure
        report["model"] = {
            "ok": False,
            "model": name,
            "latency_ms": int((time.monotonic() - started) * 1000),
            **_describe(error),
        }
    report["ok"] = report["engine"]["ok"] and report["model"]["ok"]
    return report


def ready_sync(model: Any = None, timeout: float = 30.0) -> Dict[str, Any]:
    """:func:`ready` for scripts (not inside an event loop)."""
    return asyncio.run(ready(model, timeout))

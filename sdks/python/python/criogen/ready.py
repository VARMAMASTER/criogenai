"""``ready()``: one call that says whether Criogen can work here, for start-up checks and health endpoints.

It never raises; it reports. (In Python the engine is part of the wheel,
which pip only installs on systems it runs on, so if ``import criogen``
worked the engine loads.)
"""

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
    """Checks the engine and, with ``model``, that the model answers.

    The model check is one small call (the agent is asked to reply ``OK``,
    with a limit of one model call). Failures are reported, never raised.

    Args:
        model: A model string (e.g. ``"openai:gpt-5-mini"``) or model object
            to check. Default: check only the engine.
        timeout: The longest the model check may take, in seconds. Default
            30.

    Returns:
        ``{"ok", "engine", "model"}``. ``ok`` is ``True`` when everything
        checked works. ``engine`` holds ``ok``, ``platform``, ``arch`` and
        ``libc``. ``model`` (only when a model was given) holds ``ok``,
        ``model`` (its name), ``latency_ms`` and, on failure, ``error`` and
        ``code``.

    Example:
        from criogen import ready

        report = await ready("openai:gpt-5-mini", timeout=10)
        if not report["ok"]:
            print(report["model"].get("code"), report["model"].get("error"))
    """
    report: Dict[str, Any] = {"ok": True, "engine": _engine()}
    if model is None:
        return report
    from .agent import Agent  # here, so importing this module stays cheap

    name = _model_name(model)
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


def _model_name(model: Any) -> str:
    """``"provider:model"`` for a string or provider model; else the class name (e.g. ``"FakeModel"``)."""
    if isinstance(model, str):
        return model
    kind, name = getattr(model, "kind", None), getattr(model, "model", None)
    if isinstance(kind, str) and isinstance(name, str) and name:
        return f"{kind}:{name}"
    if isinstance(name, str) and name:
        return name
    return type(model).__name__


def ready_sync(model: Any = None, timeout: float = 30.0) -> Dict[str, Any]:
    """:func:`ready` for scripts (not inside an event loop).

    Args:
        model: As for :func:`ready`.
        timeout: As for :func:`ready`, in seconds. Default 30.

    Returns:
        ``{"ok", "engine", "model"}``. ``ok`` is ``True`` when everything
        checked works. ``engine`` holds ``ok``, ``platform``, ``arch`` and
        ``libc``. ``model`` (only when a model was given) holds ``ok``,
        ``model`` (its name), ``latency_ms`` and, on failure, ``error`` and
        ``code``.

    Example:
        from criogen import ready_sync

        print(ready_sync()["engine"])   # {"ok": True, "platform": ..., "arch": ..., "libc": ...}
    """
    return asyncio.run(ready(model, timeout))

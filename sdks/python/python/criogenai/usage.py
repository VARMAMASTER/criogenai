"""What a run cost: which models answered, with their calls and tokens (from
the engine's per-message metadata), the call's wall time, and a cost estimate
from prices the caller supplies (they change too often to ship)."""

import time
from typing import Any, Dict, List, Mapping, Optional


def run_facts(messages: List[Dict[str, Any]], started: Optional[float]) -> Dict[str, Any]:
    """``duration_ms``, ``answered_by`` and ``models`` for a result. Only this
    call's model calls count: the messages after the last user message."""
    start = 0
    for i, message in enumerate(messages):
        if message.get("role") == "user":
            start = i + 1
    uses: Dict[tuple, Dict[str, Any]] = {}
    answered_by: Optional[Dict[str, str]] = None
    for message in messages[start:]:
        meta = message.get("metadata") or {}
        if message.get("role") != "assistant" or not (meta.get("provider") or meta.get("model")):
            continue
        key = (meta.get("provider") or "", meta.get("model") or "")
        use = uses.setdefault(key, {"provider": key[0], "model": key[1], "calls": 0, "usage": {}})
        use["calls"] += 1
        for name, value in (meta.get("usage") or {}).items():
            if isinstance(value, (int, float)):
                use["usage"][name] = use["usage"].get(name, 0) + value
        answered_by = {"provider": key[0], "model": key[1]}
    duration = int((time.monotonic() - started) * 1000) if started is not None else 0
    return {"duration_ms": duration, "answered_by": answered_by, "models": list(uses.values())}


def estimate_cost(result: Any, prices: Mapping[str, Mapping[str, float]]) -> Optional[float]:
    """Estimates what a run cost in USD, from prices you supply.

    CriogenAI ships no prices (they change too often). Cached input tokens
    are charged at ``cache_read`` (or ``input`` when it is missing).
    Reasoning tokens count as output tokens.

    Args:
        result: A ``RunResult`` or ``Reply``; its ``models`` field is read.
        prices: USD per million tokens for each model, keyed by
            ``"provider:model"`` or by model name:
            ``{"input": ..., "output": ..., "cache_read": ...}``
            (``cache_read`` is optional).

    Returns:
        The cost in USD, or ``None`` when a model the run used has no price.

    Example:
        from criogenai import Agent, FakeModel, estimate_cost

        result = Agent(FakeModel(["Hello."])).run_sync("Hi")
        cost = estimate_cost(result, {"fake": {"input": 0.25, "output": 2.0}})
        print(f"{cost:.8f} USD")
    """
    total = 0.0
    for use in result.models:
        price = prices.get(f"{use['provider']}:{use['model']}") or prices.get(use["model"])
        if not price:
            return None
        usage = use["usage"]
        cached = usage.get("cache_read_tokens", 0)
        fresh = max(usage.get("input_tokens", 0) - cached, 0)
        total += (
            fresh * price["input"]
            + cached * price.get("cache_read", price["input"])
            + usage.get("output_tokens", 0) * price["output"]
        ) / 1_000_000
    return total

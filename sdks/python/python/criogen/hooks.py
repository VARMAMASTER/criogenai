"""Hooks (design 0017 §5): code that runs around the agent loop, in order.

A hook is any object with some of these methods (each sync or async):

- ``on_start(ctx)``: once per run segment; may return ``HookAction(state=…)``.
- ``before_model(ctx)``: before each model call; may replace the
  transcript (``HookAction(messages=[…])``) or end the run.
- ``after_model(ctx)``: after each model reply (``ctx.messages`` ends with
  it); may replace it (``HookAction(message={…})``) or end the run.
- ``before_tool(call)``: before each tool call; may return a changed
  ``ToolCall``, a ``ToolResult`` to skip the tool, or raise ``ToolError``.
- ``after_tool(call, result)``: after each tool call; may return a
  replacement result.
- ``on_end(ctx, outcome)``: always, once per segment (``"completed"``,
  ``"suspended"`` or ``"failed"``); cannot change the outcome.

``on_start``, ``before_model`` and ``before_tool`` run in list order, the
others in reverse. An exception in ``on_start``, ``before_model`` or
``after_model`` fails the run with ``hook_failed`` (the exception is its
``__cause__``); one in ``before_tool`` / ``after_tool`` counts as the tool's
(see ``on_tool_exception``). Hook methods run on your event loop: keep sync
ones quick.
"""

import inspect
import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Tuple

from .tools import ToolError

STAGES = ("on_start", "before_model", "after_model", "before_tool", "after_tool", "on_end")


@dataclass
class HookContext:
    """What a hook sees."""

    run_id: str
    #: The transcript so far (dicts, as in ``result.messages``).
    messages: List[Dict[str, Any]]
    model_calls: int
    #: What this hook saved with ``HookAction(state=…)``, if anything.
    hook_state: Any
    #: The whole run state, read-only.
    state: Dict[str, Any]


_UNSET: Any = object()


@dataclass
class HookAction:
    """What a hook wants: save its ``state``; replace the transcript
    (``messages``, ``before_model``) or the new reply (``message``,
    ``after_model``); or ``end`` the run, with ``output``."""

    state: Any = _UNSET
    messages: Optional[List[Dict[str, Any]]] = None
    message: Optional[Dict[str, Any]] = None
    end: bool = False
    output: Any = None

    def _envelope(self) -> Dict[str, Any]:
        envelope: Dict[str, Any] = {"messages": self.messages, "message": self.message, "end": self.end, "output": self.output}
        if self.state is not _UNSET:
            envelope["state"] = self.state
        return envelope


@dataclass
class ToolCall:
    """A tool call going through ``before_tool`` (change ``name`` or
    ``args`` and return it)."""

    call_id: str
    name: str
    args: Dict[str, Any] = field(default_factory=dict)


@dataclass
class ToolResult:
    """Returned by ``before_tool``: use ``value`` as the result and skip the
    tool (e.g. a cached answer)."""

    value: Any


def _context(payload: Dict[str, Any]) -> HookContext:
    return HookContext(
        run_id=payload.get("run_id", ""),
        messages=payload.get("messages") or [],
        model_calls=payload.get("model_calls", 0),
        hook_state=payload.get("hook_state"),
        state=payload.get("state") or {},
    )


def _envelope(stage: str, value: Any) -> Dict[str, Any]:
    if value is None:
        return {}
    if stage in ("on_start", "before_model", "after_model"):
        if not isinstance(value, HookAction):
            raise TypeError(f"{stage} returns a HookAction or None, not {value!r}")
        return value._envelope()
    if stage == "before_tool":
        if isinstance(value, ToolResult):
            return {"result": value.value}
        if isinstance(value, ToolCall):
            return {"call": {"name": value.name, "args": value.args}}
        raise TypeError(f"before_tool returns a ToolCall, a ToolResult or None, not {value!r}")
    if stage == "after_tool":
        return {"result": value}
    return {}


def hook_adapter(hook: Any) -> Tuple[str, Callable[[str, str], Any], List[str]]:
    """``(name, adapter, stages)`` for the engine: ``adapter(stage,
    payload_json)`` returns a coroutine of an envelope."""
    stages = [stage for stage in STAGES if callable(getattr(hook, stage, None))]
    if not stages:
        raise TypeError(f"{hook!r} is not a hook: it has none of {', '.join(STAGES)}")
    name = getattr(hook, "name", None) or type(hook).__name__

    async def adapter(stage: str, payload_json: str) -> str:
        payload = json.loads(payload_json)
        method = getattr(hook, stage)
        try:
            if stage == "before_tool":
                value = method(ToolCall(payload["call_id"], payload["name"], payload.get("args") or {}))
            elif stage == "after_tool":
                call = payload["call"]
                value = method(ToolCall(call["call_id"], call["name"], call.get("args") or {}), payload.get("result"))
            elif stage == "on_end":
                value = method(_context(payload), payload.get("outcome"))
            else:
                value = method(_context(payload))
            if inspect.isawaitable(value):
                value = await value
        except ToolError as error:
            if stage != "before_tool":
                raise
            return json.dumps({"tool_error": error.message})
        return json.dumps(_envelope(stage, value), default=str)

    return name, adapter, stages

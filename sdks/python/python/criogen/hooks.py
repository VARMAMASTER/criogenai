"""Hooks: your code at fixed points of the agent loop.

Pass hooks with ``Agent(hooks=[...])``. A hook is any object with some of
these methods (each may be sync or async):

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
ones quick. A hook's ``name`` attribute, or else its class name, identifies
its saved state.

Example:

    from criogen import Agent, FakeModel, HookAction

    class StopAfterTwoCalls:
        def before_model(self, ctx):
            if ctx.model_calls >= 2:
                return HookAction(end=True, output="stopped: too many model calls")
            return None

    agent = Agent(FakeModel(["Hello."]), hooks=[StopAfterTwoCalls()])
"""

import inspect
import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Tuple

from .tools import ToolError

STAGES = ("on_start", "before_model", "after_model", "before_tool", "after_tool", "on_end")


@dataclass
class HookContext:
    """What a hook sees: passed to ``on_start``, ``before_model``,
    ``after_model`` and ``on_end``.

    Fields:
        run_id: The run's id.
        messages: The transcript so far, as dicts (the form of
            ``result.messages``). In ``after_model`` it ends with the new
            reply.
        model_calls: Model calls made in the run so far.
        hook_state: What this hook saved with ``HookAction(state=...)``, or
            ``None``.
        state: The whole run state, including values passed as keyword
            arguments to ``run`` or ``send``. Read-only: changes are not
            kept.
    """

    run_id: str
    messages: List[Dict[str, Any]]
    model_calls: int
    hook_state: Any
    state: Dict[str, Any]


_UNSET: Any = object()


@dataclass
class HookAction:
    """What a hook wants done; returned by ``on_start``, ``before_model`` or
    ``after_model`` (return ``None`` to change nothing).

    Fields:
        state: A JSON value to save for this hook; later calls see it as
            ``ctx.hook_state``. Left unset, the saved state is kept.
        messages: A new transcript to send to the model instead
            (``before_model``).
        message: A new reply to use instead of the model's
            (``after_model``), as a message dict.
        end: End the run now, with stop reason ``"hook_end"``.
        output: With ``end``, the run's ``output``.

    Example:
        class CountRuns:
            def on_start(self, ctx):
                runs = (ctx.hook_state or {}).get("runs", 0) + 1
                return HookAction(state={"runs": runs})
    """

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
    """A tool call, as ``before_tool`` and ``after_tool`` see it.

    In ``before_tool``, change ``name`` or ``args`` and return the call to
    run the changed call.

    Fields:
        call_id: The call's id.
        name: The tool to call.
        args: The arguments, as a dict.
    """

    call_id: str
    name: str
    args: Dict[str, Any] = field(default_factory=dict)


@dataclass
class ToolResult:
    """Returned by ``before_tool`` to skip the tool and use ``value`` as its
    result, for example a cached answer.

    Fields:
        value: The result the model receives, any JSON value.

    Example:
        class Cache:
            def before_tool(self, call):
                if call.name == "weather" and call.args.get("city") == "Paris":
                    return ToolResult("sunny (cached)")
                return None
    """

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

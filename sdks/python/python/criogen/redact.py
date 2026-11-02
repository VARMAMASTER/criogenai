"""``redact``: a function applied to every text in the transcript before each
model call: the user's input, earlier replies and tool results (text, and the
strings inside JSON values). It runs first among the hooks, so no other hook
and no provider sees the text before it. The redacted transcript is also what
a thread saves. Tool names, call ids and arguments are kept."""

import inspect
from typing import Any, Callable, Dict, List, Optional

from .hooks import HookAction, HookContext


async def _apply(redact: Callable[[str], Any], text: str) -> str:
    value = redact(text)
    if inspect.isawaitable(value):
        value = await value
    return value


async def _value(redact: Callable[[str], Any], value: Any) -> Any:
    if isinstance(value, str):
        return await _apply(redact, value)
    if isinstance(value, list):
        return [await _value(redact, v) for v in value]
    if isinstance(value, dict):
        return {k: await _value(redact, v) for k, v in value.items()}
    return value


async def _block(redact: Callable[[str], Any], block: Any) -> Any:
    if not isinstance(block, dict):
        return block
    out: Dict[str, Any] = dict(block)
    if isinstance(out.get("text"), str):
        out["text"] = await _apply(redact, out["text"])
    if "value" in out:
        out["value"] = await _value(redact, out["value"])
    if isinstance(out.get("content"), list):
        out["content"] = [await _block(redact, c) for c in out["content"]]
    return out


class Redaction:
    """The hook behind the agent's ``redact`` option."""

    name = "redact"

    def __init__(self, redact: Callable[[str], Any]):
        if not callable(redact):
            raise TypeError("redact must be a function text -> text")
        self._redact = redact

    async def before_model(self, ctx: HookContext) -> Optional[HookAction]:
        changed = False
        messages: List[Dict[str, Any]] = []
        for message in ctx.messages:
            content = message.get("content")
            if isinstance(content, list):
                new = [await _block(self._redact, b) for b in content]
                if new != content:
                    changed = True
                message = {**message, "content": new}
            messages.append(message)
        return HookAction(messages=messages) if changed else None

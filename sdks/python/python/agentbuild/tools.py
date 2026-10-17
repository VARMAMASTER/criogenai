"""Tools (design 0011 §2): Python functions the engine calls.

The schema comes from the type hints; the Rust engine validates the
model's arguments against it before the function runs.
"""

import inspect
import json
import typing
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Tuple, Union


class ToolError(Exception):
    """A failure the model should see (and may recover from), unlike a
    crash, which fails the run with ``tool_failed``."""

    def __init__(self, message: str, retryable: bool = False):
        super().__init__(message)
        self.message = message
        self.retryable = retryable


@dataclass(frozen=True)
class ToolContext:
    """What a tool can know about its call. Ask for it with a parameter
    annotated ``ToolContext``."""

    call_id: str
    run_id: str


_NONE = type(None)


def _schema(annotation: Any) -> Dict[str, Any]:
    """JSON Schema for a type hint."""
    if annotation is inspect.Parameter.empty or annotation is Any:
        return {}
    origin = typing.get_origin(annotation)
    args = typing.get_args(annotation)
    if origin is Union or (origin is not None and getattr(origin, "__name__", "") == "UnionType"):
        options = [a for a in args if a is not _NONE]
        inner = _schema(options[0]) if len(options) == 1 else {"anyOf": [_schema(a) for a in options]}
        if _NONE in args:
            return {"anyOf": [inner, {"type": "null"}]} if inner else {}
        return inner
    if origin is typing.Literal:
        return {"enum": list(args)}
    if annotation is str:
        return {"type": "string"}
    if annotation is bool:
        return {"type": "boolean"}
    if annotation is int:
        return {"type": "integer"}
    if annotation is float:
        return {"type": "number"}
    if annotation in (list, List) or origin in (list, List):
        return {"type": "array", "items": _schema(args[0])} if args else {"type": "array"}
    if annotation in (dict, Dict) or origin in (dict, Dict):
        return {"type": "object"}
    if hasattr(annotation, "model_json_schema"):  # Pydantic
        return annotation.model_json_schema()
    raise TypeError(f"no JSON Schema for the type {annotation!r}")


def _jsonable(value: Any) -> Any:
    if hasattr(value, "model_dump"):  # Pydantic
        return value.model_dump(mode="json")
    return value


class Tool:
    """A function the model can call. Made by ``@tool``."""

    def __init__(
        self,
        func: Callable[..., Any],
        name: Optional[str] = None,
        description: Optional[str] = None,
        read_only: bool = False,
        destructive: bool = False,
        idempotent: bool = False,
    ):
        self.func = func
        self.name = name or func.__name__
        self.description = description if description is not None else inspect.getdoc(func) or ""
        self.read_only = read_only
        self.destructive = destructive
        self.idempotent = idempotent
        self.is_async = inspect.iscoroutinefunction(func)
        hints = typing.get_type_hints(func)
        self._context_param: Optional[str] = None
        self._models: Dict[str, Any] = {}
        properties: Dict[str, Any] = {}
        required: List[str] = []
        for param in inspect.signature(func).parameters.values():
            annotation = hints.get(param.name, param.annotation)
            if annotation is ToolContext:
                self._context_param = param.name
                continue
            properties[param.name] = _schema(annotation)
            if hasattr(annotation, "model_validate"):
                self._models[param.name] = annotation
            if param.default is inspect.Parameter.empty:
                required.append(param.name)
        self.schema = {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": False,
        }

    def spec(self) -> Dict[str, Any]:
        return {
            "name": self.name,
            "description": self.description,
            "schema": self.schema,
            "read_only": self.read_only,
            "destructive": self.destructive,
            "idempotent": self.idempotent,
        }

    def _kwargs(self, args_json: str, ctx_json: str) -> Dict[str, Any]:
        kwargs = json.loads(args_json)
        for name, model in self._models.items():
            if name in kwargs:
                kwargs[name] = model.model_validate(kwargs[name])
        if self._context_param:
            kwargs[self._context_param] = ToolContext(**json.loads(ctx_json))
        return kwargs

    def adapter(self) -> Tuple[Callable[[str, str], Any], bool]:
        """What the engine calls: ``(args_json, ctx_json)`` → a JSON
        envelope (or, for async tools, a coroutine that gives one)."""
        async def call_async(args_json: str, ctx_json: str) -> str:
            try:
                value = await self.func(**self._kwargs(args_json, ctx_json))
            except ToolError as error:
                return json.dumps({"tool_error": error.message, "retryable": error.retryable})
            return json.dumps({"ok": _jsonable(value)}, default=str)

        def call_sync(args_json: str, ctx_json: str) -> str:
            try:
                value = self.func(**self._kwargs(args_json, ctx_json))
            except ToolError as error:
                return json.dumps({"tool_error": error.message, "retryable": error.retryable})
            return json.dumps({"ok": _jsonable(value)}, default=str)

        if self.is_async:
            return call_async, True
        return call_sync, False

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        return self.func(*args, **kwargs)

    def __repr__(self) -> str:
        return f"Tool({self.name!r})"


def tool(
    func: Optional[Callable[..., Any]] = None,
    *,
    name: Optional[str] = None,
    description: Optional[str] = None,
    read_only: bool = False,
    destructive: bool = False,
    idempotent: bool = False,
) -> Any:
    """Makes a function a tool: ``@tool`` or ``@tool(destructive=True)``.
    The docstring is the description; type hints are the schema."""

    def wrap(f: Callable[..., Any]) -> Tool:
        return Tool(f, name, description, read_only, destructive, idempotent)

    return wrap(func) if func is not None else wrap

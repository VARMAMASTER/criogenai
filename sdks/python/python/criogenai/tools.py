"""Tools: Python functions the model can call.

Decorate a function with ``@tool``. Its docstring is the description the
model reads, and its type hints become the JSON Schema of its arguments.
The Rust engine checks the model's arguments against that schema before
the function runs, so a tool only sees arguments of the right shape.

Supported hints: ``str``, ``int``, ``float``, ``bool``, ``list[...]``,
``dict``, ``Optional[...]``, ``Union[...]``, ``Literal[...]`` and Pydantic
models (passed to the function as instances). A parameter without a
default is required. Sync tools run on a worker thread, async tools on
your event loop. The return value is sent to the model as JSON (Pydantic
models are dumped; other values that JSON cannot hold become strings).
"""

import inspect
import json
import typing
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Tuple, Union


class ToolError(Exception):
    """A failure the model should see, and may recover from.

    Raise it from a tool: the model gets ``message`` as the call's error
    result and the run goes on. Any other exception counts as the tool
    crashing; ``Agent(on_tool_exception=...)`` decides what happens then.

    Args:
        message: What went wrong, written for the model.
        retryable: The call may succeed if tried again. Calls to tools
            marked ``read_only`` or ``idempotent`` are then retried, as the
            agent's ``tool_retry`` says; other tools are not retried.

    Example:
        @tool
        def lookup_order(order_id: str) -> str:
            "Looks up an order by its id."
            if not order_id.startswith("A-"):
                raise ToolError(f"unknown order id {order_id}; ids start with A-")
            return "shipped"
    """

    def __init__(self, message: str, retryable: bool = False):
        super().__init__(message)
        self.message = message
        self.retryable = retryable


@dataclass(frozen=True)
class ToolContext:
    """What a tool can know about its call.

    Ask for it with a parameter annotated ``ToolContext``; that parameter is
    filled by the engine and left out of the schema the model sees.

    Fields:
        call_id: The id of this tool call (as in the transcript and events).
        run_id: The id of the run that made the call.

    Example:
        @tool
        def save_note(text: str, ctx: ToolContext) -> str:
            "Saves a note."
            print(f"run {ctx.run_id}, call {ctx.call_id}")
            return "saved"
    """

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
    """A function the model can call; made by ``@tool``.

    Calling the ``Tool`` object calls the function directly, so tools stay
    easy to test. ``tool.schema`` is the JSON Schema of its arguments.

    Args:
        func: The function, sync or async.
        name: The tool name the model sees. Default: the function's name.
        description: What the tool does, for the model. Default: the
            function's docstring.
        read_only: The tool has no side effects. Such tools may be retried.
        destructive: The tool may destroy data. A hint kept with the tool; it
            does not ask for approval by itself (list the tool in
            ``Agent(approve=[...])`` for that).
        idempotent: Calling the tool twice with the same arguments has the
            same effect as calling it once. Such tools may be retried.

    Raises:
        TypeError: A parameter's type hint has no JSON Schema.
    """

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
        defs: Dict[str, Any] = {}
        required: List[str] = []
        for param in inspect.signature(func).parameters.values():
            annotation = hints.get(param.name, param.annotation)
            if annotation is ToolContext:
                self._context_param = param.name
                continue
            schema = _schema(annotation)
            # `$defs` belong at the root: `#/$defs/X` is resolved against it.
            for name, definition in schema.pop("$defs", {}).items():
                if defs.setdefault(name, definition) != definition:
                    raise TypeError(
                        f"two different types are both called {name!r} in the arguments of {self.name}"
                    )
            properties[param.name] = schema
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
        if defs:
            self.schema["$defs"] = defs

    def spec(self) -> Dict[str, Any]:
        """The tool as the engine reads it: name, description, schema and
        hints (internal)."""
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
    """Makes a function a tool: ``@tool`` or ``@tool(read_only=True)``.

    The docstring is the description the model reads; the type hints are
    the schema of the arguments.

    Args:
        func: The function, when used as ``@tool`` without parentheses.
        name: The tool name the model sees. Default: the function's name.
        description: What the tool does, for the model. Default: the
            function's docstring.
        read_only: The tool has no side effects. Such tools are retried after
            a retryable error or a timeout.
        destructive: The tool may destroy data. A hint only; it does not ask
            for approval by itself (use ``Agent(approve=[...])``).
        idempotent: Calling the tool twice with the same arguments has the
            same effect as once. Such tools are retried after a retryable
            error or a timeout.

    Returns:
        A ``Tool``, or (with arguments only) a decorator that makes one.

    Raises:
        TypeError: A parameter's type hint has no JSON Schema.

    Example:
        from typing import Literal
        from criogenai import tool

        @tool(read_only=True)
        def weather(city: str, unit: Literal["C", "F"] = "C") -> str:
            "Current weather for a city."
            return f"21 {unit} and sunny in {city}"

        print(weather.schema["required"])  # ['city']
    """

    def wrap(f: Callable[..., Any]) -> Tool:
        return Tool(f, name, description, read_only, destructive, idempotent)

    return wrap(func) if func is not None else wrap

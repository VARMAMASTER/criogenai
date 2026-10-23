"""Models (design 0011 §2): real providers, and ``FakeModel`` for tests.

Each turns into a small JSON spec the Rust engine builds the model from.
Strings work too: ``"openai:gpt-5-mini"`` takes its key from
``OPENAI_API_KEY``.
"""

import os
from typing import Any, Dict, List, Optional, Sequence, Tuple, Union

_KEYS = {
    "openai": "OPENAI_API_KEY",
    "gemini": "GEMINI_API_KEY",
    "anthropic": "ANTHROPIC_API_KEY",
    "openrouter": "OPENROUTER_API_KEY",
}


class _Model:
    kind = ""

    def __init__(self, model: str, api_key: Optional[str] = None, base_url: Optional[str] = None):
        self.model = model
        self.api_key = api_key if api_key is not None else _env_key(self.kind)
        self.base_url = base_url

    def spec(self) -> Dict[str, Any]:
        spec = {"kind": self.kind, "model": self.model}
        if self.api_key:
            spec["api_key"] = self.api_key
        if self.base_url:
            spec["base_url"] = self.base_url
        return spec

    def __repr__(self) -> str:  # never shows the key
        return f"{type(self).__name__}({self.model!r})"


def _env_key(kind: str) -> Optional[str]:
    name = _KEYS.get(kind)
    return os.environ.get(name) if name else None


class OpenAI(_Model):
    """OpenAI's API (key from ``OPENAI_API_KEY`` by default)."""

    kind = "openai"


class Gemini(_Model):
    """Google Gemini through its OpenAI-compatible endpoint."""

    kind = "gemini"


class Anthropic(_Model):
    """Anthropic's Messages API."""

    kind = "anthropic"


class OpenRouter(_Model):
    """OpenRouter."""

    kind = "openrouter"


class Ollama(_Model):
    """Ollama on this machine (or ``base_url``), no key."""

    kind = "ollama"


class OpenAICompatible(_Model):
    """Any OpenAI-compatible server (vLLM, LM Studio, a gateway)."""

    kind = "openai_compatible"

    def __init__(
        self,
        model: str,
        base_url: str,
        api_key: Optional[str] = None,
        name: str = "custom",
        allow_insecure_http: bool = False,
    ):
        """``allow_insecure_http``: send the key over plain ``http://`` to
        another machine (a vLLM server on your LAN). Refused by default: a
        key sent over http is readable by anyone on the path."""
        super().__init__(model, api_key, base_url)
        self.name = name
        self.allow_insecure_http = allow_insecure_http

    def spec(self) -> Dict[str, Any]:
        spec = super().spec()
        spec["name"] = self.name
        spec["allow_insecure_http"] = self.allow_insecure_http
        return spec


class FakeReply:
    """One scripted answer of a ``FakeModel``."""

    def __init__(self, spec: Dict[str, Any]):
        self._spec = spec

    @staticmethod
    def text(text: str) -> "FakeReply":
        return FakeReply({"text": text})

    @staticmethod
    def tool_call(name: str, args: Optional[Dict[str, Any]] = None) -> "FakeReply":
        return FakeReply.tool_calls([(name, args or {})])

    @staticmethod
    def tool_calls(calls: Sequence[Tuple[str, Dict[str, Any]]]) -> "FakeReply":
        return FakeReply({"tool_calls": [{"name": n, "args": a} for n, a in calls]})


class FakeModel:
    """A simulated model for tests: answers with the scripted replies in
    order (a string is a text reply), then echoes. Never a network call.
    ``fail_first``: the first n calls fail with ``rate_limited`` (a
    transient error the agent retries), to test retries. ``by_turn``: choose
    each reply by the conversation's turn (the first reply for a run's first
    model call, the second for its second, then the last repeats) instead
    of using replies up, so one model serves many runs, also at once.
    ``name``: the model name in replies (``"fake:<name>"``), to tell two
    fake models apart."""

    def __init__(
        self,
        replies: Sequence[Union[str, FakeReply]] = (),
        delay: float = 0.0,
        fail_first: int = 0,
        by_turn: bool = False,
        name: Optional[str] = None,
    ):
        self.replies: List[FakeReply] = [
            FakeReply.text(r) if isinstance(r, str) else r for r in replies
        ]
        self.delay = delay
        self.fail_first = fail_first
        self.by_turn = by_turn
        self.name = name

    def spec(self) -> Dict[str, Any]:
        return {
            "kind": "fake",
            "replies": [r._spec for r in self.replies],
            "delay_ms": int(self.delay * 1000),
            "fail_first": self.fail_first,
            "by_turn": self.by_turn,
            "name": self.name,
        }

    def __repr__(self) -> str:
        return f"FakeModel({len(self.replies)} replies)"


def model_spec(model: Any) -> Dict[str, Any]:
    """The JSON spec of a model object or ``"provider:model"`` string."""
    if isinstance(model, str):
        provider, sep, name = model.partition(":")
        classes = {
            "openai": OpenAI,
            "gemini": Gemini,
            "anthropic": Anthropic,
            "openrouter": OpenRouter,
            "ollama": Ollama,
        }
        if not sep or provider not in classes:
            raise ValueError(
                f"model {model!r}: use 'provider:model' with provider one of {sorted(classes)}"
            )
        model = classes[provider](name)
    if not hasattr(model, "spec"):
        raise TypeError(f"not a model: {model!r}")
    return model.spec()


class Embedder:
    """Turns text into vectors, for long-term memory searched by meaning
    (``Memory.semantic``, design 0017 §3). Built from the same providers as
    models; keys come from the same environment variables."""

    def __init__(self, spec: Dict[str, Any]):
        self._spec = spec

    @staticmethod
    def _provider(kind: str, model: str, api_key: Optional[str], base_url: Optional[str], dimensions: Optional[int]) -> "Embedder":
        spec: Dict[str, Any] = {"kind": kind, "model": model}
        key = api_key if api_key is not None else _env_key(kind)
        if key:
            spec["api_key"] = key
        if base_url:
            spec["base_url"] = base_url
        if dimensions is not None:
            spec["dimensions"] = dimensions
        return Embedder(spec)

    @staticmethod
    def openai(model: str = "text-embedding-3-small", api_key: Optional[str] = None, dimensions: Optional[int] = None) -> "Embedder":
        """OpenAI's ``/embeddings`` (key from ``OPENAI_API_KEY``)."""
        return Embedder._provider("openai", model, api_key, None, dimensions)

    @staticmethod
    def gemini(model: str = "text-embedding-004", api_key: Optional[str] = None) -> "Embedder":
        """Gemini's OpenAI-compatible ``/embeddings`` (``GEMINI_API_KEY``)."""
        return Embedder._provider("gemini", model, api_key, None, None)

    @staticmethod
    def ollama(model: str = "nomic-embed-text", base_url: Optional[str] = None) -> "Embedder":
        """Ollama on this machine (or ``base_url``): nothing leaves your network."""
        return Embedder._provider("ollama", model, None, base_url, None)

    @staticmethod
    def openai_compatible(model: str, base_url: str, api_key: Optional[str] = None, dimensions: Optional[int] = None) -> "Embedder":
        """Any OpenAI-compatible ``/embeddings`` (vLLM, a gateway)."""
        spec = Embedder._provider("openai_compatible", model, api_key, base_url, dimensions)._spec
        spec["name"] = "custom"
        return Embedder(spec)

    @staticmethod
    def fake(synonyms: Optional[Dict[str, str]] = None, failing: bool = False) -> "Embedder":
        """For tests, never a network call: texts sharing words are close;
        ``synonyms`` makes different words count as the same (``{"car":
        "automobile"}``); ``failing`` makes every call fail (search then
        falls back to keywords)."""
        return Embedder({"kind": "fake", "synonyms": [[k, v] for k, v in (synonyms or {}).items()], "failing": failing})

    def __repr__(self) -> str:  # never shows the key
        return f"Embedder({self._spec.get('kind')!r}, {self._spec.get('model')!r})"

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

    def __init__(self, model: str, base_url: str, api_key: Optional[str] = None, name: str = "custom"):
        super().__init__(model, api_key, base_url)
        self.name = name

    def spec(self) -> Dict[str, Any]:
        spec = super().spec()
        spec["name"] = self.name
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
    of using replies up, so one model serves many runs, also at once."""

    def __init__(
        self,
        replies: Sequence[Union[str, FakeReply]] = (),
        delay: float = 0.0,
        fail_first: int = 0,
        by_turn: bool = False,
    ):
        self.replies: List[FakeReply] = [
            FakeReply.text(r) if isinstance(r, str) else r for r in replies
        ]
        self.delay = delay
        self.fail_first = fail_first
        self.by_turn = by_turn

    def spec(self) -> Dict[str, Any]:
        return {
            "kind": "fake",
            "replies": [r._spec for r in self.replies],
            "delay_ms": int(self.delay * 1000),
            "fail_first": self.fail_first,
            "by_turn": self.by_turn,
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

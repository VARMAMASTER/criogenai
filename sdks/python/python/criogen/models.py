"""Models (design 0011 §2): real providers, and ``FakeModel`` for tests.

Each turns into a small JSON spec the Rust engine builds the model from.
Strings work too: ``"openai:gpt-5-mini"`` takes its key from
``OPENAI_API_KEY``, ``"deepseek:deepseek-chat"`` from ``DEEPSEEK_API_KEY``
(every name in ``_KEYS``, plus ``ollama``, ``lmstudio`` and ``kaggle``).
"""

import os
from typing import Any, Dict, List, Optional, Sequence, Tuple, Union

# Where each named provider's key lives (design 0006 §4.1). The tests check
# this against the engine's own table (``_native.PRESETS``).
_KEYS = {
    "openai": "OPENAI_API_KEY",
    "gemini": "GEMINI_API_KEY",
    "anthropic": "ANTHROPIC_API_KEY",
    "openrouter": "OPENROUTER_API_KEY",
    "deepseek": "DEEPSEEK_API_KEY",
    "mistral": "MISTRAL_API_KEY",
    "xai": "XAI_API_KEY",
    "groq": "GROQ_API_KEY",
    "cerebras": "CEREBRAS_API_KEY",
    "together": "TOGETHER_API_KEY",
    "fireworks": "FIREWORKS_API_KEY",
    "qwen": "DASHSCOPE_API_KEY",
    "moonshot": "MOONSHOT_API_KEY",
    "zai": "ZAI_API_KEY",
}


class _Model:
    kind = ""

    def __init__(self, model: str, api_key: Optional[str] = None, base_url: Optional[str] = None):
        self.model = model
        self.api_key = api_key if api_key is not None else _env_key(self.kind)
        self.base_url = base_url

    def spec(self) -> Dict[str, Any]:
        name = _KEYS.get(self.kind)
        if name and not self.api_key and not self.base_url:
            raise ValueError(
                f"no API key for {self.kind}: set {name} in the environment or a .env file "
                f"(criogen.load_env()), or pass api_key="
            )
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


class DeepSeek(_Model):
    """DeepSeek (``DEEPSEEK_API_KEY``), e.g. ``DeepSeek("deepseek-chat")``."""

    kind = "deepseek"


class Mistral(_Model):
    """Mistral AI (``MISTRAL_API_KEY``), e.g. ``Mistral("mistral-large-latest")``."""

    kind = "mistral"


class XAI(_Model):
    """xAI's Grok models (``XAI_API_KEY``)."""

    kind = "xai"


class Groq(_Model):
    """Groq's fast inference for open models (``GROQ_API_KEY``)."""

    kind = "groq"


class Cerebras(_Model):
    """Cerebras's fast inference for open models (``CEREBRAS_API_KEY``)."""

    kind = "cerebras"


class Together(_Model):
    """Together AI's open models (``TOGETHER_API_KEY``)."""

    kind = "together"


class Fireworks(_Model):
    """Fireworks AI's open models (``FIREWORKS_API_KEY``)."""

    kind = "fireworks"


class Qwen(_Model):
    """Qwen on Alibaba Cloud Model Studio, international endpoint
    (``DASHSCOPE_API_KEY``). For the China endpoint pass
    ``base_url="https://dashscope.aliyuncs.com/compatible-mode/v1"``."""

    kind = "qwen"


class Moonshot(_Model):
    """Moonshot AI's Kimi models (``MOONSHOT_API_KEY``). China keys:
    ``base_url="https://api.moonshot.cn/v1"``."""

    kind = "moonshot"


class ZAI(_Model):
    """Z.ai's GLM models (``ZAI_API_KEY``). China keys:
    ``base_url="https://open.bigmodel.cn/api/paas/v4"``."""

    kind = "zai"


class Kaggle:
    """Kaggle's Model Proxy (design 0020): the hosted models your Kaggle
    account's AI-model allowance pays for, e.g.
    ``Kaggle("google/gemini-3.6-flash")`` or ``"kaggle:google/gemini-3.6-flash"``.

    Credentials, in order: ``api_token`` (a Kaggle API token); ``proxy_url``
    and ``proxy_token`` (a proxy token minted elsewhere, as in a Kaggle
    notebook); else the environment: ``MODEL_PROXY_URL`` and
    ``MODEL_PROXY_API_KEY``, ``KAGGLE_API_TOKEN``, or the ``access_token`` file
    in ``KAGGLE_CONFIG_DIR`` or ``~/.kaggle``. The engine mints the short-lived
    proxy token and renews it before it expires. For prototypes and tests,
    not production traffic (see the models guide)."""

    kind = "kaggle"

    def __init__(
        self,
        model: str,
        api_token: Optional[str] = None,
        proxy_url: Optional[str] = None,
        proxy_token: Optional[str] = None,
        api_base: Optional[str] = None,
    ):
        self.model = model
        self.api_token = api_token
        self.proxy_url = proxy_url
        self.proxy_token = proxy_token
        self.api_base = api_base

    def spec(self) -> Dict[str, Any]:
        spec: Dict[str, Any] = {"kind": self.kind, "model": self.model}
        for key, value in (
            ("api_key", self.api_token),
            ("proxy_url", self.proxy_url),
            ("proxy_token", self.proxy_token),
            ("api_base", self.api_base),
        ):
            if value:
                spec[key] = value
        return spec

    def __repr__(self) -> str:  # never shows a token
        return f"Kaggle({self.model!r})"


class Azure:
    """Azure OpenAI or Microsoft Foundry (design 0006 §17): a deployment,
    e.g. ``Azure("gpt-5-mini", endpoint="my-resource")`` or
    ``"azure:gpt-5-mini"``. The model name is the deployment name.

    ``endpoint`` is the resource name or URL (else ``AZURE_OPENAI_ENDPOINT``).
    Credentials, in order: ``api_key``; ``token`` (a bearer token from
    elsewhere); ``client_secret`` with ``tenant_id`` and ``client_id`` (a
    service principal); ``managed_identity=True`` (with ``client_id`` for a
    user-assigned identity); else the environment: ``AZURE_OPENAI_API_KEY``,
    or ``AZURE_TENANT_ID`` + ``AZURE_CLIENT_ID`` + ``AZURE_CLIENT_SECRET``, or
    ``AZURE_USE_MANAGED_IDENTITY=1``. Entra ID tokens are obtained and renewed
    by the engine. ``api="chat"`` for Foundry's models from other makers."""

    kind = "azure"

    def __init__(
        self,
        model: str,
        endpoint: Optional[str] = None,
        api_key: Optional[str] = None,
        token: Optional[str] = None,
        tenant_id: Optional[str] = None,
        client_id: Optional[str] = None,
        client_secret: Optional[str] = None,
        managed_identity: bool = False,
        api: str = "responses",
        scope: Optional[str] = None,
        authority: Optional[str] = None,
        identity_endpoint: Optional[str] = None,
    ):
        if api not in ("responses", "chat"):
            raise ValueError("api must be 'responses' or 'chat'")
        self.model = model
        self._fields = {
            "endpoint": endpoint, "api_key": api_key, "token": token, "tenant_id": tenant_id,
            "client_id": client_id, "client_secret": client_secret, "api": api, "scope": scope,
            "authority": authority, "identity_endpoint": identity_endpoint,
        }
        self.managed_identity = managed_identity

    def spec(self) -> Dict[str, Any]:
        spec: Dict[str, Any] = {"kind": self.kind, "model": self.model}
        spec.update({k: v for k, v in self._fields.items() if v})
        if self.managed_identity:
            spec["managed_identity"] = True
        return spec

    def __repr__(self) -> str:  # never shows a key, token or secret
        return f"Azure({self.model!r})"


class Ollama(_Model):
    """Ollama on this machine (or ``base_url``), no key."""

    kind = "ollama"


class LMStudio(_Model):
    """LM Studio on this machine (or ``base_url``), no key."""

    kind = "lmstudio"


class OpenAICompatible(_Model):
    """Any OpenAI-compatible server (vLLM, a gateway, a provider without a
    name here)."""

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


class AnthropicCompatible(OpenAICompatible):
    """Any server that speaks Anthropic's Messages API: a self-hosted
    gateway, or a provider's Anthropic-style endpoint (DeepSeek, Z.ai,
    Qwen and Moonshot offer one)."""

    kind = "anthropic_compatible"


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
            "deepseek": DeepSeek,
            "mistral": Mistral,
            "xai": XAI,
            "groq": Groq,
            "cerebras": Cerebras,
            "together": Together,
            "fireworks": Fireworks,
            "qwen": Qwen,
            "moonshot": Moonshot,
            "zai": ZAI,
            "ollama": Ollama,
            "lmstudio": LMStudio,
            "kaggle": Kaggle,
            "azure": Azure,
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
    def gemini(model: str = "gemini-embedding-001", api_key: Optional[str] = None) -> "Embedder":
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

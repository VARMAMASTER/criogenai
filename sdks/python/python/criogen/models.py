"""Models: hosted providers, cloud platforms, local servers, and
``FakeModel`` for tests.

Pass a model object or a ``"provider:model"`` string wherever a model is
taken (``Agent``, ``Gateway``, fallbacks). A string takes the provider's key
from its usual environment variable: ``"openai:gpt-5-mini"`` from
``OPENAI_API_KEY``, ``"deepseek:deepseek-chat"`` from ``DEEPSEEK_API_KEY``
(``criogen.env.key_name`` gives the variable). The providers a string may
name: openai, anthropic, gemini, openrouter, deepseek, mistral, xai, groq,
cerebras, together, fireworks, qwen, moonshot, zai, ollama, lmstudio,
kaggle, azure, bedrock and vertex.

The named providers' classes (``OpenAI``, ``Anthropic``, ``Gemini``, ...)
take ``(model, api_key=None, base_url=None)``: ``api_key`` defaults to the
provider's environment variable, and ``base_url`` replaces the provider's
URL (a regional endpoint, a proxy). Using a named provider without a key
(and without ``base_url``) raises ``ValueError``. ``repr()`` of a model
never shows a key.
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
    """OpenAI's API, e.g. ``OpenAI("gpt-5-mini")`` or ``"openai:gpt-5-mini"``.

    Constructor: ``OpenAI(model, api_key=None, base_url=None)``. The key
    defaults to ``OPENAI_API_KEY``; ``base_url`` replaces OpenAI's URL.

    Example:
        from criogen import Agent, OpenAI

        agent = Agent(OpenAI("gpt-5-mini"), instructions="Answer briefly.")
        # the same, with the key from OPENAI_API_KEY:
        agent = Agent("openai:gpt-5-mini", instructions="Answer briefly.")
    """

    kind = "openai"


class Gemini(_Model):
    """Google Gemini through its OpenAI-compatible endpoint, e.g.
    ``Gemini("gemini-2.5-flash")`` or ``"gemini:gemini-2.5-flash"``.

    Constructor: ``Gemini(model, api_key=None, base_url=None)``; the key
    defaults to ``GEMINI_API_KEY``. For Gemini on Google Cloud, see
    ``Vertex``.
    """

    kind = "gemini"


class Anthropic(_Model):
    """Anthropic's Messages API, e.g. ``Anthropic("claude-sonnet-5-5")`` or
    ``"anthropic:claude-sonnet-5-5"``.

    Constructor: ``Anthropic(model, api_key=None, base_url=None)``; the key
    defaults to ``ANTHROPIC_API_KEY``.
    """

    kind = "anthropic"


class OpenRouter(_Model):
    """OpenRouter: many providers' models behind one key, e.g.
    ``OpenRouter("meta-llama/llama-3.3-70b-instruct")``.

    Constructor: ``OpenRouter(model, api_key=None, base_url=None)``; the key
    defaults to ``OPENROUTER_API_KEY``.
    """

    kind = "openrouter"


class DeepSeek(_Model):
    """DeepSeek, e.g. ``DeepSeek("deepseek-chat")``.

    Constructor: ``DeepSeek(model, api_key=None, base_url=None)``; the key
    defaults to ``DEEPSEEK_API_KEY``.
    """

    kind = "deepseek"


class Mistral(_Model):
    """Mistral AI, e.g. ``Mistral("mistral-large-latest")``.

    Constructor: ``Mistral(model, api_key=None, base_url=None)``; the key
    defaults to ``MISTRAL_API_KEY``.
    """

    kind = "mistral"


class XAI(_Model):
    """xAI's Grok models.

    Constructor: ``XAI(model, api_key=None, base_url=None)``; the key
    defaults to ``XAI_API_KEY``.
    """

    kind = "xai"


class Groq(_Model):
    """Groq's inference for open models, e.g.
    ``Groq("llama-3.3-70b-versatile")``.

    Constructor: ``Groq(model, api_key=None, base_url=None)``; the key
    defaults to ``GROQ_API_KEY``.
    """

    kind = "groq"


class Cerebras(_Model):
    """Cerebras's inference for open models.

    Constructor: ``Cerebras(model, api_key=None, base_url=None)``; the key
    defaults to ``CEREBRAS_API_KEY``.
    """

    kind = "cerebras"


class Together(_Model):
    """Together AI's open models.

    Constructor: ``Together(model, api_key=None, base_url=None)``; the key
    defaults to ``TOGETHER_API_KEY``.
    """

    kind = "together"


class Fireworks(_Model):
    """Fireworks AI's open models.

    Constructor: ``Fireworks(model, api_key=None, base_url=None)``; the key
    defaults to ``FIREWORKS_API_KEY``.
    """

    kind = "fireworks"


class Qwen(_Model):
    """Qwen on Alibaba Cloud Model Studio, international endpoint.

    Constructor: ``Qwen(model, api_key=None, base_url=None)``; the key
    defaults to ``DASHSCOPE_API_KEY``. For the China endpoint pass
    ``base_url="https://dashscope.aliyuncs.com/compatible-mode/v1"``.
    """

    kind = "qwen"


class Moonshot(_Model):
    """Moonshot AI's Kimi models.

    Constructor: ``Moonshot(model, api_key=None, base_url=None)``; the key
    defaults to ``MOONSHOT_API_KEY``. For China keys pass
    ``base_url="https://api.moonshot.cn/v1"``.
    """

    kind = "moonshot"


class ZAI(_Model):
    """Z.ai's GLM models.

    Constructor: ``ZAI(model, api_key=None, base_url=None)``; the key
    defaults to ``ZAI_API_KEY``. For China keys pass
    ``base_url="https://open.bigmodel.cn/api/paas/v4"``.
    """

    kind = "zai"


class Kaggle:
    """Kaggle's Model Proxy: the hosted models your Kaggle account's model
    allowance pays for, e.g. ``Kaggle("google/gemini-3.6-flash")`` or
    ``"kaggle:google/gemini-3.6-flash"``.

    Credentials, in order: ``proxy_url`` with ``proxy_token`` (a proxy token
    minted elsewhere, as in a Kaggle notebook); ``api_token``; else the
    environment: ``MODEL_PROXY_URL`` and ``MODEL_PROXY_API_KEY``,
    ``KAGGLE_API_TOKEN``, or the ``access_token`` file in
    ``KAGGLE_CONFIG_DIR`` or ``~/.kaggle``. From an API token the engine
    mints the short-lived proxy token and renews it before it expires. Meant
    for prototypes and tests, not production traffic.

    Args:
        model: The model's Kaggle name, such as
            ``"google/gemini-3.6-flash"``.
        api_token: A Kaggle API token.
        proxy_url: The proxy's URL, with ``proxy_token``.
        proxy_token: A proxy token minted elsewhere, with ``proxy_url``.
        api_base: Another Kaggle API base URL for minting tokens (default
            ``https://api.kaggle.com``); for tests.
    """

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
        """The JSON spec the engine builds the model from (internal)."""
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


class Vertex:
    """Google Vertex AI, e.g.
    ``Vertex("google/gemini-3.6-flash", project="my-project", location="global")``
    or ``"vertex:claude-sonnet-5"``.

    Credentials, in order: ``token``, ``credentials_file``, else
    ``GOOGLE_APPLICATION_CREDENTIALS``, the file that ``gcloud auth
    application-default login`` writes, or the metadata server when running
    on Google Cloud. Service-account key files are not supported.
    ``claude-...`` models go to Claude's endpoint, the others to the
    OpenAI-compatible one.

    Args:
        model: The model, such as ``"google/gemini-3.6-flash"`` or
            ``"claude-sonnet-5"``.
        project: The Google Cloud project. Default:
            ``GOOGLE_CLOUD_PROJECT``.
        location: The region, or ``"global"``. Default:
            ``GOOGLE_CLOUD_LOCATION``. There is no built-in default, because
            it decides where data is processed.
        token: An OAuth access token obtained elsewhere.
        credentials_file: A path to an application-default credentials file.
        api: ``"auto"`` (by model name), ``"chat"`` (OpenAI-compatible) or
            ``"claude"``. Other values raise ``ValueError``.
        host: Another API host (a private endpoint).
        token_url: Another OAuth token endpoint; for tests.
        metadata_url: Another metadata-server URL; for tests.

    Example:
        from criogen import Agent, Vertex

        model = Vertex("google/gemini-3.6-flash", project="my-project", location="europe-west4")
        agent = Agent(model)
    """

    kind = "vertex"

    def __init__(
        self,
        model: str,
        project: Optional[str] = None,
        location: Optional[str] = None,
        token: Optional[str] = None,
        credentials_file: Optional[str] = None,
        api: str = "auto",
        host: Optional[str] = None,
        token_url: Optional[str] = None,
        metadata_url: Optional[str] = None,
    ):
        if api not in ("auto", "chat", "claude"):
            raise ValueError("api must be 'auto', 'chat' or 'claude'")
        self.model = model
        self._fields = {
            "project": project, "location": location, "token": token,
            "credentials_file": credentials_file, "api": api, "host": host,
            "token_url": token_url, "metadata_url": metadata_url,
        }

    def spec(self) -> Dict[str, Any]:
        """The JSON spec the engine builds the model from (internal)."""
        spec: Dict[str, Any] = {"kind": self.kind, "model": self.model}
        spec.update({k: v for k, v in self._fields.items() if v})
        return spec

    def __repr__(self) -> str:  # never shows a token
        return f"Vertex({self.model!r})"


class Bedrock:
    """Amazon Bedrock with an API key, e.g.
    ``Bedrock("us.anthropic.claude-sonnet-5", region="us-east-1")`` or
    ``"bedrock:us.anthropic.claude-sonnet-5"``.

    Claude models are called over the Anthropic Messages API, other makers'
    models over Chat Completions. IAM roles (SigV4 signing) are not
    supported.

    Args:
        model: The Bedrock model id or inference profile.
        region: The AWS region. Default: ``AWS_REGION`` or
            ``AWS_DEFAULT_REGION``. There is no built-in default, so data
            never goes to a region you did not choose.
        api_key: A Bedrock API key. Default: ``AWS_BEARER_TOKEN_BEDROCK``.
            Without one, building an agent fails with ``ModelError``.
        api: ``"auto"`` (by model), ``"messages"``, ``"chat"`` or
            ``"responses"``. Other values raise ``ValueError``.
        base_url: Another endpoint, such as bedrock-mantle or a VPC
            endpoint.

    Example:
        from criogen import Agent, Bedrock

        agent = Agent(Bedrock("us.anthropic.claude-sonnet-5", region="us-east-1"))
    """

    kind = "bedrock"

    def __init__(
        self,
        model: str,
        region: Optional[str] = None,
        api_key: Optional[str] = None,
        api: str = "auto",
        base_url: Optional[str] = None,
    ):
        if api not in ("auto", "messages", "chat", "responses"):
            raise ValueError("api must be 'auto', 'messages', 'chat' or 'responses'")
        self.model = model
        self._fields = {"region": region, "api_key": api_key, "api": api, "base_url": base_url}

    def spec(self) -> Dict[str, Any]:
        """The JSON spec the engine builds the model from (internal)."""
        spec: Dict[str, Any] = {"kind": self.kind, "model": self.model}
        spec.update({k: v for k, v in self._fields.items() if v})
        return spec

    def __repr__(self) -> str:  # never shows a key
        return f"Bedrock({self.model!r})"


class Azure:
    """Azure OpenAI or Microsoft Foundry: a deployment, e.g.
    ``Azure("gpt-5-mini", endpoint="my-resource")`` or ``"azure:gpt-5-mini"``.

    Credentials, in order: ``api_key``; ``token``; ``client_secret`` with
    ``tenant_id`` and ``client_id`` (a service principal);
    ``managed_identity=True``; else the environment:
    ``AZURE_OPENAI_API_KEY``, or ``AZURE_TENANT_ID`` + ``AZURE_CLIENT_ID`` +
    ``AZURE_CLIENT_SECRET``, or ``AZURE_USE_MANAGED_IDENTITY=1``. The engine
    obtains Entra ID tokens and renews them.

    Args:
        model: The deployment name.
        endpoint: The resource name or its URL. Default:
            ``AZURE_OPENAI_ENDPOINT``.
        api_key: The resource's API key.
        token: A bearer token obtained elsewhere.
        tenant_id: The Entra ID tenant, for a service principal. Default:
            ``AZURE_TENANT_ID``.
        client_id: The service principal's client id, or the client id of a
            user-assigned managed identity. Default for a service principal:
            ``AZURE_CLIENT_ID``.
        client_secret: The service principal's secret.
        managed_identity: Use the managed identity of the machine or app
            running this code.
        api: ``"responses"`` (default) or ``"chat"`` (Chat Completions, for
            Foundry models from other makers). Other values raise
            ``ValueError``.
        scope: Another token scope (default
            ``https://cognitiveservices.azure.com/.default``).
        authority: Another Entra ID authority, such as a sovereign cloud's.
        identity_endpoint: Another managed-identity endpoint; for tests.

    Example:
        from criogen import Agent, Azure

        agent = Agent(Azure("gpt-5-mini", endpoint="my-resource", managed_identity=True))
    """

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
        """The JSON spec the engine builds the model from (internal)."""
        spec: Dict[str, Any] = {"kind": self.kind, "model": self.model}
        spec.update({k: v for k, v in self._fields.items() if v})
        if self.managed_identity:
            spec["managed_identity"] = True
        return spec

    def __repr__(self) -> str:  # never shows a key, token or secret
        return f"Azure({self.model!r})"


class Ollama(_Model):
    """Ollama on this machine (``http://localhost:11434/v1``), no key, e.g.
    ``Ollama("llama3.2")`` or ``"ollama:llama3.2"``.

    Constructor: ``Ollama(model, api_key=None, base_url=None)``; pass
    ``base_url`` for Ollama on another address.
    """

    kind = "ollama"


class LMStudio(_Model):
    """LM Studio on this machine (``http://localhost:1234/v1``), no key.

    Constructor: ``LMStudio(model, api_key=None, base_url=None)``; pass
    ``base_url`` for LM Studio on another address.
    """

    kind = "lmstudio"


class OpenAICompatible(_Model):
    """Any server that speaks OpenAI's Chat Completions API: vLLM, a gateway,
    or a provider without a class here.

    Args:
        model: The model name the server expects.
        base_url: The server's API URL, usually ending in ``/v1``.
        api_key: The key, if the server needs one. No environment variable
            is read.
        name: The provider name in results and telemetry
            (``answered_by["provider"]``).
        allow_insecure_http: Send the key over plain ``http://`` to another
            machine (a vLLM server on your network). Refused by default: a
            key sent over ``http`` is readable by anyone on the path. Servers
            on this machine are always allowed.

    Example:
        from criogen import Agent, OpenAICompatible

        model = OpenAICompatible("my-model", base_url="http://localhost:8000/v1")
        agent = Agent(model)
    """

    kind = "openai_compatible"

    def __init__(
        self,
        model: str,
        base_url: str,
        api_key: Optional[str] = None,
        name: str = "custom",
        allow_insecure_http: bool = False,
    ):
        """Builds the model; the class describes the arguments."""
        super().__init__(model, api_key, base_url)
        self.name = name
        self.allow_insecure_http = allow_insecure_http

    def spec(self) -> Dict[str, Any]:
        """The JSON spec the engine builds the model from (internal)."""
        spec = super().spec()
        spec["name"] = self.name
        spec["allow_insecure_http"] = self.allow_insecure_http
        return spec


class AnthropicCompatible(OpenAICompatible):
    """Any server that speaks Anthropic's Messages API: a self-hosted
    gateway, or a provider's Anthropic-style endpoint (DeepSeek, Z.ai, Qwen
    and Moonshot offer one).

    Takes the same arguments as ``OpenAICompatible``.
    """

    kind = "anthropic_compatible"


class FakeReply:
    """One scripted answer of a ``FakeModel``: text, or tool calls.

    Build replies with ``FakeReply.text``, ``FakeReply.tool_call`` and
    ``FakeReply.tool_calls``. A plain string in ``FakeModel``'s list is a text
    reply.

    Args:
        spec: The reply as the engine reads it (internal); use the static
            methods instead.
    """

    def __init__(self, spec: Dict[str, Any]):
        self._spec = spec

    @staticmethod
    def text(text: str) -> "FakeReply":
        """A text answer.

        Args:
            text: The answer's text.
        """
        return FakeReply({"text": text})

    @staticmethod
    def tool_call(name: str, args: Optional[Dict[str, Any]] = None) -> "FakeReply":
        """An answer that calls one tool.

        Args:
            name: The tool to call. For an agent with ``output=``, the name
                ``"final_output"`` gives the structured output.
            args: The call's arguments. ``None`` means ``{}``.
        """
        return FakeReply.tool_calls([(name, args or {})])

    @staticmethod
    def tool_calls(calls: Sequence[Tuple[str, Dict[str, Any]]]) -> "FakeReply":
        """An answer that calls several tools at once.

        Args:
            calls: ``(name, args)`` pairs, in call order.
        """
        return FakeReply({"tool_calls": [{"name": n, "args": a} for n, a in calls]})


class FakeModel:
    """A simulated model for tests; it never makes a network call.

    It answers with the scripted replies in order. When they are used up,
    it echoes the last user message (``"[fake-llm] reply to: ..."``).

    Args:
        replies: The answers, in order: ``FakeReply`` objects, or strings for
            text answers.
        delay: How long each answer takes, in seconds.
        fail_first: The first n calls fail with ``rate_limited``, a transient
            error that the agent retries; use it to test retries and
            fallbacks.
        by_turn: Choose each reply by the model call's place in the run (the
            first reply for a run's first model call, the second for its
            second; the last repeats) instead of using replies up. One model
            then serves many runs, also at the same time.
        name: The model name in results (``answered_by["model"]``; provider
            ``"fake"``), to tell two fake models apart. Default ``"fake"``.

    Example:
        from criogen import Agent, FakeModel, FakeReply, tool

        @tool
        def weather(city: str) -> str:
            "Current weather for a city."
            return f"Sunny in {city}"

        model = FakeModel([FakeReply.tool_call("weather", {"city": "Paris"}), "It is sunny."])
        result = Agent(model, tools=[weather]).run_sync("Weather in Paris?")
        assert result.text == "It is sunny." and result.tool_calls == 1
    """

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
        """The JSON spec the engine builds the model from (internal)."""
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
            "bedrock": Bedrock,
            "vertex": Vertex,
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
    """Turns text into vectors, for search by meaning.

    Used by ``Memory.semantic``, collections and ``Gateway.embed``. Build one
    with the static methods; keys come from the same environment variables
    as the models'. ``repr()`` never shows a key.

    Args:
        spec: The embedder as the engine reads it (internal); use the static
            methods instead.

    Example:
        from criogen import Embedder, Gateway

        vectors = Gateway().embed_sync(Embedder.fake(), ["a red car", "a blue car"])
        print(len(vectors), len(vectors[0]))  # 2 256
    """

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
        """OpenAI's ``/embeddings``.

        Args:
            model: The embedding model.
            api_key: The key. Default: ``OPENAI_API_KEY``.
            dimensions: Shorter vectors, for models that support it. ``None``
                keeps the model's size.
        """
        return Embedder._provider("openai", model, api_key, None, dimensions)

    @staticmethod
    def gemini(model: str = "gemini-embedding-001", api_key: Optional[str] = None) -> "Embedder":
        """Gemini's OpenAI-compatible ``/embeddings``.

        Args:
            model: The embedding model.
            api_key: The key. Default: ``GEMINI_API_KEY``.
        """
        return Embedder._provider("gemini", model, api_key, None, None)

    @staticmethod
    def ollama(model: str = "nomic-embed-text", base_url: Optional[str] = None) -> "Embedder":
        """Ollama's ``/embeddings`` on this machine: nothing leaves it.

        Args:
            model: The embedding model, pulled into Ollama first.
            base_url: Ollama on another address. Default
                ``http://localhost:11434/v1``.
        """
        return Embedder._provider("ollama", model, None, base_url, None)

    @staticmethod
    def openai_compatible(model: str, base_url: str, api_key: Optional[str] = None, dimensions: Optional[int] = None) -> "Embedder":
        """Any OpenAI-compatible ``/embeddings`` (vLLM, a gateway).

        Args:
            model: The embedding model name the server expects.
            base_url: The server's API URL, usually ending in ``/v1``.
            api_key: The key, if the server needs one.
            dimensions: Shorter vectors, for models that support it.
        """
        spec = Embedder._provider("openai_compatible", model, api_key, base_url, dimensions)._spec
        spec["name"] = "custom"
        return Embedder(spec)

    @staticmethod
    def fake(synonyms: Optional[Dict[str, str]] = None, failing: bool = False) -> "Embedder":
        """An embedder for tests; it never makes a network call.

        Texts that share words get close vectors.

        Args:
            synonyms: Words to count as the same word, such as
                ``{"car": "automobile"}``.
            failing: Make every call fail, to test the fallback: memory
                search then falls back to keywords.
        """
        return Embedder({"kind": "fake", "synonyms": [[k, v] for k, v in (synonyms or {}).items()], "failing": failing})

    def __repr__(self) -> str:  # never shows the key
        return f"Embedder({self._spec.get('kind')!r}, {self._spec.get('model')!r})"

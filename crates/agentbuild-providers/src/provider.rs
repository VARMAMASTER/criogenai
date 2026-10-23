//! Endpoints and `ProviderModel`: a protocol, an endpoint and the HTTP
//! transport, combined into a [`Model`] (design 0006 §3, §4).

use serde_json::Value;

use crate::anthropic_messages::{self, AnthropicOptions, EventDecoder};
use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::model::{BoxFuture, DeltaSink, Model};
use crate::openai_chat::{self, ChunkDecoder, OpenAiQuirks};
use crate::request::{Capabilities, ModelRequest, ModelResponse};
use crate::sse::{SseEvent, SseParser};
use crate::stream::{Assembler, StreamItem, Warning};
use crate::transport::{HttpConfig, Transport, WireRequest};

/// The wire format an endpoint speaks.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Protocol {
    /// OpenAI chat completions (`POST {base_url}/chat/completions`).
    OpenAiChat(OpenAiQuirks),
    /// Anthropic Messages (`POST {base_url}/messages`).
    AnthropicMessages(AnthropicOptions),
}

/// Where and how to reach a provider. Plain data; presets cover common
/// servers. The provider is never guessed from a model name.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Endpoint {
    /// Short name, used as `provider` in messages and errors (e.g. `ollama`).
    pub name: String,
    /// Wire format.
    pub protocol: Protocol,
    /// Base URL without a trailing slash, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// Sent as `Authorization: Bearer …` (OpenAI chat) or `x-api-key`
    /// (Anthropic) when set. Redacted from errors.
    pub api_key: Option<String>,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// What models behind this endpoint support (unknown = `None`).
    pub capabilities: Capabilities,
    /// Allow sending credentials over plain `http://` to a host that is not
    /// this machine. Off by default: see [`Endpoint::allow_insecure_http`].
    pub insecure_http: bool,
}

impl std::fmt::Debug for Endpoint {
    /// The key and header values are not shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field(
                "headers",
                &self.headers.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("capabilities", &self.capabilities)
            .field("insecure_http", &self.insecure_http)
            .finish()
    }
}

/// Header names that carry credentials.
fn is_secret_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "authorization"
        || name == "proxy-authorization"
        || name.contains("api-key")
        || name.contains("apikey")
        || name.contains("token")
        || name.contains("secret")
}

/// `true` for `localhost`, `127.x.x.x` and `::1`.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The scheme and host of a URL.
fn url_host(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    Some((scheme, host))
}

impl Endpoint {
    /// Allows credentials over plain `http://` to another machine (for
    /// example a vLLM server on your LAN). Without this, building a model
    /// for such an endpoint fails: a key sent over `http` is readable by
    /// anyone on the path.
    pub fn allow_insecure_http(mut self) -> Self {
        self.insecure_http = true;
        self
    }

    /// The HTTP transport for this endpoint: refuses credentials over plain
    /// `http` to another machine, and removes every credential (the key and
    /// auth-like header values) from error messages.
    pub(crate) fn transport(&self, config: HttpConfig) -> Result<Transport, ModelError> {
        let has_credentials =
            self.api_key.is_some() || self.headers.iter().any(|(name, _)| is_secret_header(name));
        if has_credentials
            && !self.insecure_http
            && let Some(("http", host)) = url_host(&self.base_url)
            && !is_loopback_host(host)
        {
            return Err(ModelError::new(
                ModelErrorKind::InvalidRequest,
                format!(
                    "refusing to send credentials over plain http to `{host}`: use https, or                      `Endpoint::allow_insecure_http` for a trusted network"
                ),
            )
            .with_dispatch(Dispatch::NotSent));
        }
        let secrets = self.api_key.iter().cloned().chain(
            self.headers
                .iter()
                .filter(|(name, _)| is_secret_header(name))
                .map(|(_, value)| value.clone()),
        );
        Ok(Transport::new(config)?.redacting(secrets))
    }

    /// Any OpenAI-compatible server.
    pub fn openai_compatible(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            protocol: Protocol::OpenAiChat(OpenAiQuirks::default()),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.filter(|k| !k.is_empty()),
            headers: Vec::new(),
            capabilities: Capabilities {
                streaming: Some(true),
                ..Capabilities::default()
            },
            insecure_http: false,
        }
    }

    /// OpenAI's API.
    pub fn openai(api_key: impl Into<String>) -> Self {
        let mut endpoint =
            Self::openai_compatible("openai", "https://api.openai.com/v1", Some(api_key.into()));
        endpoint.protocol = Protocol::OpenAiChat(OpenAiQuirks::openai());
        endpoint.capabilities.tools = Some(true);
        endpoint.capabilities.parallel_tool_calls = Some(true);
        endpoint
    }

    /// OpenRouter.
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self::openai_compatible(
            "openrouter",
            "https://openrouter.ai/api/v1",
            Some(api_key.into()),
        )
    }

    /// Google Gemini, through its OpenAI-compatible endpoint
    /// (`https://generativelanguage.googleapis.com/v1beta/openai`). Models
    /// such as `gemini-2.5-flash`. Tool calls, streaming and JSON output
    /// work (checked live in `tests/live`).
    pub fn gemini(api_key: impl Into<String>) -> Self {
        let mut endpoint = Self::openai_compatible(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            Some(api_key.into()),
        );
        endpoint.capabilities.tools = Some(true);
        if let Protocol::OpenAiChat(quirks) = &mut endpoint.protocol {
            quirks.thought_signatures = true;
        }
        endpoint
    }

    /// Ollama on this machine (`http://localhost:11434/v1`), no key.
    pub fn ollama() -> Self {
        Self::openai_compatible("ollama", "http://localhost:11434/v1", None)
    }

    /// LM Studio on this machine (`http://localhost:1234/v1`), no key.
    pub fn lmstudio() -> Self {
        Self::openai_compatible("lmstudio", "http://localhost:1234/v1", None)
    }

    /// A vLLM server.
    pub fn vllm(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self::openai_compatible("vllm", base_url, api_key)
    }

    /// Any server that speaks the Anthropic Messages API.
    pub fn anthropic_compatible(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            protocol: Protocol::AnthropicMessages(AnthropicOptions::default()),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.filter(|k| !k.is_empty()),
            headers: Vec::new(),
            capabilities: Capabilities {
                streaming: Some(true),
                ..Capabilities::default()
            },
            insecure_http: false,
        }
    }

    /// Anthropic's API (`https://api.anthropic.com/v1`).
    pub fn anthropic(api_key: impl Into<String>) -> Self {
        let mut endpoint = Self::anthropic_compatible(
            "anthropic",
            "https://api.anthropic.com/v1",
            Some(api_key.into()),
        );
        let caps = &mut endpoint.capabilities;
        caps.tools = Some(true);
        caps.parallel_tool_calls = Some(true);
        caps.native_structured_output = Some(true);
        caps.vision = Some(true);
        caps.reasoning = Some(true);
        caps.tool_result_images = Some(true);
        endpoint
    }

    /// Replaces the Anthropic options (no effect on other protocols).
    pub fn with_anthropic_options(mut self, options: AnthropicOptions) -> Self {
        if let Protocol::AnthropicMessages(current) = &mut self.protocol {
            *current = options;
        }
        self
    }

    /// Adds a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Replaces the base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }
}

/// A model reached over HTTP: endpoint + model name + transport.
///
/// ```no_run
/// use agentbuild_providers::{Endpoint, Message, Model, ModelRequest, ProviderModel};
///
/// # async fn demo() -> Result<(), agentbuild_providers::ModelError> {
/// let model = ProviderModel::new(Endpoint::ollama(), "llama3.2")?;
/// let response = model.invoke(&ModelRequest::new(vec![Message::user("hi")])).await?;
/// println!("{}", response.message.text());
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct ProviderModel {
    endpoint: Endpoint,
    model: String,
    transport: Transport,
}

impl ProviderModel {
    /// A model with default timeouts (0003 §5.1).
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Result<Self, ModelError> {
        Self::with_config(endpoint, model, HttpConfig::default())
    }

    /// A model with explicit timeouts.
    pub fn with_config(
        endpoint: Endpoint,
        model: impl Into<String>,
        config: HttpConfig,
    ) -> Result<Self, ModelError> {
        let transport = endpoint.transport(config)?;
        Ok(Self {
            endpoint,
            model: model.into(),
            transport,
        })
    }

    /// The endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn wire(
        &self,
        request: &ModelRequest,
        stream: bool,
        warnings: &mut Vec<Warning>,
    ) -> Result<WireRequest, ModelError> {
        let endpoint = &self.endpoint;
        let (path, body, betas) = match &endpoint.protocol {
            Protocol::OpenAiChat(quirks) => {
                let body = openai_chat::encode(
                    request,
                    &self.model,
                    &endpoint.name,
                    quirks,
                    stream,
                    warnings,
                )?;
                ("chat/completions", body, Vec::new())
            }
            Protocol::AnthropicMessages(options) => {
                let wire = anthropic_messages::encode(
                    request,
                    &self.model,
                    &endpoint.name,
                    options,
                    stream,
                    warnings,
                )?;
                ("messages", wire.body, wire.betas)
            }
        };
        let mut wire = WireRequest::post(
            format!("{}/{path}", endpoint.base_url),
            serde_json::to_vec(&body).expect("JSON values serialize"),
        )
        .header("content-type", "application/json");
        if stream {
            wire = wire.header("accept", "text/event-stream");
        }
        match &endpoint.protocol {
            Protocol::OpenAiChat(_) => {
                if let Some(key) = &endpoint.api_key {
                    wire = wire.header("authorization", format!("Bearer {key}"));
                }
            }
            Protocol::AnthropicMessages(options) => {
                if let Some(key) = &endpoint.api_key {
                    wire = wire.header("x-api-key", key.clone());
                }
                wire = wire.header("anthropic-version", options.version.clone());
                if !betas.is_empty() {
                    wire = wire.header("anthropic-beta", betas.join(","));
                }
            }
        }
        for (name, value) in &endpoint.headers {
            wire = wire.header(name.clone(), value.clone());
        }
        Ok(wire)
    }

    fn tag(&self, error: ModelError) -> ModelError {
        let error = if error.provider.is_none() {
            error.for_model(self.endpoint.name.clone(), self.model.clone())
        } else {
            error
        };
        match &self.endpoint.api_key {
            Some(key) => error.redact(&[key.as_str()]),
            None => error,
        }
    }

    async fn call(&self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let mut warnings = Vec::new();
        let wire = self.wire(request, false, &mut warnings)?;
        let body = self.transport.send(&wire).await?;
        let mut response = match &self.endpoint.protocol {
            Protocol::OpenAiChat(quirks) => {
                openai_chat::decode(&body, &self.endpoint.name, quirks.think_tags)?
            }
            Protocol::AnthropicMessages(_) => {
                anthropic_messages::decode(&body, &self.endpoint.name)?
            }
        };
        warnings.append(&mut response.warnings);
        response.warnings = warnings;
        Ok(response)
    }

    async fn call_streaming(
        &self,
        request: &ModelRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ModelResponse, ModelError> {
        let mut warnings = Vec::new();
        let wire = self.wire(request, true, &mut warnings)?;
        let mut body = self.transport.open(&wire).await?;
        let mut parser = SseParser::new();
        let mut decoder = match &self.endpoint.protocol {
            Protocol::OpenAiChat(quirks) => {
                Decoder::OpenAi(ChunkDecoder::new(quirks.think_tags, &self.endpoint.name))
            }
            Protocol::AnthropicMessages(_) => Decoder::Anthropic(EventDecoder::default()),
        };
        let mut assembler = Assembler::new();
        let streaming = |error: ModelError, emitted: bool| {
            error
                .with_dispatch(Dispatch::Streaming)
                .with_emitted(emitted)
        };
        'read: while let Some(chunk) = body
            .next_chunk()
            .await
            .map_err(|e| streaming(e, assembler.emitted_deltas()))?
        {
            for event in parser.feed(&chunk) {
                let items = decoder
                    .on_event(&event)
                    .map_err(|e| streaming(e, assembler.emitted_deltas()))?;
                for item in items {
                    if let Some(delta) = assembler.push(item) {
                        on_delta(delta);
                    }
                }
                if decoder.is_done() {
                    break 'read;
                }
            }
        }
        let model = decoder.model().map(str::to_string);
        let stop_sequence = decoder.stop_sequence().map(str::to_string);
        let mut response = assembler.finish()?;
        let metadata = &mut response.message.metadata;
        metadata.provider = Some(self.endpoint.name.clone());
        metadata.model = model;
        if let Some(sequence) = stop_sequence {
            metadata
                .other
                .insert("stop_sequence".into(), Value::String(sequence));
        }
        warnings.append(&mut response.warnings);
        response.warnings = warnings;
        Ok(response)
    }
}

/// The stream decoder of an endpoint's protocol.
enum Decoder {
    OpenAi(ChunkDecoder),
    Anthropic(EventDecoder),
}

impl Decoder {
    fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamItem>, ModelError> {
        match self {
            Self::OpenAi(decoder) => decoder.on_event(event),
            Self::Anthropic(decoder) => decoder.on_event(event),
        }
    }

    fn is_done(&self) -> bool {
        match self {
            Self::OpenAi(decoder) => decoder.is_done(),
            Self::Anthropic(decoder) => decoder.is_done(),
        }
    }

    fn model(&self) -> Option<&str> {
        match self {
            Self::OpenAi(decoder) => decoder.model(),
            Self::Anthropic(decoder) => decoder.model(),
        }
    }

    fn stop_sequence(&self) -> Option<&str> {
        match self {
            Self::OpenAi(_) => None,
            Self::Anthropic(decoder) => decoder.stop_sequence(),
        }
    }
}

impl Model for ProviderModel {
    fn provider(&self) -> &str {
        &self.endpoint.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.endpoint.capabilities.clone()
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move { self.call(request).await.map_err(|e| self.tag(e)) })
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            self.call_streaming(request, on_delta)
                .await
                .map_err(|e| self.tag(e))
        })
    }
}

/// The JSON body `model` would send for `request`, for inspection and
/// golden tests. Warnings are what would be dropped. Headers (such as
/// Anthropic beta flags) are not shown.
pub fn preview_request(
    endpoint: &Endpoint,
    model: &str,
    request: &ModelRequest,
    stream: bool,
) -> Result<(Value, Vec<Warning>), ModelError> {
    let mut warnings = Vec::new();
    let body = match &endpoint.protocol {
        Protocol::OpenAiChat(quirks) => openai_chat::encode(
            request,
            model,
            &endpoint.name,
            quirks,
            stream,
            &mut warnings,
        )?,
        Protocol::AnthropicMessages(options) => {
            anthropic_messages::encode(
                request,
                model,
                &endpoint.name,
                options,
                stream,
                &mut warnings,
            )?
            .body
        }
    };
    Ok((body, warnings))
}

/// An embedding model reached over HTTP: the OpenAI-compatible
/// `POST {base_url}/embeddings` (OpenAI, Gemini's OpenAI-compatible
/// endpoint, Ollama, LM Studio, vLLM, OpenRouter). Design 0008 §16.1.
///
/// ```no_run
/// use agentbuild_providers::{Embedder, Endpoint, ProviderEmbedder};
///
/// # async fn demo() -> Result<(), agentbuild_providers::ModelError> {
/// let embedder = ProviderEmbedder::new(Endpoint::ollama(), "nomic-embed-text")?;
/// let vectors = embedder.embed(&["Severe penicillin allergy".to_string()]).await?;
/// println!("{} dimensions", vectors[0].len());
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct ProviderEmbedder {
    endpoint: Endpoint,
    model: String,
    dimensions: Option<u32>,
    transport: Transport,
}

impl ProviderEmbedder {
    /// An embedder with default timeouts. An Anthropic endpoint has no
    /// embeddings API: `invalid_request`.
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Result<Self, ModelError> {
        Self::with_config(endpoint, model, HttpConfig::default())
    }

    /// An embedder with explicit timeouts.
    pub fn with_config(
        endpoint: Endpoint,
        model: impl Into<String>,
        config: HttpConfig,
    ) -> Result<Self, ModelError> {
        if let Protocol::AnthropicMessages(_) = endpoint.protocol {
            return Err(ModelError::new(
                crate::ModelErrorKind::InvalidRequest,
                format!(
                    "endpoint `{}` speaks the Anthropic Messages API, which has no embeddings; \
                     use an OpenAI-compatible endpoint",
                    endpoint.name
                ),
            ));
        }
        let transport = endpoint.transport(config)?;
        Ok(Self {
            endpoint,
            model: model.into(),
            dimensions: None,
            transport,
        })
    }

    /// Asks for vectors of `n` dimensions (models that support shortening,
    /// such as OpenAI's `text-embedding-3-*`).
    pub fn dimensions(mut self, n: u32) -> Self {
        self.dimensions = Some(n);
        self
    }

    /// The endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    async fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ModelError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut body = serde_json::json!({"model": self.model, "input": texts});
        if let Some(n) = self.dimensions {
            body["dimensions"] = n.into();
        }
        let mut wire = WireRequest::post(
            format!("{}/embeddings", self.endpoint.base_url),
            serde_json::to_vec(&body).expect("JSON values serialize"),
        )
        .header("content-type", "application/json");
        if let Some(key) = &self.endpoint.api_key {
            wire = wire.header("authorization", format!("Bearer {key}"));
        }
        for (name, value) in &self.endpoint.headers {
            wire = wire.header(name.clone(), value.clone());
        }
        let bytes = self.transport.send(&wire).await?;
        decode_embeddings(&bytes, texts.len())
    }
}

/// The vectors of an embeddings response, ordered by `index`.
fn decode_embeddings(bytes: &[u8], expected: usize) -> Result<Vec<Vec<f32>>, ModelError> {
    #[derive(serde::Deserialize)]
    struct Response {
        data: Vec<Item>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        #[serde(default)]
        index: Option<usize>,
        embedding: Vec<f32>,
    }
    let invalid = |why: String| ModelError::new(crate::ModelErrorKind::InvalidResponse, why);
    let response: Response = serde_json::from_slice(bytes)
        .map_err(|err| invalid(format!("unreadable embeddings response: {err}")))?;
    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (position, item) in response.data.into_iter().enumerate() {
        let index = item.index.unwrap_or(position);
        match vectors.get_mut(index) {
            Some(slot @ None) => *slot = Some(item.embedding),
            _ => return Err(invalid(format!("unexpected embedding index {index}"))),
        }
    }
    vectors
        .into_iter()
        .enumerate()
        .map(|(index, vector)| {
            vector
                .filter(|v| !v.is_empty())
                .ok_or_else(|| invalid(format!("no embedding for input {index}")))
        })
        .collect()
}

impl crate::embed::Embedder for ProviderEmbedder {
    fn provider(&self) -> &str {
        &self.endpoint.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, ModelError>> {
        Box::pin(async move {
            self.call(texts).await.map_err(|error| {
                let error = if error.provider.is_none() {
                    error.for_model(self.endpoint.name.clone(), self.model.clone())
                } else {
                    error
                };
                match &self.endpoint.api_key {
                    Some(key) => error.redact(&[key.as_str()]),
                    None => error,
                }
            })
        })
    }
}

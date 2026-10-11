//! Endpoints and `ProviderModel`: a protocol, an endpoint and the HTTP
//! transport, combined into a [`Model`] (design 0006 §3, §4).

use serde_json::Value;

use crate::error::{Dispatch, ModelError};
use crate::model::{BoxFuture, DeltaSink, Model};
use crate::openai_chat::{self, ChunkDecoder, OpenAiQuirks};
use crate::request::{Capabilities, ModelRequest, ModelResponse};
use crate::sse::SseParser;
use crate::stream::{Assembler, Warning};
use crate::transport::{HttpConfig, Transport, WireRequest};

/// The wire format an endpoint speaks.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Protocol {
    /// OpenAI chat completions (`POST {base_url}/chat/completions`).
    OpenAiChat(OpenAiQuirks),
}

/// Where and how to reach a provider. Plain data; presets cover common
/// servers. The provider is never guessed from a model name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Endpoint {
    /// Short name, used as `provider` in messages and errors (e.g. `ollama`).
    pub name: String,
    /// Wire format.
    pub protocol: Protocol,
    /// Base URL without a trailing slash, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// Sent as `Authorization: Bearer …` when set. Redacted from errors.
    pub api_key: Option<String>,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// What models behind this endpoint support (unknown = `None`).
    pub capabilities: Capabilities,
}

impl Endpoint {
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
        let transport = Transport::new(config)?.redacting(endpoint.api_key.clone());
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
        let Protocol::OpenAiChat(quirks) = &self.endpoint.protocol;
        let body = openai_chat::encode(
            request,
            &self.model,
            &self.endpoint.name,
            quirks,
            stream,
            warnings,
        )?;
        let mut wire = WireRequest::post(
            format!("{}/chat/completions", self.endpoint.base_url),
            serde_json::to_vec(&body).expect("JSON values serialize"),
        )
        .header("content-type", "application/json");
        if stream {
            wire = wire.header("accept", "text/event-stream");
        }
        if let Some(key) = &self.endpoint.api_key {
            wire = wire.header("authorization", format!("Bearer {key}"));
        }
        for (name, value) in &self.endpoint.headers {
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
        let mut response = openai_chat::decode(&body, &self.endpoint.name)?;
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
        let mut decoder = ChunkDecoder::default();
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
        let mut response = assembler.finish()?;
        let metadata = &mut response.message.metadata;
        metadata.provider = Some(self.endpoint.name.clone());
        metadata.model = model;
        warnings.append(&mut response.warnings);
        response.warnings = warnings;
        Ok(response)
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
/// golden tests. Warnings are what would be dropped.
pub fn preview_request(
    endpoint: &Endpoint,
    model: &str,
    request: &ModelRequest,
    stream: bool,
) -> Result<(Value, Vec<Warning>), ModelError> {
    let Protocol::OpenAiChat(quirks) = &endpoint.protocol;
    let mut warnings = Vec::new();
    let body = openai_chat::encode(
        request,
        model,
        &endpoint.name,
        quirks,
        stream,
        &mut warnings,
    )?;
    Ok((body, warnings))
}

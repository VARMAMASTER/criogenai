//! Google Vertex AI (design 0006 §19): Gemini and Google's other hosted
//! models over the OpenAI-compatible endpoint, Claude over `rawPredict`,
//! with a Google access token that [`VertexModel`] obtains and renews.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::anthropic_messages::AnthropicOptions;
use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::model::{BoxFuture, DeltaSink, Model};
use crate::openai_chat::OpenAiQuirks;
use crate::provider::{Endpoint, Protocol, ProviderModel, is_loopback_host, url_host};
use crate::request::{Capabilities, ModelRequest, ModelResponse};
use crate::transport::{HttpConfig, Transport, WireRequest};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const METADATA_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
const MAX_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
const DEFAULT_LIFETIME: Duration = Duration::from_secs(50 * 60);
/// Largest credentials file read.
const MAX_FILE: u64 = 64 * 1024;

/// Which Vertex API a model is called with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum VertexApi {
    /// `claude-…` models over Claude's path, the rest OpenAI-compatible.
    #[default]
    Auto,
    /// The OpenAI-compatible Chat Completions endpoint.
    Chat,
    /// Claude's `rawPredict` (Anthropic Messages).
    Claude,
}

impl VertexApi {
    /// `auto`, `chat` or `claude`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "auto" => Self::Auto,
            "chat" => Self::Chat,
            "claude" => Self::Claude,
            _ => return None,
        })
    }
}

#[derive(Clone)]
enum Kind {
    Token(String),
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
    MetadataServer,
}

/// How [`VertexModel`] gets its access token. `Debug` shows no secret.
#[derive(Clone)]
pub struct VertexCredential(Kind);

impl std::fmt::Debug for VertexCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            Kind::Token(_) => "VertexCredential::Token([redacted])",
            Kind::AuthorizedUser { .. } => "VertexCredential::AuthorizedUser([redacted])",
            Kind::MetadataServer => "VertexCredential::MetadataServer",
        })
    }
}

impl VertexCredential {
    /// An access token obtained elsewhere (`gcloud auth print-access-token`).
    /// Used as is; not renewed.
    pub fn token(token: impl Into<String>) -> Self {
        Self(Kind::Token(token.into().trim().to_string()))
    }

    /// A user's refresh token (what `gcloud auth application-default login`
    /// saves); access tokens are obtained from it and renewed.
    pub fn authorized_user(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        refresh_token: impl Into<String>,
    ) -> Self {
        Self(Kind::AuthorizedUser {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            refresh_token: refresh_token.into(),
        })
    }

    /// The service account of the Google Cloud machine this runs on (Cloud
    /// Run, GKE, Compute Engine), from its metadata server.
    pub fn metadata_server() -> Self {
        Self(Kind::MetadataServer)
    }

    /// A credentials file: an `authorized_user` file is used; a
    /// `service_account` key file is refused (it needs signing, not built
    /// yet). Returns the credential and the file's quota project, if any.
    pub fn from_file(path: &std::path::Path) -> Result<(Self, Option<String>), ModelError> {
        use std::io::Read;
        let invalid = |why: String| {
            ModelError::new(ModelErrorKind::Auth, why).with_dispatch(Dispatch::NotSent)
        };
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(MAX_FILE).read_to_string(&mut text))
            .map_err(|err| invalid(format!("cannot read the Google credentials file: {err}")))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|_| invalid("the Google credentials file is not JSON".into()))?;
        let field = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_string);
        let project = field("quota_project_id");
        match field("type").as_deref() {
            Some("authorized_user") => {
                let (Some(id), Some(secret), Some(refresh)) = (
                    field("client_id"),
                    field("client_secret"),
                    field("refresh_token"),
                ) else {
                    return Err(invalid(
                        "the Google credentials file lacks client_id, client_secret or \
                         refresh_token"
                            .into(),
                    ));
                };
                Ok((Self::authorized_user(id, secret, refresh), project))
            }
            Some("service_account") => Err(invalid(
                "service-account key files are not supported yet (they need RS256 signing): \
                 run on Google Cloud (the metadata server), use `gcloud auth \
                 application-default login`, or pass a token"
                    .into(),
            )),
            other => Err(invalid(format!(
                "unsupported Google credentials type {}",
                other.unwrap_or("(none)")
            ))),
        }
    }

    /// `GOOGLE_APPLICATION_CREDENTIALS`, else gcloud's default file; `None`
    /// when there is no file (the caller may try the metadata server).
    pub fn from_env() -> Result<Option<(Self, Option<String>)>, ModelError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(path) = var("GOOGLE_APPLICATION_CREDENTIALS") {
            return Self::from_file(std::path::Path::new(&path)).map(Some);
        }
        let default = if cfg!(windows) {
            var("APPDATA").map(|dir| {
                std::path::Path::new(&dir).join("gcloud/application_default_credentials.json")
            })
        } else {
            var("HOME").map(|home| {
                std::path::Path::new(&home)
                    .join(".config/gcloud/application_default_credentials.json")
            })
        };
        match default.filter(|path| path.is_file()) {
            Some(path) => Self::from_file(&path).map(Some),
            None => Ok(None),
        }
    }

    fn renews(&self) -> bool {
        !matches!(self.0, Kind::Token(_))
    }
}

/// Lowercase letters, digits and dashes (project ids, locations).
fn plain_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The API base for `location`: `global` has no location prefix.
fn host(location: &str) -> String {
    if location == "global" {
        "https://aiplatform.googleapis.com".into()
    } else {
        format!("https://{location}-aiplatform.googleapis.com")
    }
}

#[derive(Clone)]
struct Session {
    model: Arc<ProviderModel>,
    renew_at: Option<SystemTime>,
}

/// A model on Google Vertex AI.
///
/// ```no_run
/// use criogen_providers::{Message, Model, ModelRequest, VertexModel};
///
/// # async fn demo() -> Result<(), criogen_providers::ModelError> {
/// // GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, and gcloud's credentials
/// // or the metadata server on Google Cloud
/// let model = VertexModel::from_env("google/gemini-3.6-flash")?;
/// let response = model.invoke(&ModelRequest::new(vec![Message::user("hi")])).await?;
/// println!("{}", response.message.text());
/// # Ok(()) }
/// ```
pub struct VertexModel {
    model: String,
    project: String,
    location: String,
    api: VertexApi,
    credential: VertexCredential,
    host: String,
    token_url: String,
    metadata_url: String,
    config: HttpConfig,
    session: Mutex<Option<Session>>,
}

impl std::fmt::Debug for VertexModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexModel")
            .field("model", &self.model)
            .field("project", &self.project)
            .field("location", &self.location)
            .field("api", &self.api)
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

impl VertexModel {
    /// `model` in `project` at `location` (`global`, `us-central1`,
    /// `europe-west4`, …) with this credential. Project and location must
    /// be plain names (lowercase letters, digits, dashes).
    pub fn new(
        project: &str,
        location: &str,
        model: impl Into<String>,
        credential: VertexCredential,
    ) -> Result<Self, ModelError> {
        let model = model.into();
        for (what, value) in [("project", project), ("location", location)] {
            if !plain_name(value) {
                return Err(ModelError::new(
                    ModelErrorKind::InvalidRequest,
                    format!("`{value}` is not a Google Cloud {what} name"),
                )
                .with_dispatch(Dispatch::NotSent)
                .for_model("vertex", model));
            }
        }
        Ok(Self {
            host: host(location),
            model,
            project: project.to_string(),
            location: location.to_string(),
            api: VertexApi::Auto,
            credential,
            token_url: TOKEN_URL.into(),
            metadata_url: METADATA_URL.into(),
            config: HttpConfig::default(),
            session: Mutex::new(None),
        })
    }

    /// Everything from the environment: `GOOGLE_CLOUD_PROJECT` (or the
    /// credentials file's quota project), `GOOGLE_CLOUD_LOCATION`, and
    /// [`VertexCredential::from_env`], else the metadata server.
    pub fn from_env(model: impl Into<String>) -> Result<Self, ModelError> {
        Self::from_parts(None, None, model, None)
    }

    /// What is given, the rest from the environment (what the SDKs'
    /// `vertex:` models use). Messages name the variables, never a value.
    pub fn from_parts(
        project: Option<&str>,
        location: Option<&str>,
        model: impl Into<String>,
        credential: Option<VertexCredential>,
    ) -> Result<Self, ModelError> {
        let model = model.into();
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let missing = |what: &str| {
            ModelError::new(ModelErrorKind::Auth, what)
                .with_dispatch(Dispatch::NotSent)
                .for_model("vertex", model.clone())
        };
        let (credential, file_project) = match credential {
            Some(credential) => (credential, None),
            None => match VertexCredential::from_env()
                .map_err(|e| e.for_model("vertex", model.clone()))?
            {
                Some((credential, project)) => (credential, project),
                None => (VertexCredential::metadata_server(), None),
            },
        };
        let given = |v: Option<&str>| {
            v.map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let project = given(project)
            .or_else(|| var("GOOGLE_CLOUD_PROJECT"))
            .or(file_project)
            .ok_or_else(|| {
                missing("no Google Cloud project: pass `project`, or set GOOGLE_CLOUD_PROJECT")
            })?;
        let location = given(location).or_else(|| var("GOOGLE_CLOUD_LOCATION")).ok_or_else(|| {
            missing(
                "no Vertex AI location: pass `location` (e.g. `global`, `europe-west4`), or set \
                 GOOGLE_CLOUD_LOCATION; there is no default, it decides where data is processed",
            )
        })?;
        Self::new(&project, &location, model, credential)
    }

    /// Forces an API instead of choosing by model name.
    pub fn with_api(mut self, api: VertexApi) -> Self {
        self.api = api;
        self
    }

    /// Another API host (a private endpoint; tests use a local server).
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into().trim_end_matches('/').to_string();
        self
    }

    /// Another OAuth token endpoint (tests).
    pub fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = url.into();
        self
    }

    /// Another metadata-server URL (tests).
    pub fn with_metadata_url(mut self, url: impl Into<String>) -> Self {
        self.metadata_url = url.into();
        self
    }

    /// Timeouts and limits.
    pub fn with_config(mut self, config: HttpConfig) -> Self {
        self.config = config;
        self
    }

    fn uses_claude(&self) -> bool {
        match self.api {
            VertexApi::Claude => true,
            VertexApi::Chat => false,
            VertexApi::Auto => self.model.starts_with("claude"),
        }
    }

    /// The endpoint for a bearer token.
    fn endpoint(&self, token: String) -> Result<Endpoint, ModelError> {
        let base = format!(
            "{}/v1/projects/{}/locations/{}",
            self.host, self.project, self.location
        );
        if self.uses_claude() {
            if !self
                .model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '@'))
            {
                return Err(ModelError::new(
                    ModelErrorKind::InvalidRequest,
                    format!("`{}` is not a Claude model name on Vertex AI", self.model),
                )
                .with_dispatch(Dispatch::NotSent));
            }
            let mut endpoint = Endpoint::anthropic_compatible(
                "vertex",
                format!("{base}/publishers/anthropic/models"),
                Some(token),
            );
            if let Protocol::AnthropicMessages(options) = &mut endpoint.protocol {
                *options = AnthropicOptions::default().vertex(true);
            }
            let caps = &mut endpoint.capabilities;
            caps.tools = Some(true);
            caps.parallel_tool_calls = Some(true);
            caps.vision = Some(true);
            caps.reasoning = Some(true);
            caps.tool_result_images = Some(true);
            Ok(endpoint)
        } else {
            let mut endpoint = Endpoint::openai_compatible(
                "vertex",
                format!("{base}/endpoints/openapi"),
                Some(token),
            );
            if let Protocol::OpenAiChat(quirks) = &mut endpoint.protocol {
                *quirks = OpenAiQuirks::default();
                quirks.thought_signatures = true;
            }
            endpoint.capabilities.tools = Some(true);
            Ok(endpoint)
        }
    }

    async fn current(&self, renew: bool) -> Result<Arc<ProviderModel>, ModelError> {
        let now = criogen_rt::system_now();
        if let Some(session) = self.session.lock().expect("session lock").clone()
            && (!renew || !self.credential.renews())
            && session.renew_at.is_none_or(|at| now < at)
        {
            return Ok(session.model);
        }
        let (token, renew_at) = match &self.credential.0 {
            Kind::Token(token) => (token.clone(), None),
            Kind::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
            } => {
                if let Some(("http", host)) = url_host(&self.token_url)
                    && !is_loopback_host(host)
                {
                    return Err(ModelError::new(
                        ModelErrorKind::InvalidRequest,
                        format!("refusing to send a refresh token over plain http to `{host}`"),
                    )
                    .with_dispatch(Dispatch::NotSent));
                }
                let body = crate::azure::form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id),
                    ("client_secret", client_secret),
                    ("refresh_token", refresh_token),
                ]);
                let request = WireRequest::post(self.token_url.clone(), body.into_bytes())
                    .header("content-type", "application/x-www-form-urlencoded");
                self.exchange(request, &[client_secret.clone(), refresh_token.clone()])
                    .await?
            }
            Kind::MetadataServer => {
                let request =
                    WireRequest::get(self.metadata_url.clone()).header("metadata-flavor", "Google");
                self.exchange(request, &[]).await?
            }
        };
        let endpoint = self.endpoint(token)?;
        let model = Arc::new(ProviderModel::with_config(
            endpoint,
            &self.model,
            self.config.clone(),
        )?);
        *self.session.lock().expect("session lock") = Some(Session {
            model: model.clone(),
            renew_at,
        });
        Ok(model)
    }

    async fn exchange(
        &self,
        request: WireRequest,
        secrets: &[String],
    ) -> Result<(String, Option<SystemTime>), ModelError> {
        let mut config = self.config.clone();
        if matches!(self.credential.0, Kind::MetadataServer) {
            // Off Google Cloud the metadata host does not exist: fail fast.
            config = config.connect_timeout(Duration::from_secs(3));
        }
        let transport = Transport::new(config)?.redacting(secrets.iter().cloned());
        let body = transport.send(&request).await.map_err(|error| {
            let mut error = error.redact(&secrets.iter().map(String::as_str).collect::<Vec<_>>());
            if matches!(
                error.kind,
                ModelErrorKind::InvalidRequest | ModelErrorKind::NotFound | ModelErrorKind::Network
            ) {
                error.kind = ModelErrorKind::Auth;
            }
            error.message = match self.credential.0 {
                Kind::MetadataServer => format!(
                    "no Google credentials: no credentials file, and the metadata server (only \
                     on Google Cloud) did not answer: {}",
                    error.message
                ),
                _ => format!("Google did not issue a token: {}", error.message),
            };
            error.for_model("vertex", self.model.clone())
        })?;
        let reply: Value = serde_json::from_slice(&body).map_err(|_| {
            ModelError::new(
                ModelErrorKind::InvalidResponse,
                "Google's token reply is not JSON",
            )
        })?;
        let token = reply
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                ModelError::new(
                    ModelErrorKind::InvalidResponse,
                    "Google's token reply has no access_token",
                )
            })?;
        let lifetime = reply
            .get("expires_in")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .map_or(DEFAULT_LIFETIME, Duration::from_secs)
            .min(MAX_LIFETIME);
        let renew_at = criogen_rt::system_now() + lifetime.saturating_sub(RENEW_BEFORE);
        Ok((token.to_string(), Some(renew_at)))
    }

    async fn call(&self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let model = self.current(false).await?;
        match model.invoke(request).await {
            Err(error) if error.kind == ModelErrorKind::Auth && self.credential.renews() => {
                self.current(true).await?.invoke(request).await
            }
            result => result,
        }
    }

    async fn call_streaming(
        &self,
        request: &ModelRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ModelResponse, ModelError> {
        let model = self.current(false).await?;
        match model.stream(request, &mut *on_delta).await {
            Err(error)
                if error.kind == ModelErrorKind::Auth
                    && !error.emitted_deltas
                    && self.credential.renews() =>
            {
                self.current(true).await?.stream(request, on_delta).await
            }
            result => result,
        }
    }
}

impl Model for VertexModel {
    fn provider(&self) -> &str {
        "vertex"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.endpoint(String::new())
            .map(|e| e.capabilities)
            .unwrap_or_default()
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(self.call(request))
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(self.call_streaming(request, on_delta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_names() {
        assert_eq!(host("global"), "https://aiplatform.googleapis.com");
        assert_eq!(
            host("europe-west4"),
            "https://europe-west4-aiplatform.googleapis.com"
        );
        assert!(plain_name("my-project-123"));
        for bad in ["", "My-Project", "a.b", "a/b", "evil.com#"] {
            assert!(!plain_name(bad), "{bad}");
        }
    }
}

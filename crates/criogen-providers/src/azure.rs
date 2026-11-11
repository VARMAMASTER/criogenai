//! Azure OpenAI and Microsoft Foundry with Entra ID (design 0006 §17): a
//! model whose bearer token [`AzureModel`] obtains from a service
//! principal or a managed identity and renews before it expires. With an
//! API key it is a plain [`Endpoint::azure`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::model::{BoxFuture, DeltaSink, Model};
use crate::provider::{Endpoint, ProviderModel, azure_base_url, is_loopback_host, url_host};
use crate::request::{Capabilities, ModelRequest, ModelResponse};
use crate::transport::{HttpConfig, Transport, WireRequest};

/// Where Entra ID tokens are issued (the public cloud).
const AUTHORITY: &str = "https://login.microsoftonline.com";
/// The instance metadata service of Azure VMs, for managed identities.
const IMDS: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
/// The default token scope for Azure OpenAI.
const SCOPE: &str = "https://cognitiveservices.azure.com/.default";
/// Renew this long before the token expires.
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
/// Never keep a token longer than this, whatever it says.
const MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// When a token states no lifetime.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(50 * 60);

/// Which Azure API a deployment is called with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum AzureApi {
    /// The Responses API (OpenAI models; reasoning with tools).
    #[default]
    Responses,
    /// Chat Completions (Foundry's models from other makers).
    Chat,
}

#[derive(Clone)]
enum Kind {
    ApiKey(String),
    ClientSecret {
        tenant: String,
        client_id: String,
        secret: String,
    },
    ManagedIdentity {
        client_id: Option<String>,
    },
    Token(String),
}

/// How [`AzureModel`] authenticates. No secret is ever shown by `Debug`.
#[derive(Clone)]
pub struct AzureCredential(Kind);

impl std::fmt::Debug for AzureCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match &self.0 {
            Kind::ApiKey(_) => "AzureCredential::ApiKey([redacted])",
            Kind::ClientSecret { .. } => "AzureCredential::ClientSecret([redacted])",
            Kind::ManagedIdentity { .. } => "AzureCredential::ManagedIdentity",
            Kind::Token(_) => "AzureCredential::Token([redacted])",
        })
    }
}

impl AzureCredential {
    /// The resource's API key (sent as `api-key`).
    pub fn api_key(key: impl Into<String>) -> Self {
        Self(Kind::ApiKey(key.into().trim().to_string()))
    }

    /// A service principal: Entra ID tokens from a client secret
    /// (`grant_type=client_credentials`).
    pub fn client_secret(
        tenant_id: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self(Kind::ClientSecret {
            tenant: tenant_id.into().trim().to_string(),
            client_id: client_id.into().trim().to_string(),
            secret: client_secret.into(),
        })
    }

    /// The managed identity of the Azure machine or app this runs on (a
    /// user-assigned one when `client_id` is given). No secret leaves the
    /// process.
    pub fn managed_identity(client_id: Option<String>) -> Self {
        Self(Kind::ManagedIdentity {
            client_id: client_id.filter(|c| !c.trim().is_empty()),
        })
    }

    /// A bearer token obtained elsewhere (the Azure CLI, a sidecar). Used
    /// as is; it is not renewed.
    pub fn token(bearer: impl Into<String>) -> Self {
        Self(Kind::Token(bearer.into().trim().to_string()))
    }

    /// Credentials from the environment: `AZURE_OPENAI_API_KEY`, else
    /// `AZURE_TENANT_ID` + `AZURE_CLIENT_ID` + `AZURE_CLIENT_SECRET`, else
    /// `AZURE_USE_MANAGED_IDENTITY=1` (with `AZURE_CLIENT_ID` for a
    /// user-assigned identity).
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        if let Some(key) = var("AZURE_OPENAI_API_KEY") {
            return Some(Self::api_key(key));
        }
        if let (Some(tenant), Some(client), Some(secret)) = (
            var("AZURE_TENANT_ID"),
            var("AZURE_CLIENT_ID"),
            var("AZURE_CLIENT_SECRET"),
        ) {
            return Some(Self::client_secret(tenant, client, secret));
        }
        var("AZURE_USE_MANAGED_IDENTITY")
            .filter(|v| v == "1" || v == "true")
            .map(|_| Self::managed_identity(var("AZURE_CLIENT_ID")))
    }

    fn renews(&self) -> bool {
        matches!(
            self.0,
            Kind::ClientSecret { .. } | Kind::ManagedIdentity { .. }
        )
    }
}

#[derive(Clone)]
struct Session {
    model: Arc<ProviderModel>,
    /// `None`: never renewed (a key or a given token).
    renew_at: Option<SystemTime>,
}

/// A deployment on Azure OpenAI or Microsoft Foundry.
///
/// ```no_run
/// use criogen_providers::{AzureCredential, AzureModel, Message, Model, ModelRequest};
///
/// # async fn demo() -> Result<(), criogen_providers::ModelError> {
/// let credential = AzureCredential::client_secret("tenant-id", "client-id", "secret");
/// let model = AzureModel::new("my-resource", "gpt-5-mini", credential);
/// let response = model.invoke(&ModelRequest::new(vec![Message::user("hi")])).await?;
/// println!("{}", response.message.text());
/// # Ok(()) }
/// ```
pub struct AzureModel {
    deployment: String,
    base_url: String,
    api: AzureApi,
    credential: AzureCredential,
    scope: String,
    authority: String,
    identity_endpoint: Option<(String, Option<String>)>,
    config: HttpConfig,
    session: Mutex<Option<Session>>,
}

impl std::fmt::Debug for AzureModel {
    /// No secret or token is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureModel")
            .field("deployment", &self.deployment)
            .field("base_url", &self.base_url)
            .field("api", &self.api)
            .field("credential", &self.credential)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl AzureModel {
    /// A deployment of `resource_or_url` (a resource name or its URL), with
    /// this credential, over the Responses API.
    pub fn new(
        resource_or_url: &str,
        deployment: impl Into<String>,
        credential: AzureCredential,
    ) -> Self {
        Self {
            deployment: deployment.into(),
            base_url: azure_base_url(resource_or_url),
            api: AzureApi::Responses,
            credential,
            scope: SCOPE.to_string(),
            authority: AUTHORITY.to_string(),
            identity_endpoint: None,
            config: HttpConfig::default(),
            session: Mutex::new(None),
        }
    }

    /// Credentials from the environment: `AZURE_OPENAI_ENDPOINT` (resource
    /// name or URL) and then `AZURE_OPENAI_API_KEY`, or `AZURE_TENANT_ID` +
    /// `AZURE_CLIENT_ID` + `AZURE_CLIENT_SECRET`, or
    /// `AZURE_USE_MANAGED_IDENTITY=1` (with `AZURE_CLIENT_ID` for a
    /// user-assigned identity). The message names the variables, never a
    /// value.
    pub fn from_env(deployment: impl Into<String>) -> Result<Self, ModelError> {
        Self::from_parts(None, deployment, None)
    }

    /// A deployment from what is given, the rest from the environment (what
    /// the SDKs' `azure:` models use): `resource_or_url`, else
    /// `AZURE_OPENAI_ENDPOINT`; `credential`, else
    /// [`AzureCredential::from_env`]. The message names the variables, never
    /// a value.
    pub fn from_parts(
        resource_or_url: Option<&str>,
        deployment: impl Into<String>,
        credential: Option<AzureCredential>,
    ) -> Result<Self, ModelError> {
        let deployment = deployment.into();
        let missing = |what: &str| {
            ModelError::new(ModelErrorKind::Auth, what)
                .with_dispatch(Dispatch::NotSent)
                .for_model("azure", deployment.clone())
        };
        let resource = match resource_or_url.map(str::trim).filter(|r| !r.is_empty()) {
            Some(resource) => resource.to_string(),
            None => std::env::var("AZURE_OPENAI_ENDPOINT")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    missing(
                        "no Azure endpoint: set AZURE_OPENAI_ENDPOINT to the resource name or URL",
                    )
                })?,
        };
        let credential = credential
            .or_else(AzureCredential::from_env)
            .ok_or_else(|| {
                missing(
                    "no Azure credentials: set AZURE_OPENAI_API_KEY, or AZURE_TENANT_ID, \
                 AZURE_CLIENT_ID and AZURE_CLIENT_SECRET, or AZURE_USE_MANAGED_IDENTITY=1",
                )
            })?;
        Ok(Self::new(&resource, deployment, credential))
    }

    /// Calls the deployment over Chat Completions instead of Responses.
    pub fn with_api(mut self, api: AzureApi) -> Self {
        self.api = api;
        self
    }

    /// The token scope (default `https://cognitiveservices.azure.com/.default`;
    /// Foundry also accepts `https://ai.azure.com/.default`).
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// Another Entra ID authority (a sovereign cloud; tests use a local
    /// server). https only, except on this machine.
    pub fn with_authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into().trim_end_matches('/').to_string();
        self
    }

    /// Another managed-identity endpoint: App Service's `IDENTITY_ENDPOINT`
    /// with its `IDENTITY_HEADER` secret, or a local server in tests. Without
    /// it, `IDENTITY_ENDPOINT` / `IDENTITY_HEADER` from the environment are
    /// used when set, else the VM metadata service.
    pub fn with_identity_endpoint(
        mut self,
        url: impl Into<String>,
        header: Option<String>,
    ) -> Self {
        self.identity_endpoint = Some((url.into(), header));
        self
    }

    /// Timeouts and limits, for the token service and the model.
    pub fn with_config(mut self, config: HttpConfig) -> Self {
        self.config = config;
        self
    }

    /// The model endpoint for a credential value.
    fn model_for(
        &self,
        key: Option<String>,
        bearer: Option<String>,
    ) -> Result<ProviderModel, ModelError> {
        let placeholder = String::new();
        let mut endpoint = match self.api {
            AzureApi::Responses => Endpoint::azure(&self.base_url, placeholder),
            AzureApi::Chat => Endpoint::azure_chat(&self.base_url, placeholder),
        };
        match (key, bearer) {
            (Some(key), _) => endpoint.api_key = Some(key),
            (None, Some(token)) => {
                endpoint.api_key = Some(token);
                endpoint.key_header = None;
            }
            (None, None) => endpoint.api_key = None,
        }
        ProviderModel::with_config(endpoint, &self.deployment, self.config.clone())
    }

    /// The current model; obtains a token when there is none, when it is
    /// due, or when `renew` is set.
    async fn current(&self, renew: bool) -> Result<Arc<ProviderModel>, ModelError> {
        let now = criogen_rt::system_now();
        if let Some(session) = self.session.lock().expect("session lock").clone()
            && (!renew || !self.credential.renews())
            && session.renew_at.is_none_or(|at| now < at)
        {
            return Ok(session.model);
        }
        let session = match &self.credential.0 {
            Kind::ApiKey(key) => Session {
                model: Arc::new(self.model_for(Some(key.clone()), None)?),
                renew_at: None,
            },
            Kind::Token(token) => Session {
                model: Arc::new(self.model_for(None, Some(token.clone()))?),
                renew_at: None,
            },
            Kind::ClientSecret {
                tenant,
                client_id,
                secret,
            } => self.client_secret_token(tenant, client_id, secret).await?,
            Kind::ManagedIdentity { client_id } => {
                self.managed_identity_token(client_id.as_deref()).await?
            }
        };
        let model = session.model.clone();
        *self.session.lock().expect("session lock") = Some(session);
        Ok(model)
    }

    fn token_error(&self, message: impl Into<String>, kind: ModelErrorKind) -> ModelError {
        ModelError::new(kind, message.into()).for_model("azure", self.deployment.clone())
    }

    async fn client_secret_token(
        &self,
        tenant: &str,
        client_id: &str,
        secret: &str,
    ) -> Result<Session, ModelError> {
        if tenant.is_empty()
            || !tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            return Err(self
                .token_error(
                    "the Azure tenant id has characters a tenant id cannot have",
                    ModelErrorKind::InvalidRequest,
                )
                .with_dispatch(Dispatch::NotSent));
        }
        if let Some(("http", host)) = url_host(&self.authority)
            && !is_loopback_host(host)
        {
            return Err(self
                .token_error(
                    format!("refusing to send the Azure client secret over plain http to `{host}`"),
                    ModelErrorKind::InvalidRequest,
                )
                .with_dispatch(Dispatch::NotSent));
        }
        let body = form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", secret),
            ("scope", &self.scope),
        ]);
        let request = WireRequest::post(
            format!("{}/{tenant}/oauth2/v2.0/token", self.authority),
            body.into_bytes(),
        )
        .header("content-type", "application/x-www-form-urlencoded");
        self.exchange(request, &[secret.to_string()]).await
    }

    async fn managed_identity_token(&self, client_id: Option<&str>) -> Result<Session, ModelError> {
        let resource = self.scope.trim_end_matches("/.default");
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let (url, header) = match &self.identity_endpoint {
            Some((url, header)) => (url.clone(), header.clone()),
            None => match (env("IDENTITY_ENDPOINT"), env("IDENTITY_HEADER")) {
                (Some(url), Some(header)) => (url, Some(header)),
                _ => (IMDS.to_string(), None),
            },
        };
        // App Service's endpoint takes 2019-08-01 and a secret header; the
        // VM metadata service takes 2018-02-01 and `Metadata: true`.
        let version = if header.is_some() {
            "2019-08-01"
        } else {
            "2018-02-01"
        };
        let mut query = form(&[("api-version", version), ("resource", resource)]);
        if let Some(id) = client_id {
            query.push('&');
            query.push_str(&form(&[("client_id", id)]));
        }
        if header.is_some()
            && let Some((scheme, host)) = url_host(&url)
            && scheme == "http"
            && !is_loopback_host(host)
        {
            return Err(self
                .token_error(
                    format!("refusing to send the identity header over plain http to `{host}`"),
                    ModelErrorKind::InvalidRequest,
                )
                .with_dispatch(Dispatch::NotSent));
        }
        let separator = if url.contains('?') { '&' } else { '?' };
        let mut request = WireRequest::get(format!("{url}{separator}{query}"));
        let mut secrets = Vec::new();
        match header {
            Some(secret) => {
                request = request.header("x-identity-header", secret.clone());
                secrets.push(secret);
            }
            None => request = request.header("metadata", "true"),
        }
        self.exchange(request, &secrets).await
    }

    /// Sends a token request and builds the session from its answer.
    async fn exchange(
        &self,
        request: WireRequest,
        secrets: &[String],
    ) -> Result<Session, ModelError> {
        let transport = Transport::new(self.config.clone())?.redacting(secrets.iter().cloned());
        let body = transport.send(&request).await.map_err(|error| {
            let mut error = error.redact(&secrets.iter().map(String::as_str).collect::<Vec<_>>());
            if matches!(
                error.kind,
                ModelErrorKind::InvalidRequest | ModelErrorKind::NotFound
            ) {
                // Entra ID answers a bad secret or tenant with 400/401.
                error.kind = ModelErrorKind::Auth;
            }
            error.message = format!("Azure did not issue a token: {}", error.message);
            error.for_model("azure", self.deployment.clone())
        })?;
        let reply: Value = serde_json::from_slice(&body).map_err(|_| {
            self.token_error(
                "Azure's token reply is not JSON",
                ModelErrorKind::InvalidResponse,
            )
        })?;
        let Some(token) = reply
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        else {
            return Err(self.token_error(
                "Azure's token reply has no access_token",
                ModelErrorKind::InvalidResponse,
            ));
        };
        let model = self.model_for(None, Some(token.to_string()))?;
        let now = criogen_rt::system_now();
        let lifetime = lifetime(&reply, now)
            .unwrap_or(DEFAULT_LIFETIME)
            .min(MAX_LIFETIME);
        let renew_at = now + lifetime.saturating_sub(RENEW_BEFORE);
        Ok(Session {
            model: Arc::new(model),
            renew_at: Some(renew_at),
        })
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

/// How long a token lives: `expires_in` (seconds, a number or a string),
/// else `expires_on` (Unix seconds).
fn lifetime(reply: &Value, now: SystemTime) -> Option<Duration> {
    let seconds = |value: &Value| -> Option<u64> {
        match value {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    if let Some(secs) = reply.get("expires_in").and_then(seconds) {
        return Some(Duration::from_secs(secs));
    }
    let on = reply.get("expires_on").and_then(seconds)?;
    let at = SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(on))?;
    at.duration_since(now).ok()
}

/// `application/x-www-form-urlencoded` pairs.
fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

impl Model for AzureModel {
    fn provider(&self) -> &str {
        "azure"
    }

    fn model(&self) -> &str {
        &self.deployment
    }

    fn capabilities(&self) -> Capabilities {
        let placeholder = String::new();
        match self.api {
            AzureApi::Responses => Endpoint::azure(&self.base_url, placeholder).capabilities,
            AzureApi::Chat => Endpoint::azure_chat(&self.base_url, placeholder).capabilities,
        }
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
    fn form_values_are_percent_encoded() {
        assert_eq!(
            form(&[("scope", "https://x/.default"), ("s", "a b&c=d~")]),
            "scope=https%3A%2F%2Fx%2F.default&s=a%20b%26c%3Dd~"
        );
    }

    #[test]
    fn lifetimes_from_numbers_strings_and_expiry_times() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let of = |v: Value| lifetime(&v, now).map(|d| d.as_secs());
        assert_eq!(of(serde_json::json!({"expires_in": 3599})), Some(3599));
        assert_eq!(of(serde_json::json!({"expires_in": "86399"})), Some(86399));
        assert_eq!(of(serde_json::json!({"expires_on": "4600"})), Some(3600));
        assert_eq!(
            of(serde_json::json!({"expires_on": 10})),
            None,
            "already past"
        );
        assert_eq!(of(serde_json::json!({})), None);
    }
}

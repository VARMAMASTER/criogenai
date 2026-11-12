//! Kaggle's Model Proxy (design 0020): the hosted models a Kaggle account's
//! AI-model allowance pays for, reached as an OpenAI-compatible endpoint with
//! a short-lived proxy token that [`KaggleModel`] mints from the account's
//! Kaggle API token and renews before it expires.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::model::{BoxFuture, DeltaSink, Model};
use crate::provider::{Endpoint, ProviderModel, is_loopback_host, url_host};
use crate::request::{Capabilities, ModelRequest, ModelResponse};
use crate::transport::{HttpConfig, Transport, WireRequest};

/// Kaggle's API, where proxy tokens are minted.
pub const KAGGLE_API_BASE: &str = "https://api.kaggle.com";

const MINT_PATH: &str = "/v1/models.ModelProxyApiService/CreateDefaultModelProxyToken";
/// Renew this long before the token's stated expiry.
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
/// Never keep a token longer than this, whatever its stated expiry.
const MAX_LIFETIME: Duration = Duration::from_secs(50 * 60);
/// Largest token file read from disk.
const MAX_TOKEN_FILE: u64 = 4 * 1024;

enum Credential {
    /// A Kaggle API token: proxy tokens are minted (and renewed) from it.
    ApiToken(String),
    /// A proxy token minted elsewhere (a Kaggle notebook): used as is.
    Proxy,
}

#[derive(Clone)]
struct Session {
    model: Arc<ProviderModel>,
    /// `None`: never renewed (a pre-minted proxy token).
    renew_at: Option<SystemTime>,
}

/// A model behind Kaggle's Model Proxy, paid from the account's AI-model
/// allowance.
///
/// ```no_run
/// use criogenai_providers::{KaggleModel, Message, Model, ModelRequest};
///
/// # async fn demo() -> Result<(), criogenai_providers::ModelError> {
/// // KAGGLE_API_TOKEN, or ~/.kaggle/access_token (what the Kaggle CLI writes)
/// let model = KaggleModel::from_env("google/gemini-3.6-flash")?;
/// let response = model.invoke(&ModelRequest::new(vec![Message::user("hi")])).await?;
/// println!("{}", response.message.text());
/// # Ok(()) }
/// ```
pub struct KaggleModel {
    model: String,
    credential: Credential,
    api_base: String,
    config: HttpConfig,
    session: Mutex<Option<Session>>,
}

impl std::fmt::Debug for KaggleModel {
    /// No token is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KaggleModel")
            .field("model", &self.model)
            .field(
                "credential",
                &match self.credential {
                    Credential::ApiToken(_) => "api token [redacted]",
                    Credential::Proxy => "proxy token [redacted]",
                },
            )
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl KaggleModel {
    /// A model that mints its proxy token from a Kaggle API token
    /// (kaggle.com/settings → API), and renews it before it expires.
    pub fn new(api_token: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            credential: Credential::ApiToken(api_token.into().trim().to_string()),
            api_base: KAGGLE_API_BASE.to_string(),
            config: HttpConfig::default(),
            session: Mutex::new(None),
        }
    }

    /// A model on a proxy token minted elsewhere (a Kaggle notebook sets
    /// `MODEL_PROXY_URL` and `MODEL_PROXY_API_KEY`). It is used until it
    /// expires and cannot be renewed.
    pub fn from_proxy(
        base_url: impl Into<String>,
        proxy_token: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let model = model.into();
        let session = Session {
            model: Arc::new(proxy_model(
                &base_url.into(),
                proxy_token.into(),
                &model,
                HttpConfig::default(),
            )?),
            renew_at: None,
        };
        Ok(Self {
            model,
            credential: Credential::Proxy,
            api_base: KAGGLE_API_BASE.to_string(),
            config: HttpConfig::default(),
            session: Mutex::new(Some(session)),
        })
    }

    /// Credentials from the environment, in order: `MODEL_PROXY_URL` and
    /// `MODEL_PROXY_API_KEY` (a pre-minted proxy token); `KAGGLE_API_TOKEN`;
    /// the file `access_token` in `KAGGLE_CONFIG_DIR` or `~/.kaggle`.
    pub fn from_env(model: impl Into<String>) -> Result<Self, ModelError> {
        let model = model.into();
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let (Some(url), Some(token)) = (var("MODEL_PROXY_URL"), var("MODEL_PROXY_API_KEY")) {
            return Self::from_proxy(url, token, model);
        }
        if let Some(token) = var("KAGGLE_API_TOKEN") {
            return Ok(Self::new(token, model));
        }
        let dir = var("KAGGLE_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                var("HOME")
                    .or_else(|| var("USERPROFILE"))
                    .map(|home| std::path::Path::new(&home).join(".kaggle"))
            });
        if let Some(token) = dir.and_then(|dir| read_token_file(&dir.join("access_token"))) {
            return Ok(Self::new(token, model));
        }
        Err(ModelError::new(
            ModelErrorKind::Auth,
            "no Kaggle credentials: set KAGGLE_API_TOKEN (kaggle.com/settings, API), or save the \
             token in ~/.kaggle/access_token, or set MODEL_PROXY_URL and MODEL_PROXY_API_KEY",
        )
        .with_dispatch(Dispatch::NotSent)
        .for_model("kaggle", model))
    }

    /// Mints tokens at another Kaggle API base (tests use a local server).
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into().trim_end_matches('/').to_string();
        self
    }

    /// Timeouts and limits for both Kaggle's API and the proxy.
    pub fn with_config(mut self, config: HttpConfig) -> Self {
        self.config = config;
        self
    }

    fn can_renew(&self) -> bool {
        matches!(self.credential, Credential::ApiToken(_))
    }

    /// The current proxy model; mints a token when there is none, when it is
    /// due for renewal, or when `renew` is set.
    async fn current(&self, renew: bool) -> Result<Arc<ProviderModel>, ModelError> {
        let now = criogenai_rt::system_now();
        if let Some(session) = self.session.lock().expect("session lock").clone()
            && (!renew || !self.can_renew())
            && session.renew_at.is_none_or(|at| now < at)
        {
            return Ok(session.model);
        }
        let Credential::ApiToken(api_token) = &self.credential else {
            // A pre-minted token that is due: keep using it until the proxy refuses it.
            let session = self.session.lock().expect("session lock").clone();
            return session.map(|s| s.model).ok_or_else(|| {
                ModelError::new(ModelErrorKind::Auth, "the Kaggle proxy token is missing")
                    .with_dispatch(Dispatch::NotSent)
            });
        };
        let session = self.mint(api_token).await?;
        let model = session.model.clone();
        *self.session.lock().expect("session lock") = Some(session);
        Ok(model)
    }

    async fn mint(&self, api_token: &str) -> Result<Session, ModelError> {
        if let Some(("http", host)) = url_host(&self.api_base)
            && !is_loopback_host(host)
        {
            return Err(ModelError::new(
                ModelErrorKind::InvalidRequest,
                format!("refusing to send the Kaggle API token over plain http to `{host}`"),
            )
            .with_dispatch(Dispatch::NotSent));
        }
        let transport = Transport::new(self.config.clone())?.redacting([api_token.to_string()]);
        let request = WireRequest::post(format!("{}{MINT_PATH}", self.api_base), b"{}".to_vec())
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {api_token}"));
        let body = transport.send(&request).await.map_err(|error| {
            let mut error = error.redact(&[api_token]);
            if error.kind == ModelErrorKind::NotFound {
                error.message = format!(
                    "Kaggle's Model Proxy is not available to this account (404): {}",
                    error.message
                );
            }
            error.for_model("kaggle", self.model.clone())
        })?;
        let reply: Value = serde_json::from_slice(&body).map_err(|_| {
            ModelError::new(
                ModelErrorKind::InvalidResponse,
                "Kaggle's token reply is not JSON",
            )
        })?;
        let field = |name: &str| {
            reply
                .get(name)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        };
        let (Some(base_url), Some(token)) = (field("baseUri"), field("token")) else {
            return Err(ModelError::new(
                ModelErrorKind::InvalidResponse,
                "Kaggle returned no Model Proxy token",
            ));
        };
        let model = proxy_model(
            base_url,
            token.to_string(),
            &self.model,
            self.config.clone(),
        )?;
        let now = criogenai_rt::system_now();
        let latest = now + MAX_LIFETIME;
        let renew_at = field("expiryTime")
            .and_then(parse_rfc3339)
            .and_then(|expiry| expiry.checked_sub(RENEW_BEFORE))
            .map_or(latest, |at| at.min(latest));
        Ok(Session {
            model: Arc::new(model),
            renew_at: Some(renew_at),
        })
    }

    async fn call(&self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let model = self.current(false).await?;
        match model.invoke(request).await {
            Err(error) if error.kind == ModelErrorKind::Auth && self.can_renew() => {
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
                    && self.can_renew() =>
            {
                self.current(true).await?.stream(request, on_delta).await
            }
            result => result,
        }
    }
}

/// The OpenAI-compatible proxy endpoint for a `baseUri` (with or without the
/// `/openapi` or `/genai` suffix).
fn proxy_model(
    base_url: &str,
    token: String,
    model: &str,
    config: HttpConfig,
) -> Result<ProviderModel, ModelError> {
    let mut base = base_url.trim_end_matches('/');
    for suffix in ["/openapi", "/genai"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped;
            break;
        }
    }
    let mut endpoint =
        Endpoint::openai_compatible("kaggle", format!("{base}/openapi"), Some(token));
    endpoint.capabilities.tools = Some(true);
    endpoint.capabilities.streaming = Some(true);
    ProviderModel::with_config(endpoint, model, config)
}

/// The first line of a small token file, if it has one.
fn read_token_file(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_TOKEN_FILE).read_to_string(&mut text).ok()?;
    let token = text.lines().next()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// `2026-11-01T15:12:50.369924200Z` or `…+05:30` as a time; `None` when it
/// does not parse (the caller then renews early instead).
fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let (clock, offset_seconds) = if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (clock, offset) = rest.split_at(at);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (h, m) = offset.get(1..)?.split_once(':')?;
        (
            clock,
            sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60),
        )
    };
    let clock = clock.split('.').next()?;
    let mut parts = clock.splitn(3, ':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(seconds).ok()?))
}

impl Model for KaggleModel {
    fn provider(&self) -> &str {
        "kaggle"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: Some(true),
            streaming: Some(true),
            ..Capabilities::default()
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

    fn unix(text: &str) -> Option<u64> {
        parse_rfc3339(text).map(|t| t.duration_since(UNIX_EPOCH).unwrap().as_secs())
    }

    #[test]
    fn expiry_times_parse_with_fractions_and_offsets() {
        assert_eq!(unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix("2026-11-01T15:12:50.369924200Z"), Some(1_793_545_970));
        assert_eq!(unix("2026-11-01T20:42:50+05:30"), Some(1_793_545_970));
        assert_eq!(unix("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(unix("not a time"), None);
        assert_eq!(unix("2026-13-01T00:00:00Z"), None);
        assert_eq!(unix("1969-12-31T23:59:59Z"), None);
    }
}

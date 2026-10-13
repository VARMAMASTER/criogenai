//! Classified model-provider errors (design 0003 §4, 0006 §6).
//!
//! Adapters never retry; they classify. Retry and fallback decisions are made
//! once, from [`ModelErrorKind`], so every provider behaves the same.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest message kept, in bytes (0006 §3).
pub const MAX_MESSAGE_BYTES: usize = 2048;

/// What kind of failure a model call had (0003 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelErrorKind {
    /// 429: too many requests.
    RateLimited,
    /// 529 or "overloaded".
    Overloaded,
    /// 500, 502, 503, 504.
    Server,
    /// The call took too long.
    Timeout,
    /// Connection, DNS or TLS failure.
    Network,
    /// 401: missing or invalid credentials.
    Auth,
    /// 403.
    Permission,
    /// Unknown model or endpoint.
    NotFound,
    /// The provider rejected the request (400).
    InvalidRequest,
    /// The prompt is longer than the model's context window.
    ContextOverflow,
    /// The provider refused the request with an error.
    ContentFilter,
    /// Billing or quota exhausted.
    QuotaExhausted,
    /// A 2xx response whose body could not be read. Not retried: it was
    /// already billed.
    InvalidResponse,
    /// Anything unclassified.
    Unknown,
}

impl ModelErrorKind {
    /// The stable snake_case name.
    pub fn code(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::Server => "server",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Auth => "auth",
            Self::Permission => "permission",
            Self::NotFound => "not_found",
            Self::InvalidRequest => "invalid_request",
            Self::ContextOverflow => "context_overflow",
            Self::ContentFilter => "content_filter",
            Self::QuotaExhausted => "quota_exhausted",
            Self::InvalidResponse => "invalid_response",
            Self::Unknown => "unknown",
        }
    }

    /// `true` for kinds that time may fix (retried by default).
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Overloaded | Self::Server | Self::Timeout | Self::Network
        )
    }

    /// The 0003 §3 category: `"transient"` or `"provider_fatal"`.
    pub fn category(self) -> &'static str {
        if self.is_transient() {
            "transient"
        } else {
            "provider_fatal"
        }
    }
}

impl fmt::Display for ModelErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// How far a request got before it failed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Dispatch {
    /// Never reached the provider (DNS, connection refused): always safe to
    /// retry or fall back immediately.
    NotSent,
    /// Reached the provider; may have been billed.
    #[default]
    Sent,
    /// Failed while streaming the answer.
    Streaming,
}

/// A failed model call, classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[non_exhaustive]
pub struct ModelError {
    /// What kind of failure.
    pub kind: ModelErrorKind,
    /// Sanitized description (no URLs or credentials, at most
    /// [`MAX_MESSAGE_BYTES`]).
    pub message: String,
    /// Provider name, e.g. `anthropic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// HTTP status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The provider's own error type or code, e.g. `overloaded_error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    /// How long the provider asked us to wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// The provider's request id, for support tickets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// How far the request got.
    #[serde(default)]
    pub dispatch: Dispatch,
    /// With [`Dispatch::Streaming`]: whether part of the answer was already
    /// emitted (the runtime then sends `message_aborted` before a retry).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub emitted_deltas: bool,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(provider) = &self.provider {
            write!(f, "{provider} ")?;
        }
        write!(f, "{}", self.kind)?;
        if let Some(status) = self.status {
            write!(f, " (HTTP {status})")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl ModelError {
    /// An error with a kind and a message (sanitized here).
    pub fn new(kind: ModelErrorKind, message: impl AsRef<str>) -> Self {
        Self {
            kind,
            message: sanitize_message(message.as_ref(), &[]),
            provider: None,
            model: None,
            status: None,
            provider_code: None,
            retry_after_ms: None,
            request_id: None,
            dispatch: Dispatch::Sent,
            emitted_deltas: false,
        }
    }

    /// Sets provider and model.
    pub fn for_model(mut self, provider: impl Into<String>, model: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self
    }

    /// Sets [`dispatch`](Self::dispatch).
    pub fn with_dispatch(mut self, dispatch: Dispatch) -> Self {
        self.dispatch = dispatch;
        self
    }

    /// Sets [`emitted_deltas`](Self::emitted_deltas).
    pub fn with_emitted(mut self, emitted_deltas: bool) -> Self {
        self.emitted_deltas = emitted_deltas;
        self
    }

    /// Sets [`retry_after_ms`](Self::retry_after_ms).
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after_ms = Some(u64::try_from(retry_after.as_millis()).unwrap_or(u64::MAX));
        self
    }

    /// Removes configured credentials from the message (0003 §6.4). Call it
    /// with every key the endpoint knows.
    pub fn redact(mut self, secrets: &[&str]) -> Self {
        self.message = sanitize_message(&self.message, secrets);
        self
    }

    /// The 0003 §3 category of [`kind`](Self::kind).
    pub fn category(&self) -> &'static str {
        self.kind.category()
    }

    /// The wait the provider asked for, if any.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after_ms.map(Duration::from_millis)
    }
}

/// A non-2xx HTTP response, as the transport saw it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct HttpFailure<'a> {
    /// HTTP status.
    pub status: u16,
    /// Response headers (names in any case).
    pub headers: &'a [(String, String)],
    /// Response body.
    pub body: &'a str,
}

impl<'a> HttpFailure<'a> {
    /// A failure with these parts.
    pub fn new(status: u16, headers: &'a [(String, String)], body: &'a str) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    fn header(&self, name: &str) -> Option<&'a str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    }
}

/// Classifies an HTTP failure from any provider. Order (research §10.8):
/// in-band error code, then HTTP status, then message text (only to refine a
/// 400/413/422 into `context_overflow` or `content_filter`).
///
/// `now` is used to read an HTTP-date `Retry-After`.
pub fn classify_http(failure: &HttpFailure<'_>, now: SystemTime) -> ModelError {
    let inband = InBand::parse(failure.body);
    let text = inband.message.as_deref().unwrap_or(failure.body);
    let lower = text.to_ascii_lowercase();
    let codes: Vec<String> = [&inband.code, &inband.reason, &inband.kind]
        .into_iter()
        .flatten()
        .map(|c| c.to_ascii_lowercase())
        .collect();

    let kind = codes
        .iter()
        .find_map(|code| kind_from_code(code))
        .unwrap_or_else(|| kind_from_status(failure.status, &lower));

    let mut error = ModelError::new(
        kind,
        if text.trim().is_empty() {
            format!("HTTP {}", failure.status)
        } else {
            text.to_string()
        },
    );
    error.status = Some(failure.status);
    error.provider_code = inband.code.or(inband.kind);
    error.request_id = ["x-request-id", "request-id"]
        .iter()
        .find_map(|name| failure.header(name))
        .map(str::to_string);
    error.retry_after_ms = retry_after(failure, now).map(|d| d.as_millis() as u64);
    error
}

/// The error for a 2xx response whose body could not be decoded.
pub fn invalid_response(detail: impl AsRef<str>) -> ModelError {
    ModelError::new(
        ModelErrorKind::InvalidResponse,
        format!(
            "could not read the provider's response: {}",
            detail.as_ref()
        ),
    )
}

/// The in-band error object, in the shapes providers use:
/// `{"error": {"type", "code", "message"}}` (OpenAI, Anthropic, llama.cpp),
/// `{"error": "text"}` (Ollama), `{"object": "error", "message", "type"}` (vLLM),
/// and Google's `[{"error": {"code": 400, "status", "message", "details":
/// [{"reason"}]}}]` (Gemini's OpenAI-compatible endpoint wraps it in a list).
#[derive(Default)]
struct InBand {
    code: Option<String>,
    kind: Option<String>,
    /// Google's `details[].reason` (e.g. `API_KEY_INVALID`).
    reason: Option<String>,
    message: Option<String>,
}

impl InBand {
    fn parse(body: &str) -> Self {
        let Ok(mut value) = serde_json::from_str::<Value>(body) else {
            return Self::default();
        };
        if let Value::Array(items) = &mut value {
            value = items.first().cloned().unwrap_or(Value::Null);
        }
        let string = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
        match value.get("error") {
            Some(Value::String(message)) => Self {
                message: Some(message.clone()),
                ..Self::default()
            },
            Some(error @ Value::Object(_)) => Self {
                code: string(error.get("code")),
                // Google puts its canonical code (`UNAUTHENTICATED`, …) in
                // `status`.
                kind: string(error.get("type")).or_else(|| string(error.get("status"))),
                reason: error
                    .get("details")
                    .and_then(Value::as_array)
                    .and_then(|details| details.iter().find_map(|d| string(d.get("reason")))),
                message: string(error.get("message")),
            },
            _ => Self {
                code: string(value.get("code")),
                kind: string(value.get("type")).filter(|t| t != "error"),
                reason: None,
                message: string(value.get("message")).or_else(|| string(value.get("detail"))),
            },
        }
    }
}

fn kind_from_code(code: &str) -> Option<ModelErrorKind> {
    use ModelErrorKind::*;
    Some(match code {
        "insufficient_quota" | "billing_hard_limit_reached" | "billing_error" => QuotaExhausted,
        "overloaded_error" | "overloaded" => Overloaded,
        "rate_limit_error" | "rate_limit_exceeded" => RateLimited,
        "context_length_exceeded" | "exceed_context_size_error" | "string_above_max_length" => {
            ContextOverflow
        }
        "content_policy_violation" | "content_filter" => ContentFilter,
        "model_not_found" | "not_found_error" => NotFound,
        "authentication_error" | "invalid_api_key" => Auth,
        "permission_error" => Permission,
        "timeout_error" => Timeout,
        // Google: `details[].reason`, then canonical `status` codes. Google
        // reports a bad key as HTTP 400 `INVALID_ARGUMENT` with reason
        // `API_KEY_INVALID`.
        "api_key_invalid" | "api_key_expired" | "unauthenticated" => Auth,
        "permission_denied" | "api_key_service_blocked" => Permission,
        "resource_exhausted" | "rate_limit_exceeded_per_minute" => RateLimited,
        "deadline_exceeded" => Timeout,
        // Generic codes such as `api_error` or `server_error` say nothing the
        // status does not: Ollama sends `api_error` with a 404 for an unknown
        // model, which must not become a (retried) server error.
        _ => return None,
    })
}

const CONTEXT_OVERFLOW_TEXT: &[&str] = &[
    "context_length_exceeded",
    "maximum context length",
    "prompt is too long",
    "exceeds the available context size",
    "context window",
    "input is too long",
    "too many tokens",
];

/// Bad keys some providers report as HTTP 400 (Gemini: "Please pass a
/// valid API key", `INVALID_ARGUMENT`).
const AUTH_TEXT: &[&str] = &[
    "pass a valid api key",
    "api key not valid",
    "api_key_invalid",
    "invalid api key",
    "incorrect api key",
];

const CONTENT_FILTER_TEXT: &[&str] = &[
    "content_policy_violation",
    "content management policy",
    "safety system",
    "content filter",
    "responsibleaipolicyviolation",
];

fn kind_from_status(status: u16, lower: &str) -> ModelErrorKind {
    use ModelErrorKind::*;
    let contains = |list: &[&str]| list.iter().any(|needle| lower.contains(needle));
    match status {
        429 => RateLimited,
        529 => Overloaded,
        408 => Timeout,
        500..=599 | 409 if lower.contains("overloaded") => Overloaded,
        500 | 502 | 503 | 504 | 409 => Server,
        401 => Auth,
        402 => QuotaExhausted,
        403 => Permission,
        404 => NotFound,
        400 if contains(AUTH_TEXT) => Auth,
        400 | 413 | 422 if contains(CONTEXT_OVERFLOW_TEXT) => ContextOverflow,
        400 | 413 | 422 if contains(CONTENT_FILTER_TEXT) => ContentFilter,
        400 | 413 | 422 => InvalidRequest,
        _ => Unknown,
    }
}

/// `retry-after-ms` (milliseconds), else `retry-after` (seconds or an
/// HTTP date). A date in the past means "now".
fn retry_after(failure: &HttpFailure<'_>, now: SystemTime) -> Option<Duration> {
    if let Some(ms) = failure
        .header("retry-after-ms")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
    {
        return Some(Duration::from_secs_f64(ms / 1000.0));
    }
    let value = failure.header("retry-after")?;
    if let Some(secs) = value
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= 0.0)
    {
        return Some(Duration::from_secs_f64(secs));
    }
    let at = parse_http_date(value)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Parses an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the form
/// `Retry-After` uses in practice.
fn parse_http_date(value: &str) -> Option<SystemTime> {
    let rest = value.split_once(", ")?.1;
    let parts: Vec<&str> = rest.split(' ').collect();
    let [day, month, year, time, "GMT"] = parts.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS.iter().position(|m| m == month)? as i64 + 1;
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let hms: Vec<i64> = time
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let [h, m, s] = hms.as_slice() else {
        return None;
    };
    if !(1..=31).contains(&day) || *h > 23 || *m > 59 || *s > 60 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + s;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Makes a provider message safe to log and show (0003 §6.4): removes
/// `secrets`, API-key-like tokens and URLs, and bounds the length.
pub fn sanitize_message(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(secret, "[redacted]");
    }
    let mut cleaned = String::with_capacity(out.len());
    let mut after_bearer = false;
    for piece in out.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        let tail = &piece[word.len()..];
        let replaced = if (after_bearer && !word.is_empty()) || looks_like_key(word) {
            Some("[redacted]")
        } else if word.contains("://") {
            Some("[url]")
        } else {
            None
        };
        if !word.is_empty() {
            after_bearer = word.eq_ignore_ascii_case("bearer");
        }
        match replaced {
            Some(r) => {
                // Keep punctuation that follows a URL or key, e.g. "url."
                let trailing: &str = word
                    .rfind(|c: char| c.is_alphanumeric() || c == '/' || c == '_' || c == '-')
                    .map_or("", |i| &word[i + 1..]);
                cleaned.push_str(r);
                cleaned.push_str(trailing);
            }
            None => cleaned.push_str(word),
        }
        cleaned.push_str(tail);
    }
    truncate(cleaned, MAX_MESSAGE_BYTES)
}

fn looks_like_key(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
    ["sk-", "sk_", "pk-", "AIza", "xai-", "gsk_"]
        .iter()
        .any(|prefix| word.starts_with(prefix) && word.len() >= prefix.len() + 12)
}

fn truncate(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut cut = max - "…".len();
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push('…');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(status: u16, body: &str) -> ModelError {
        classify_http(&HttpFailure::new(status, &[], body), UNIX_EPOCH)
    }

    #[test]
    fn status_table() {
        use ModelErrorKind::*;
        for (status, kind) in [
            (429, RateLimited),
            (529, Overloaded),
            (500, Server),
            (502, Server),
            (503, Server),
            (504, Server),
            (409, Server),
            (408, Timeout),
            (401, Auth),
            (402, QuotaExhausted),
            (403, Permission),
            (404, NotFound),
            (400, InvalidRequest),
            (413, InvalidRequest),
            (422, InvalidRequest),
            (418, Unknown),
        ] {
            assert_eq!(classify(status, "").kind, kind, "status {status}");
        }
    }

    #[test]
    fn google_errors_are_read_from_their_list_shape() {
        // What Gemini's OpenAI-compatible endpoint returned for a bad key.
        let bad_key = r#"[{
          "error": {
            "code": 400,
            "message": "API key not valid. Please pass a valid API key.",
            "status": "INVALID_ARGUMENT",
            "details": [{
              "@type": "type.googleapis.com/google.rpc.ErrorInfo",
              "reason": "API_KEY_INVALID",
              "domain": "googleapis.com"
            }]
          }
        }]"#;
        let error = classify(400, bad_key);
        assert_eq!(error.kind, ModelErrorKind::Auth);
        assert_eq!(
            error.message,
            "API key not valid. Please pass a valid API key."
        );

        // What it actually returns today: no `details`, only the text.
        let plain = r#"[{"error": {"code": 400, "message": "Please pass a valid API key", "status": "INVALID_ARGUMENT"}}]"#;
        assert_eq!(classify(400, plain).kind, ModelErrorKind::Auth);

        let quota = r#"[{"error": {"code": 429, "message": "Quota exceeded", "status": "RESOURCE_EXHAUSTED"}}]"#;
        assert_eq!(classify(429, quota).kind, ModelErrorKind::RateLimited);
        let denied = r#"{"error": {"code": 403, "message": "no", "status": "PERMISSION_DENIED"}}"#;
        assert_eq!(classify(403, denied).kind, ModelErrorKind::Permission);
        // A plain bad request stays one, and its text is still checked.
        let invalid = r#"[{"error": {"code": 400, "message": "The input token count exceeds the maximum number of tokens allowed, too many tokens", "status": "INVALID_ARGUMENT"}}]"#;
        assert_eq!(classify(400, invalid).kind, ModelErrorKind::ContextOverflow);
        let other =
            r#"[{"error": {"code": 400, "message": "bad field", "status": "INVALID_ARGUMENT"}}]"#;
        assert_eq!(classify(400, other).kind, ModelErrorKind::InvalidRequest);
        assert_eq!(classify(400, "[]").kind, ModelErrorKind::InvalidRequest);
    }

    #[test]
    fn in_band_codes_win_over_status() {
        let openai_quota = r#"{"error": {"message": "You exceeded your current quota", "type": "insufficient_quota", "code": "insufficient_quota"}}"#;
        let error = classify(429, openai_quota);
        assert_eq!(error.kind, ModelErrorKind::QuotaExhausted);
        assert!(!error.kind.is_transient());
        assert_eq!(error.provider_code.as_deref(), Some("insufficient_quota"));
        assert_eq!(error.message, "You exceeded your current quota");

        let anthropic =
            r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#;
        assert_eq!(classify(529, anthropic).kind, ModelErrorKind::Overloaded);
        assert_eq!(classify(500, anthropic).kind, ModelErrorKind::Overloaded);

        let openai_context = r#"{"error": {"message": "This model's maximum context length is 8192 tokens", "type": "invalid_request_error", "code": "context_length_exceeded"}}"#;
        assert_eq!(
            classify(400, openai_context).kind,
            ModelErrorKind::ContextOverflow
        );
    }

    #[test]
    fn message_text_refines_bad_requests_only() {
        let anthropic_long = r#"{"type": "error", "error": {"type": "invalid_request_error", "message": "prompt is too long: 210000 tokens > 200000 maximum"}}"#;
        assert_eq!(
            classify(400, anthropic_long).kind,
            ModelErrorKind::ContextOverflow
        );
        let llama = r#"{"error": {"code": 400, "message": "the request exceeds the available context size", "type": "invalid_request_error"}}"#;
        assert_eq!(classify(400, llama).kind, ModelErrorKind::ContextOverflow);
        let azure = r#"{"error": {"message": "The response was filtered due to the prompt triggering Azure OpenAI's content management policy."}}"#;
        assert_eq!(classify(400, azure).kind, ModelErrorKind::ContentFilter);
        // The same text on a 500 does not turn it into a request error.
        assert_eq!(classify(500, anthropic_long).kind, ModelErrorKind::Server);
    }

    #[test]
    fn generic_in_band_types_do_not_override_the_status() {
        // Ollama's OpenAI-compatible endpoint, unknown model.
        let ollama = r#"{"error":{"message":"model \"llama3.2\" not found, try pulling it first","type":"api_error","param":null,"code":null}}"#;
        let error = classify(404, ollama);
        assert_eq!(error.kind, ModelErrorKind::NotFound);
        assert!(!error.kind.is_transient());
        let anthropic_500 = r#"{"type": "error", "error": {"type": "api_error", "message": "Internal server error"}}"#;
        assert_eq!(classify(500, anthropic_500).kind, ModelErrorKind::Server);
    }

    #[test]
    fn ollama_and_vllm_shapes() {
        let ollama = r#"{"error": "model 'llama9' not found, try pulling it first"}"#;
        let error = classify(404, ollama);
        assert_eq!(error.kind, ModelErrorKind::NotFound);
        assert!(error.message.starts_with("model 'llama9'"));
        let vllm = r#"{"object": "error", "message": "This model's maximum context length is 4096 tokens.", "type": "BadRequestError", "code": 400}"#;
        let error = classify(400, vllm);
        assert_eq!(error.kind, ModelErrorKind::ContextOverflow);
        assert_eq!(error.provider_code.as_deref(), Some("BadRequestError"));
    }

    #[test]
    fn non_json_body_and_empty_body() {
        let error = classify(502, "<html>Bad Gateway</html>");
        assert_eq!(error.kind, ModelErrorKind::Server);
        assert_eq!(error.message, "<html>Bad Gateway</html>");
        assert_eq!(classify(503, "  ").message, "HTTP 503");
    }

    #[test]
    fn retry_after_forms_and_request_id() {
        let headers = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let now = UNIX_EPOCH + Duration::from_secs(784_111_777); // Sun, 06 Nov 1994 08:49:37 GMT
        let cases = [
            (headers(&[("Retry-After-Ms", "1500")]), Some(1500)),
            (headers(&[("retry-after", "7")]), Some(7000)),
            (headers(&[("retry-after", "0.5")]), Some(500)),
            (
                headers(&[("Retry-After", "Sun, 06 Nov 1994 08:50:07 GMT")]),
                Some(30_000),
            ),
            (
                headers(&[("retry-after", "Sun, 06 Nov 1994 08:00:00 GMT")]),
                Some(0),
            ),
            (headers(&[("retry-after", "soon")]), None),
            (headers(&[("retry-after", "-3")]), None),
            (
                headers(&[("retry-after-ms", "250"), ("retry-after", "9")]),
                Some(250),
            ),
        ];
        for (h, expected) in cases {
            let error = classify_http(&HttpFailure::new(429, &h, ""), now);
            assert_eq!(error.retry_after_ms, expected, "{h:?}");
        }
        let h = headers(&[("X-Request-Id", "req_123")]);
        let error = classify_http(&HttpFailure::new(500, &h, ""), now);
        assert_eq!(error.request_id.as_deref(), Some("req_123"));
    }

    #[test]
    fn http_date_matches_known_epoch_values() {
        let at = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(at, UNIX_EPOCH + Duration::from_secs(784_111_777));
        let leap = parse_http_date("Thu, 29 Feb 2024 00:00:00 GMT").unwrap();
        assert_eq!(leap, UNIX_EPOCH + Duration::from_secs(1_709_164_800));
        assert!(parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT").is_none());
    }

    #[test]
    fn messages_are_sanitized() {
        let text = "Invalid key sk-proj-ABCDEFGHIJKLMNOP for https://api.example.com/v1?key=x. \
                    Header was Bearer abc.def.ghi, secret hunter22.";
        let clean = sanitize_message(text, &["hunter22"]);
        assert_eq!(
            clean,
            "Invalid key [redacted] for [url]. Header was Bearer [redacted], secret [redacted]."
        );
        let long = "é".repeat(MAX_MESSAGE_BYTES);
        let cut = sanitize_message(&long, &[]);
        assert!(cut.len() <= MAX_MESSAGE_BYTES && cut.ends_with('…'));
        assert_eq!(
            sanitize_message("Authorization: Bearer   tok123", &[]),
            "Authorization: Bearer   [redacted]"
        );
        // Short tokens that merely start like a key are kept.
        assert_eq!(sanitize_message("use sk-1 here", &[]), "use sk-1 here");
    }

    #[test]
    fn display_and_json() {
        let error =
            classify(429, r#"{"error": {"message": "slow down"}}"#).for_model("openai", "gpt-x");
        assert_eq!(
            error.to_string(),
            "openai rate_limited (HTTP 429): slow down"
        );
        assert_eq!(error.category(), "transient");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "rate_limited");
        assert_eq!(json["dispatch"], "sent");
        let back: ModelError = serde_json::from_value(json).unwrap();
        assert_eq!(back, error);
    }
}

//! The Langfuse preset (design 0009 §4).

use crate::exporter::{OtlpConfig, TelemetryError};

/// Default Langfuse Cloud region (EU).
pub const DEFAULT_BASE_URL: &str = "https://cloud.langfuse.com";

/// Builds an [`OtlpConfig`] for Langfuse (cloud, any region, or
/// self-hosted ≥ v3.22).
#[derive(Debug, Clone, Copy)]
pub struct Langfuse;

impl Langfuse {
    /// Reads `LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY` and
    /// `LANGFUSE_BASE_URL` (or the older `LANGFUSE_HOST`; default
    /// [`DEFAULT_BASE_URL`]).
    pub fn from_env() -> Result<OtlpConfig, TelemetryError> {
        Self::resolve(None, None, None)
    }

    /// A config from settings given in code, with the environment for the
    /// rest: both keys or neither (then `LANGFUSE_PUBLIC_KEY` and
    /// `LANGFUSE_SECRET_KEY`); `base_url` when given, else
    /// `LANGFUSE_BASE_URL` (or `LANGFUSE_HOST`), else [`DEFAULT_BASE_URL`].
    ///
    /// # Errors
    ///
    /// [`TelemetryError::Missing`] when only one key is given, or neither
    /// is given and a key variable is not set.
    pub fn resolve(
        public_key: Option<&str>,
        secret_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<OtlpConfig, TelemetryError> {
        resolve_with(public_key, secret_key, base_url, |name| {
            std::env::var(name).ok()
        })
    }
}

/// [`Langfuse::resolve`] with the environment as a function (for tests).
fn resolve_with(
    public_key: Option<&str>,
    secret_key: Option<&str>,
    base_url: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<OtlpConfig, TelemetryError> {
    let given = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    let var = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    let (public, secret) = match (given(public_key), given(secret_key)) {
        (Some(public), Some(secret)) => (public, secret),
        (Some(_), None) => {
            return Err(TelemetryError::Missing(
                "secret_key (pass both keys, or neither to read the environment)".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(TelemetryError::Missing(
                "public_key (pass both keys, or neither to read the environment)".into(),
            ));
        }
        (None, None) => (
            var("LANGFUSE_PUBLIC_KEY")
                .ok_or_else(|| TelemetryError::Missing("LANGFUSE_PUBLIC_KEY".into()))?,
            var("LANGFUSE_SECRET_KEY")
                .ok_or_else(|| TelemetryError::Missing("LANGFUSE_SECRET_KEY".into()))?,
        ),
    };
    let base = given(base_url)
        .or_else(|| var("LANGFUSE_BASE_URL"))
        .or_else(|| var("LANGFUSE_HOST"))
        .unwrap_or_else(|| DEFAULT_BASE_URL.into());
    Ok(Langfuse::config(public, secret, base))
}

impl Langfuse {
    /// A config for these keys and base URL (e.g.
    /// `https://us.cloud.langfuse.com` or `http://localhost:3000`).
    pub fn config(
        public_key: impl AsRef<str>,
        secret_key: impl AsRef<str>,
        base_url: impl AsRef<str>,
    ) -> OtlpConfig {
        let base = base_url.as_ref().trim_end_matches('/');
        let credentials = format!("{}:{}", public_key.as_ref(), secret_key.as_ref());
        OtlpConfig::new(format!("{base}/api/public/otel/v1/traces"))
            .header(
                "authorization",
                format!("Basic {}", base64(credentials.as_bytes())),
            )
            // Without it, Langfuse may delay ingestion by up to 10 minutes.
            .header("x-langfuse-ingestion-version", "4")
    }
}

/// Standard base64 with padding (RFC 4648).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn resolve_takes_both_keys_or_neither_and_prefers_the_given_address() {
        let env = |name: &str| match name {
            "LANGFUSE_PUBLIC_KEY" => Some("pk-env".to_string()),
            "LANGFUSE_SECRET_KEY" => Some("sk-env".to_string()),
            "LANGFUSE_BASE_URL" => Some("http://env:3000".to_string()),
            _ => None,
        };
        let endpoint = |c: OtlpConfig| c.endpoint;
        // One key alone is an error, never silently replaced by the environment.
        assert!(matches!(
            resolve_with(Some("pk-1"), None, None, env),
            Err(TelemetryError::Missing(m)) if m.starts_with("secret_key")
        ));
        assert!(matches!(
            resolve_with(None, Some("sk-1"), None, env),
            Err(TelemetryError::Missing(m)) if m.starts_with("public_key")
        ));
        // The given address wins, with given keys or keys from the environment.
        for (public, secret) in [(Some("pk-1"), Some("sk-1")), (None, None)] {
            let config = resolve_with(public, secret, Some("http://mine:3000"), env).unwrap();
            assert_eq!(
                endpoint(config),
                "http://mine:3000/api/public/otel/v1/traces"
            );
        }
        // Without one: the environment's address, also with given keys.
        let config = resolve_with(Some("pk-1"), Some("sk-1"), None, env).unwrap();
        assert_eq!(
            endpoint(config),
            "http://env:3000/api/public/otel/v1/traces"
        );
        let config = resolve_with(None, None, None, |_| None);
        assert!(matches!(config, Err(TelemetryError::Missing(m)) if m == "LANGFUSE_PUBLIC_KEY"));
        let config = resolve_with(Some("pk"), Some("sk"), None, |_| None).unwrap();
        assert_eq!(
            endpoint(config),
            format!("{DEFAULT_BASE_URL}/api/public/otel/v1/traces")
        );
    }

    #[test]
    fn config_targets_the_otel_endpoint_with_basic_auth() {
        let config = Langfuse::config("pk-lf-1", "sk-lf-2", "http://localhost:3000/");
        assert_eq!(
            config.endpoint,
            "http://localhost:3000/api/public/otel/v1/traces"
        );
        let header = |name: &str| {
            config
                .headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            header("authorization").unwrap(),
            format!("Basic {}", base64(b"pk-lf-1:sk-lf-2"))
        );
        assert_eq!(header("x-langfuse-ingestion-version").unwrap(), "4");
        // Credentials never show in Debug output.
        assert!(!format!("{config:?}").contains("sk-lf-2"));
    }
}

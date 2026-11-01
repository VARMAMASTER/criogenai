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
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let public = var("LANGFUSE_PUBLIC_KEY")
            .ok_or_else(|| TelemetryError::Missing("LANGFUSE_PUBLIC_KEY".into()))?;
        let secret = var("LANGFUSE_SECRET_KEY")
            .ok_or_else(|| TelemetryError::Missing("LANGFUSE_SECRET_KEY".into()))?;
        let base = var("LANGFUSE_BASE_URL")
            .or_else(|| var("LANGFUSE_HOST"))
            .unwrap_or_else(|| DEFAULT_BASE_URL.into());
        Ok(Self::config(public, secret, base))
    }

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

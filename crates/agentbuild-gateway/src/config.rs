//! The gateway's config file (design 0006 §12.1). Secrets never appear in
//! it: upstream API keys and virtual keys are read from environment
//! variables the file names.

use std::sync::Arc;

use agentbuild_providers::{Endpoint, Model, ProviderModel};
use serde::Deserialize;

use crate::{Gateway, GatewayBuilder, GatewayError};

/// A config file that could not be used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Not valid TOML, or not the expected shape.
    #[error("unreadable gateway config: {0}")]
    Parse(String),
    /// An environment variable the config names is not set.
    #[error("environment variable `{0}` is not set")]
    MissingEnv(String),
    /// Something else is wrong.
    #[error(transparent)]
    Gateway(#[from] GatewayError),
}

/// The whole file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
    /// What clients may ask for.
    pub routes: Vec<RouteConfig>,
    /// Virtual keys; none means an open gateway.
    #[serde(default)]
    pub keys: Vec<KeyConfig>,
    /// Largest request body in bytes.
    #[serde(default)]
    pub body_limit: Option<usize>,
}

/// `[[routes]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RouteConfig {
    /// The name clients send as `model`.
    pub name: String,
    /// Tried in order (design 0006 §12.4).
    pub models: Vec<ModelConfig>,
}

/// One upstream model.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModelConfig {
    /// `openai`, `gemini`, `anthropic`, `openrouter`, `ollama` or
    /// `openai_compatible`.
    pub provider: String,
    /// The provider's model name.
    pub model: String,
    /// The environment variable holding the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Another base URL (required for `openai_compatible`).
    #[serde(default)]
    pub base_url: Option<String>,
}

/// `[[keys]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct KeyConfig {
    /// Shown in usage records.
    pub name: String,
    /// The environment variable holding the key.
    pub key_env: String,
    /// Routes it may use.
    pub routes: Vec<String>,
}

impl Config {
    /// Parses TOML.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|err| ConfigError::Parse(err.to_string()))
    }

    /// Builds the gateway, reading secrets with `env` (normally
    /// `std::env::var`).
    pub fn into_gateway_with(
        self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Gateway, ConfigError> {
        Ok(self.into_builder_with(env)?.build()?)
    }

    /// The builder the config describes, to add to (e.g.
    /// [`GatewayBuilder::usage`]) before building.
    pub fn into_builder_with(
        self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<GatewayBuilder, ConfigError> {
        let var = |name: &str| env(name).ok_or_else(|| ConfigError::MissingEnv(name.to_string()));
        let mut builder = Gateway::builder();
        if let Some(limit) = self.body_limit {
            builder = builder.body_limit(limit);
        }
        for route in self.routes {
            let mut models: Vec<Arc<dyn Model>> = Vec::new();
            for m in route.models {
                let key = m.api_key_env.as_deref().map(var).transpose()?;
                let need_key = || {
                    key.clone().ok_or_else(|| {
                        GatewayError::Config(format!(
                            "route `{}`: provider `{}` needs `api_key_env`",
                            route.name, m.provider
                        ))
                    })
                };
                let mut endpoint = match m.provider.as_str() {
                    "openai" => Endpoint::openai(need_key()?),
                    "gemini" => Endpoint::gemini(need_key()?),
                    "anthropic" => Endpoint::anthropic(need_key()?),
                    "openrouter" => Endpoint::openrouter(need_key()?),
                    "ollama" => Endpoint::ollama(),
                    "openai_compatible" => {
                        let base = m.base_url.clone().ok_or_else(|| {
                            GatewayError::Config(format!(
                                "route `{}`: `openai_compatible` needs `base_url`",
                                route.name
                            ))
                        })?;
                        Endpoint::openai_compatible("custom", base, key.clone())
                    }
                    other => {
                        return Err(GatewayError::Config(format!(
                            "route `{}`: unknown provider `{other}`",
                            route.name
                        ))
                        .into());
                    }
                };
                if let Some(base) = &m.base_url
                    && m.provider != "openai_compatible"
                {
                    endpoint = endpoint.with_base_url(base);
                }
                let model = ProviderModel::new(endpoint, &m.model)
                    .map_err(|err| GatewayError::Config(err.to_string()))?;
                models.push(Arc::new(model));
            }
            builder = builder.route_with_fallbacks(route.name, models);
        }
        for key in self.keys {
            builder = builder.key(key.name, var(&key.key_env)?, key.routes);
        }
        Ok(builder)
    }

    /// [`into_gateway_with`](Self::into_gateway_with) the process
    /// environment.
    pub fn into_gateway(self) -> Result<Gateway, ConfigError> {
        self.into_gateway_with(|name| std::env::var(name).ok())
    }

    /// [`into_builder_with`](Self::into_builder_with) the process
    /// environment.
    pub fn into_builder(self) -> Result<GatewayBuilder, ConfigError> {
        self.into_builder_with(|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
        [[routes]]
        name = "support"
        models = [
          { provider = "openai", model = "gpt-5-mini", api_key_env = "TEST_OPENAI_KEY" },
          { provider = "ollama", model = "llama3.2" },
        ]

        [[routes]]
        name = "local"
        models = [{ provider = "openai_compatible", model = "m", base_url = "http://127.0.0.1:9/v1" }]

        [[keys]]
        name = "team-a"
        key_env = "TEST_KEY_A"
        routes = ["support"]
    "#;

    fn env(name: &str) -> Option<String> {
        match name {
            "TEST_OPENAI_KEY" => Some("sk-test-not-real".into()),
            "TEST_KEY_A" => Some("a-long-enough-test-key".into()),
            _ => None,
        }
    }

    #[test]
    fn a_config_builds_a_gateway_with_secrets_from_the_environment() {
        let gateway = Config::parse(CONFIG)
            .unwrap()
            .into_gateway_with(env)
            .unwrap();
        assert!(gateway.has_keys());
        assert_eq!(gateway.inner.routes["support"].len(), 2);
        // The Debug output names routes, never keys.
        let shown = format!("{gateway:?}");
        assert!(
            shown.contains("support") && !shown.contains("a-long-enough"),
            "{shown}"
        );
    }

    #[test]
    fn bad_configs_say_what_is_wrong() {
        let missing = Config::parse(CONFIG)
            .unwrap()
            .into_gateway_with(|n| (n == "TEST_KEY_A").then(|| "a-long-enough-test-key".into()))
            .unwrap_err();
        assert!(matches!(missing, ConfigError::MissingEnv(ref v) if v == "TEST_OPENAI_KEY"));
        for (text, needle) in [
            ("routes = 1", "unreadable"),
            (
                "[[routes]]\nname = \"x\"\nmodels = []\nextra = 1",
                "unreadable",
            ),
            ("[[routes]]\nname = \"x\"\nmodels = []", "no models"),
            (
                "[[routes]]\nname = \"x\"\nmodels = [{ provider = \"nope\", model = \"m\" }]",
                "unknown provider",
            ),
            (
                "[[routes]]\nname = \"x\"\nmodels = [{ provider = \"openai\", model = \"m\" }]",
                "needs `api_key_env`",
            ),
        ] {
            let err = Config::parse(text)
                .and_then(|c| c.into_gateway_with(env))
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{err} (wanted {needle})");
        }
    }
}

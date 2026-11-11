//! The gateway's config file (design 0006 §12.1). Secrets never appear in
//! it: upstream API keys and virtual keys are read from environment
//! variables the file names.

use std::sync::Arc;

use criogen_providers::{
    AzureApi, AzureCredential, AzureModel, Endpoint, KaggleModel, Model, ProviderModel, preset,
};
use serde::Deserialize;

use crate::{BudgetPeriod, Gateway, GatewayBuilder, GatewayError, KeyLimits};

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
    /// Most requests in flight at once, over all keys (default 1024).
    #[serde(default)]
    pub max_concurrent_requests: Option<usize>,
    /// Largest `max_tokens` a client may ask for; larger values are
    /// lowered to it (default 200000).
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Send the upstream's `provider/model` in an `x-criogen-model`
    /// response header (default off: it tells every caller what you use).
    #[serde(default)]
    pub expose_model_header: bool,
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
    /// Send the key over plain `http://` to another machine (a vLLM server
    /// on your LAN). Refused by default.
    #[serde(default)]
    pub allow_insecure_http: bool,
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
    /// Requests per minute (design 0006 §13.2).
    #[serde(default)]
    pub requests_per_minute: Option<u32>,
    /// Input plus output tokens per minute.
    #[serde(default)]
    pub tokens_per_minute: Option<u64>,
    /// A token budget (design 0006 §13.3).
    #[serde(default)]
    pub budget: Option<BudgetConfig>,
    /// Requests in flight at once for this key.
    #[serde(default)]
    pub max_concurrent: Option<u32>,
}

/// `budget = { tokens = 5000000, period = "month" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BudgetConfig {
    /// Input plus output tokens per period.
    pub tokens: u64,
    /// `"day"`, `"month"` (UTC calendar) or `"total"`.
    pub period: String,
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
        if let Some(n) = self.max_concurrent_requests {
            builder = builder.max_concurrent_requests(n);
        }
        if let Some(n) = self.max_tokens {
            builder = builder.max_tokens(n);
        }
        builder = builder.expose_model_header(self.expose_model_header);
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
                if m.provider == "azure" || m.provider == "azure_chat" {
                    // Design 0006 §17: `base_url` (resource name or URL, else
                    // AZURE_OPENAI_ENDPOINT); the key from `api_key_env`, else
                    // the AZURE_* variables (key, service principal, managed
                    // identity).
                    let mut model = AzureModel::from_parts(
                        m.base_url.as_deref(),
                        &m.model,
                        key.clone().map(AzureCredential::api_key),
                    )
                    .map_err(|err| {
                        GatewayError::Config(format!("route `{}`: {}", route.name, err.message))
                    })?;
                    if m.provider == "azure_chat" {
                        model = model.with_api(AzureApi::Chat);
                    }
                    models.push(Arc::new(model));
                    continue;
                }
                if m.provider == "kaggle" {
                    // Design 0020: a Kaggle API token (`api_key_env`, else
                    // KAGGLE_API_TOKEN); the proxy token is minted and renewed.
                    let token = match key.clone() {
                        Some(token) => token,
                        None => var("KAGGLE_API_TOKEN")?,
                    };
                    let mut model = KaggleModel::new(token, &m.model);
                    if let Some(base) = &m.base_url {
                        model = model.with_api_base(base);
                    }
                    models.push(Arc::new(model));
                    continue;
                }
                let mut endpoint = match m.provider.as_str() {
                    kind @ ("openai_compatible" | "anthropic_compatible") => {
                        let base = m.base_url.clone().ok_or_else(|| {
                            GatewayError::Config(format!(
                                "route `{}`: `{kind}` needs `base_url`",
                                route.name
                            ))
                        })?;
                        if kind == "openai_compatible" {
                            Endpoint::openai_compatible("custom", base, key.clone())
                        } else {
                            Endpoint::anthropic_compatible("custom", base, key.clone())
                        }
                    }
                    // Every named provider (design 0006 §4.1).
                    named => match preset(named) {
                        Some(p) if p.key_env.is_some() => p
                            .endpoint(Some(need_key()?))
                            .map_err(|err| GatewayError::Config(err.message))?,
                        Some(p) => p
                            .endpoint(key.clone())
                            .map_err(|err| GatewayError::Config(err.message))?,
                        None => {
                            return Err(GatewayError::Config(format!(
                                "route `{}`: unknown provider `{named}`",
                                route.name
                            ))
                            .into());
                        }
                    },
                };
                if let Some(base) = &m.base_url
                    && !m.provider.ends_with("_compatible")
                {
                    endpoint = endpoint.with_base_url(base);
                }
                if m.allow_insecure_http {
                    endpoint = endpoint.allow_insecure_http();
                }
                let model = ProviderModel::new(endpoint, &m.model)
                    .map_err(|err| GatewayError::Config(err.to_string()))?;
                models.push(Arc::new(model));
            }
            builder = builder.route_with_fallbacks(route.name, models);
        }
        for key in self.keys {
            let mut limits = KeyLimits::new();
            if let Some(n) = key.requests_per_minute {
                limits = limits.requests_per_minute(n);
            }
            if let Some(n) = key.tokens_per_minute {
                limits = limits.tokens_per_minute(n);
            }
            if let Some(n) = key.max_concurrent {
                limits = limits.max_concurrent(n);
            }
            if let Some(budget) = &key.budget {
                let period = BudgetPeriod::parse(&budget.period).ok_or_else(|| {
                    GatewayError::Config(format!(
                        "key `{}`: budget period `{}` is not day, month or total",
                        key.name, budget.period
                    ))
                })?;
                limits = limits.budget(budget.tokens, period);
            }
            builder = builder
                .key(&key.name, var(&key.key_env)?, key.routes)
                .key_limits(key.name, limits);
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

        [[routes]]
        name = "prototype"
        models = [{ provider = "kaggle", model = "google/gemini-3.6-flash" }]

        [[routes]]
        name = "enterprise"
        models = [
          { provider = "azure", model = "gpt-5-mini", base_url = "my-resource", api_key_env = "TEST_AZURE_KEY" },
          { provider = "azure_chat", model = "DeepSeek-V4", base_url = "my-resource", api_key_env = "TEST_AZURE_KEY" },
        ]

        [[keys]]
        name = "team-a"
        key_env = "TEST_KEY_A"
        routes = ["support"]
        requests_per_minute = 60
        tokens_per_minute = 100000
        budget = { tokens = 5000000, period = "month" }
    "#;

    fn env(name: &str) -> Option<String> {
        match name {
            "TEST_OPENAI_KEY" => Some("sk-test-not-real".into()),
            "TEST_KEY_A" => Some("a-long-enough-test-key".into()),
            "KAGGLE_API_TOKEN" => Some("KGAT_not_real".into()),
            "TEST_AZURE_KEY" => Some("azure-not-real".into()),
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
        assert_eq!(gateway.inner.routes["prototype"][0].provider(), "kaggle");
        let enterprise = &gateway.inner.routes["enterprise"];
        assert_eq!(enterprise[0].provider(), "azure");
        assert_eq!(enterprise[1].model(), "DeepSeek-V4");
        assert_eq!(gateway.spent("team-a"), Some(0));
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
            (
                "[[routes]]\nname = \"x\"\nmodels = [{ provider = \"deepseek\", model = \"m\" }]",
                "needs `api_key_env`",
            ),
            (
                "[[routes]]\nname = \"x\"\nmodels = [{ provider = \"anthropic_compatible\", model = \"m\" }]",
                "needs `base_url`",
            ),
        ] {
            let err = Config::parse(text)
                .and_then(|c| c.into_gateway_with(env))
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{err} (wanted {needle})");
        }
    }

    #[test]
    fn named_providers_come_from_the_shared_table() {
        let text = r#"
            [[routes]]
            name = "open"
            models = [
              { provider = "deepseek", model = "deepseek-chat", api_key_env = "TEST_OPENAI_KEY" },
              { provider = "groq", model = "llama-3.3-70b-versatile", api_key_env = "TEST_OPENAI_KEY" },
              { provider = "anthropic_compatible", model = "glm-5.2", base_url = "https://api.z.ai/api/anthropic", api_key_env = "TEST_OPENAI_KEY" },
              { provider = "lmstudio", model = "qwen3" },
            ]
        "#;
        let gateway = Config::parse(text).unwrap().into_gateway_with(env).unwrap();
        let providers: Vec<_> = gateway.inner.routes["open"]
            .iter()
            .map(|m| m.provider())
            .collect();
        assert_eq!(providers, ["deepseek", "groq", "custom", "lmstudio"]);
    }
}

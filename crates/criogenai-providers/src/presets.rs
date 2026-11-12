//! Named providers (design 0006 §4.1): one table of the hosted and local
//! servers CriogenAI knows by name, so `"deepseek:deepseek-chat"` works the
//! same in Rust, both SDKs and the gateway. Each entry is plain data: the
//! wire protocol, the base URL, the environment variable its key usually
//! lives in, and the quirks of that server.

use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::openai_chat::OpenAiQuirks;
use crate::provider::{Endpoint, Protocol};

/// A provider CriogenAI knows by name. See [`PRESETS`] and [`preset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preset {
    /// The name used in model strings (`"groq:llama-3.3-70b-versatile"`)
    /// and as `provider` in messages and errors.
    pub name: &'static str,
    /// The provider's own name, for docs and messages.
    pub title: &'static str,
    /// Base URL of the API.
    pub base_url: &'static str,
    /// Where its key usually lives; `None` for local servers that need none.
    pub key_env: Option<&'static str>,
    /// `true` for the Anthropic Messages protocol, `false` for the OpenAI
    /// protocols (Chat Completions; `openai` itself speaks the Responses API,
    /// design 0006 §16).
    pub anthropic: bool,
}

const fn hosted(
    name: &'static str,
    title: &'static str,
    base_url: &'static str,
    key_env: &'static str,
) -> Preset {
    Preset {
        name,
        title,
        base_url,
        key_env: Some(key_env),
        anthropic: false,
    }
}

const fn local(name: &'static str, title: &'static str, base_url: &'static str) -> Preset {
    Preset {
        name,
        title,
        base_url,
        key_env: None,
        anthropic: false,
    }
}

/// Every named provider, in the order the docs list them.
pub const PRESETS: &[Preset] = &[
    hosted(
        "openai",
        "OpenAI",
        "https://api.openai.com/v1",
        "OPENAI_API_KEY",
    ),
    Preset {
        anthropic: true,
        ..hosted(
            "anthropic",
            "Anthropic",
            "https://api.anthropic.com/v1",
            "ANTHROPIC_API_KEY",
        )
    },
    hosted(
        "gemini",
        "Google Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "GEMINI_API_KEY",
    ),
    hosted(
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        "OPENROUTER_API_KEY",
    ),
    hosted(
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com/v1",
        "DEEPSEEK_API_KEY",
    ),
    hosted(
        "mistral",
        "Mistral AI",
        "https://api.mistral.ai/v1",
        "MISTRAL_API_KEY",
    ),
    hosted("xai", "xAI", "https://api.x.ai/v1", "XAI_API_KEY"),
    hosted(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        "GROQ_API_KEY",
    ),
    hosted(
        "cerebras",
        "Cerebras",
        "https://api.cerebras.ai/v1",
        "CEREBRAS_API_KEY",
    ),
    hosted(
        "together",
        "Together AI",
        "https://api.together.xyz/v1",
        "TOGETHER_API_KEY",
    ),
    hosted(
        "fireworks",
        "Fireworks AI",
        "https://api.fireworks.ai/inference/v1",
        "FIREWORKS_API_KEY",
    ),
    hosted(
        "qwen",
        "Qwen (Alibaba Cloud Model Studio)",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "DASHSCOPE_API_KEY",
    ),
    hosted(
        "moonshot",
        "Moonshot AI (Kimi)",
        "https://api.moonshot.ai/v1",
        "MOONSHOT_API_KEY",
    ),
    hosted(
        "zai",
        "Z.ai (GLM)",
        "https://api.z.ai/api/paas/v4",
        "ZAI_API_KEY",
    ),
    local("ollama", "Ollama", "http://localhost:11434/v1"),
    local("lmstudio", "LM Studio", "http://localhost:1234/v1"),
];

/// The named provider `name`, if CriogenAI knows it.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

impl Preset {
    /// The endpoint for this provider with `api_key`. Fails with
    /// `invalid_request` (nothing sent) when the provider needs a key and
    /// none is given; the message names the variable, never a value.
    pub fn endpoint(&self, api_key: Option<String>) -> Result<Endpoint, ModelError> {
        let api_key = api_key.filter(|k| !k.is_empty());
        if let Some(var) = self.key_env
            && api_key.is_none()
        {
            return Err(ModelError::new(
                ModelErrorKind::InvalidRequest,
                format!("no API key for {}: set {var}, or pass the key", self.name),
            )
            .with_dispatch(Dispatch::NotSent)
            .for_model(self.name, ""));
        }
        let key = || api_key.clone().unwrap_or_default();
        let mut endpoint = match self.name {
            "openai" => Endpoint::openai(key()),
            "anthropic" => Endpoint::anthropic(key()),
            "gemini" => Endpoint::gemini(key()),
            _ if self.anthropic => {
                Endpoint::anthropic_compatible(self.name, self.base_url, api_key.clone())
            }
            _ => Endpoint::openai_compatible(self.name, self.base_url, api_key.clone()),
        };
        if let Protocol::OpenAiChat(quirks) = &mut endpoint.protocol {
            tune(self.name, quirks);
        }
        Ok(endpoint)
    }
}

/// Per-server differences (design 0006 §4.1). Anything not listed keeps
/// the defaults that work with most compatible servers.
fn tune(name: &str, quirks: &mut OpenAiQuirks) {
    match name {
        // Thinking models want their earlier `reasoning_content` back on
        // assistant messages that made tool calls.
        "deepseek" | "moonshot" => quirks.reasoning_echo = Some("reasoning_content".into()),
        // Strict about request fields: Cerebras documents a 400 for fields
        // it does not support; for Mistral we do not send it, to be safe.
        // Streams then carry no token counts.
        "mistral" | "cerebras" => quirks.stream_usage = false,
        "xai" | "groq" | "together" | "fireworks" => quirks.parallel_tool_calls = true,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_lowercase() {
        let mut names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        assert!(
            names
                .iter()
                .all(|n| n.chars().all(|c| c.is_ascii_lowercase()))
        );
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PRESETS.len());
    }

    #[test]
    fn hosted_presets_use_https_and_local_ones_need_no_key() {
        for p in PRESETS {
            if p.key_env.is_some() {
                assert!(p.base_url.starts_with("https://"), "{}", p.name);
            } else {
                assert!(p.base_url.starts_with("http://localhost"), "{}", p.name);
            }
            assert!(!p.base_url.ends_with('/'), "{}", p.name);
        }
    }

    #[test]
    fn a_hosted_preset_without_a_key_names_the_variable() {
        let err = preset("deepseek").unwrap().endpoint(None).unwrap_err();
        assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
        assert!(err.message.contains("DEEPSEEK_API_KEY"), "{}", err.message);
        assert!(
            preset("groq")
                .unwrap()
                .endpoint(Some(String::new()))
                .is_err()
        );
    }

    #[test]
    fn every_preset_builds_an_endpoint_under_its_own_name() {
        for p in PRESETS {
            let endpoint = p.endpoint(p.key_env.map(|_| "k".to_string())).unwrap();
            assert_eq!(endpoint.name, p.name);
            assert_eq!(endpoint.base_url, p.base_url);
            assert_eq!(
                matches!(endpoint.protocol, Protocol::AnthropicMessages(_)),
                p.anthropic
            );
            assert_eq!(
                endpoint.api_key.is_some(),
                p.key_env.is_some(),
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn the_old_presets_are_unchanged() {
        assert_eq!(
            preset("openai")
                .unwrap()
                .endpoint(Some("k".into()))
                .unwrap(),
            Endpoint::openai("k")
        );
        assert_eq!(
            preset("anthropic")
                .unwrap()
                .endpoint(Some("k".into()))
                .unwrap(),
            Endpoint::anthropic("k")
        );
        assert_eq!(
            preset("gemini")
                .unwrap()
                .endpoint(Some("k".into()))
                .unwrap(),
            Endpoint::gemini("k")
        );
        assert_eq!(
            preset("openrouter")
                .unwrap()
                .endpoint(Some("k".into()))
                .unwrap(),
            Endpoint::openrouter("k")
        );
        assert_eq!(
            preset("ollama").unwrap().endpoint(None).unwrap(),
            Endpoint::ollama()
        );
        assert_eq!(
            preset("lmstudio").unwrap().endpoint(None).unwrap(),
            Endpoint::lmstudio()
        );
    }

    #[test]
    fn quirks_follow_the_server() {
        let quirks = |name: &str| match preset(name)
            .unwrap()
            .endpoint(Some("k".into()))
            .unwrap()
            .protocol
        {
            Protocol::OpenAiChat(q) => q,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            quirks("deepseek").reasoning_echo.as_deref(),
            Some("reasoning_content")
        );
        assert!(!quirks("mistral").stream_usage);
        assert!(quirks("groq").parallel_tool_calls);
        assert_eq!(quirks("qwen"), OpenAiQuirks::default());
    }

    #[test]
    fn unknown_names_are_none() {
        assert!(preset("azure").is_none());
        assert!(preset("OpenAI").is_none());
    }
}

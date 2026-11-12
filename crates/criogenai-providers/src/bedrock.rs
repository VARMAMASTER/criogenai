//! Amazon Bedrock with an API key (design 0006 §18): Claude over the
//! Anthropic Messages API, other makers' models over Chat Completions or
//! Responses, all on `bedrock-runtime`. No new protocol and no AWS SDK.

use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::provider::Endpoint;

/// Which API a Bedrock model is called with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BedrockApi {
    /// Messages for Anthropic's model ids, Chat Completions for the rest.
    #[default]
    Auto,
    /// The Anthropic Messages API (Claude).
    Messages,
    /// Chat Completions.
    Chat,
    /// The Responses API (models whose card lists it).
    Responses,
}

impl BedrockApi {
    /// `auto`, `messages`, `chat` or `responses`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "auto" => Self::Auto,
            "messages" => Self::Messages,
            "chat" => Self::Chat,
            "responses" => Self::Responses,
            _ => return None,
        })
    }

    /// The API `Auto` resolves to for `model`.
    fn resolve(self, model: &str) -> Self {
        if self != Self::Auto {
            return self;
        }
        if is_anthropic(model) {
            Self::Messages
        } else {
            Self::Chat
        }
    }
}

/// `anthropic.…`, or with an inference-profile prefix (`us.anthropic.…`,
/// `global.anthropic.…`), or an ARN that names one.
fn is_anthropic(model: &str) -> bool {
    let id = model.rsplit('/').next().unwrap_or(model);
    id.starts_with("anthropic.")
        || id
            .split_once('.')
            .is_some_and(|(_, rest)| rest.starts_with("anthropic."))
}

/// The Bedrock endpoint for `model` in `region` with an API key. The region
/// must look like one (`us-east-1`): it becomes part of the host name.
pub fn bedrock_endpoint(
    region: &str,
    model: &str,
    api_key: impl Into<String>,
    api: BedrockApi,
) -> Result<Endpoint, ModelError> {
    let region = region.trim();
    if region.is_empty()
        || region.len() > 32
        || !region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ModelError::new(
            ModelErrorKind::InvalidRequest,
            format!("`{region}` is not an AWS region name (like `us-east-1`)"),
        )
        .with_dispatch(Dispatch::NotSent)
        .for_model("bedrock", model));
    }
    let host = format!("https://bedrock-runtime.{region}.amazonaws.com");
    let key = Some(api_key.into());
    let mut endpoint = match api.resolve(model) {
        BedrockApi::Messages => {
            let mut endpoint =
                Endpoint::anthropic_compatible("bedrock", format!("{host}/anthropic/v1"), key);
            let caps = &mut endpoint.capabilities;
            caps.tools = Some(true);
            caps.parallel_tool_calls = Some(true);
            caps.vision = Some(true);
            caps.reasoning = Some(true);
            caps.tool_result_images = Some(true);
            endpoint
        }
        BedrockApi::Responses => {
            Endpoint::openai_responses_compatible("bedrock", format!("{host}/openai/v1"), key)
        }
        BedrockApi::Chat | BedrockApi::Auto => {
            Endpoint::openai_compatible("bedrock", format!("{host}/openai/v1"), key)
        }
    };
    endpoint.capabilities.tools = Some(true);
    Ok(endpoint)
}

/// A Bedrock endpoint from what is given, the rest from the environment
/// (what the SDKs' `bedrock:` models use): `region`, else `AWS_REGION`, else
/// `AWS_DEFAULT_REGION`; `api_key`, else `AWS_BEARER_TOKEN_BEDROCK`. There
/// is no default region. Messages name the variables, never a value.
pub fn bedrock_from_env(
    model: &str,
    region: Option<&str>,
    api_key: Option<String>,
    api: BedrockApi,
) -> Result<Endpoint, ModelError> {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let missing = |what: &str| {
        ModelError::new(ModelErrorKind::Auth, what)
            .with_dispatch(Dispatch::NotSent)
            .for_model("bedrock", model)
    };
    let region = match region.map(str::trim).filter(|r| !r.is_empty()) {
        Some(region) => region.to_string(),
        None => var("AWS_REGION")
            .or_else(|| var("AWS_DEFAULT_REGION"))
            .ok_or_else(|| {
                missing(
                    "no AWS region: pass `region`, or set AWS_REGION (Bedrock picks none for you)",
                )
            })?,
    };
    let key = api_key
        .filter(|k| !k.trim().is_empty())
        .or_else(|| var("AWS_BEARER_TOKEN_BEDROCK"))
        .ok_or_else(|| {
            missing(
                "no Amazon Bedrock API key: set AWS_BEARER_TOKEN_BEDROCK, or pass the key \
                 (IAM roles and SigV4 are not supported yet)",
            )
        })?;
    bedrock_endpoint(&region, model, key, api)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Protocol;

    #[test]
    fn anthropic_model_ids_are_recognised_with_and_without_profiles() {
        for id in [
            "anthropic.claude-sonnet-5",
            "us.anthropic.claude-sonnet-5",
            "global.anthropic.claude-opus-5-5",
            "arn:aws:bedrock:us-east-1:123:inference-profile/eu.anthropic.claude-haiku-5-5",
        ] {
            assert!(is_anthropic(id), "{id}");
        }
        for id in [
            "openai.gpt-oss-120b-1:0",
            "meta.llama4-maverick",
            "us.deepseek.r1-v1:0",
        ] {
            assert!(!is_anthropic(id), "{id}");
        }
    }

    #[test]
    fn each_api_gets_its_base_url_and_protocol() {
        let claude = bedrock_endpoint(
            "eu-west-1",
            "eu.anthropic.claude-sonnet-5",
            "k",
            BedrockApi::Auto,
        )
        .unwrap();
        assert_eq!(
            claude.base_url,
            "https://bedrock-runtime.eu-west-1.amazonaws.com/anthropic/v1"
        );
        assert!(matches!(claude.protocol, Protocol::AnthropicMessages(_)));
        let oss = bedrock_endpoint(
            "us-east-1",
            "openai.gpt-oss-120b-1:0",
            "k",
            BedrockApi::Auto,
        )
        .unwrap();
        assert_eq!(
            oss.base_url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1"
        );
        assert!(matches!(oss.protocol, Protocol::OpenAiChat(_)));
        let responses = bedrock_endpoint("us-east-1", "m", "k", BedrockApi::Responses).unwrap();
        assert!(matches!(responses.protocol, Protocol::OpenAiResponses(_)));
        assert_eq!(claude.name, "bedrock");
    }

    #[test]
    fn a_region_cannot_change_the_host() {
        for bad in ["", "us-east-1.evil.com", "US-EAST-1", "us east", "a/b"] {
            let err = bedrock_endpoint(bad, "m", "k", BedrockApi::Auto).unwrap_err();
            assert_eq!(err.kind, ModelErrorKind::InvalidRequest, "{bad}");
        }
    }
}

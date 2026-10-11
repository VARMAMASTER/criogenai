//! The provider contract: model request, response and capabilities
//! (design 0002 §7).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::{Extras, FinishReason, Message, Usage};
use crate::tool::ToolSpec;

/// Everything a model call needs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ModelRequest {
    /// The conversation.
    pub messages: Vec<Message>,
    /// Tools the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Whether and which tool the model must call.
    #[serde(default, skip_serializing_if = "ToolChoice::is_auto")]
    pub tool_choice: ToolChoice,
    /// Structured output (0004 §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputSpec>,
    /// Sampling and limits.
    #[serde(default, skip_serializing_if = "Settings::is_default")]
    pub settings: Settings,
    /// Provider-specific request fields, keyed by provider.
    #[serde(default, skip_serializing_if = "Extras::is_empty")]
    pub extras: Extras,
}

impl ModelRequest {
    /// A request with these messages and default settings.
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            ..Self::default()
        }
    }

    /// Sets [`tools`](Self::tools).
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    /// Sets [`settings`](Self::settings).
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }
}

/// Whether and which tool the model must call. JSON: `"auto"`, `"none"`,
/// `"required"` or `{"name": "…"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ToolChoiceRepr", into = "ToolChoiceRepr")]
#[non_exhaustive]
pub enum ToolChoice {
    /// The model decides.
    #[default]
    Auto,
    /// No tool calls.
    None,
    /// At least one tool call.
    Required,
    /// Exactly this tool.
    Tool(String),
}

impl ToolChoice {
    fn is_auto(&self) -> bool {
        *self == ToolChoice::Auto
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ToolChoiceRepr {
    Mode(ToolChoiceMode),
    Named { name: String },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolChoiceMode {
    Auto,
    None,
    Required,
}

impl From<ToolChoiceRepr> for ToolChoice {
    fn from(repr: ToolChoiceRepr) -> Self {
        match repr {
            ToolChoiceRepr::Mode(ToolChoiceMode::Auto) => Self::Auto,
            ToolChoiceRepr::Mode(ToolChoiceMode::None) => Self::None,
            ToolChoiceRepr::Mode(ToolChoiceMode::Required) => Self::Required,
            ToolChoiceRepr::Named { name } => Self::Tool(name),
        }
    }
}

impl From<ToolChoice> for ToolChoiceRepr {
    fn from(choice: ToolChoice) -> Self {
        match choice {
            ToolChoice::Auto => Self::Mode(ToolChoiceMode::Auto),
            ToolChoice::None => Self::Mode(ToolChoiceMode::None),
            ToolChoice::Required => Self::Mode(ToolChoiceMode::Required),
            ToolChoice::Tool(name) => Self::Named { name },
        }
    }
}

/// Structured output: the answer must match `schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OutputSpec {
    /// JSON Schema of the answer.
    pub schema: Value,
    /// How to obtain it; chosen from the capabilities when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<OutputStrategy>,
}

/// How structured output is obtained (0004 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutputStrategy {
    /// The provider's own structured-output feature.
    Native,
    /// A forced call to the reserved `final_output` tool.
    Tool,
}

/// Sampling and limits. Unset fields use the provider's defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Settings {
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Whether the model may request several tool calls at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Reasoning effort or budget, mapped per protocol (0006 §9 Q2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}

impl Settings {
    fn is_default(&self) -> bool {
        *self == Settings::default()
    }
}

/// A neutral reasoning setting. JSON: `{"effort": "medium"}` or
/// `{"budget_tokens": 8000}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Reasoning {
    /// Relative effort.
    Effort {
        /// Low, medium or high.
        effort: ReasoningEffort,
    },
    /// A token budget.
    Budget {
        /// Tokens the model may spend reasoning.
        budget_tokens: u32,
    },
}

/// Relative reasoning effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningEffort {
    /// Little reasoning.
    Low,
    /// Moderate reasoning.
    Medium,
    /// Extensive reasoning.
    High,
}

/// A model's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ModelResponse {
    /// The assistant message.
    pub message: Message,
    /// Tokens used.
    #[serde(default)]
    pub usage: Usage,
    /// Why the model stopped.
    pub finish_reason: FinishReason,
}

impl ModelResponse {
    /// A response. `usage` and `finish_reason` are also copied into the
    /// message's metadata.
    pub fn new(mut message: Message, usage: Usage, finish_reason: FinishReason) -> Self {
        message.metadata.usage = usage;
        message.metadata.finish_reason = Some(finish_reason);
        Self {
            message,
            usage,
            finish_reason,
        }
    }
}

/// What a model supports. Declared by each adapter, never guessed from model
/// names. `None` means unknown: the feature is attempted and the provider
/// decides (0006 §10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Capabilities {
    /// Tool calling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Several tool calls in one answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// The provider's own structured output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_structured_output: Option<bool>,
    /// Streaming responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Image input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Reasoning output or settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Images inside tool results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_images: Option<bool>,
    /// Context window size in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_choice_json_forms() {
        for (choice, value) in [
            (ToolChoice::Auto, json!("auto")),
            (ToolChoice::None, json!("none")),
            (ToolChoice::Required, json!("required")),
            (ToolChoice::Tool("search".into()), json!({"name": "search"})),
        ] {
            assert_eq!(serde_json::to_value(&choice).unwrap(), value);
            assert_eq!(serde_json::from_value::<ToolChoice>(value).unwrap(), choice);
        }
        assert!(serde_json::from_value::<ToolChoice>(json!("sometimes")).is_err());
    }

    #[test]
    fn reasoning_json_forms() {
        let effort: Reasoning = serde_json::from_value(json!({"effort": "high"})).unwrap();
        assert_eq!(
            effort,
            Reasoning::Effort {
                effort: ReasoningEffort::High
            }
        );
        let budget: Reasoning = serde_json::from_value(json!({"budget_tokens": 8000})).unwrap();
        assert_eq!(
            budget,
            Reasoning::Budget {
                budget_tokens: 8000
            }
        );
    }

    #[test]
    fn minimal_request_round_trips_compactly() {
        let value = json!({"messages": [
            {"id": "m1", "role": "user", "content": [{"type": "text", "text": "hi"}]}
        ]});
        let request: ModelRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(request.tool_choice, ToolChoice::Auto);
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
    }

    #[test]
    fn unknown_capabilities_are_none() {
        let caps: Capabilities = serde_json::from_value(json!({"tools": true})).unwrap();
        assert_eq!(caps.tools, Some(true));
        assert_eq!(caps.vision, None);
    }
}

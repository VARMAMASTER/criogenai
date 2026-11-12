//! Structured output (design 0004 §6).

use criogenai_providers::{OutputStrategy, ToolSpec};
use serde_json::{Value, json};

use crate::schema;

/// Name of the reserved tool used by the `tool` strategy.
pub const FINAL_OUTPUT_TOOL: &str = "final_output";

/// The output a run must produce, and how it is obtained.
#[derive(Debug, Clone)]
pub(crate) struct OutputConfig {
    /// The schema as given.
    schema: Value,
    /// Tool arguments must be an object: other schemas are wrapped as
    /// `{"value": …}` for the tool strategy.
    wrapped: bool,
    pub strategy: OutputStrategy,
}

impl OutputConfig {
    pub(crate) fn new(schema: Value, strategy: OutputStrategy) -> Self {
        let wrapped =
            strategy == OutputStrategy::Tool && schema.get("type") != Some(&json!("object"));
        Self {
            schema,
            wrapped,
            strategy,
        }
    }

    /// The schema as given (native strategy).
    pub(crate) fn schema(&self) -> &Value {
        &self.schema
    }

    /// The reserved `final_output` tool (tool strategy).
    pub(crate) fn tool_spec(&self) -> ToolSpec {
        let input_schema = if self.wrapped {
            json!({
                "type": "object",
                "properties": {"value": self.schema},
                "required": ["value"],
                "additionalProperties": false
            })
        } else {
            self.schema.clone()
        };
        ToolSpec::new(
            FINAL_OUTPUT_TOOL,
            "Give your final answer by calling this tool once, with the complete answer. \
             Do not call it until you have everything you need.",
            input_schema,
        )
    }

    /// Checks `final_output` arguments; returns the output value.
    pub(crate) fn accept_tool_args(&self, args: &Value) -> Result<Value, Vec<String>> {
        let spec = self.tool_spec();
        let problems = schema::validate(&spec.input_schema, args);
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(if self.wrapped {
            args.get("value").cloned().unwrap_or(Value::Null)
        } else {
            args.clone()
        })
    }

    /// Parses and checks a native (JSON mode) answer.
    pub(crate) fn accept_text(&self, text: &str) -> Result<Value, Vec<String>> {
        let value: Value = serde_json::from_str(strip_fence(text))
            .map_err(|err| vec![format!("the answer is not valid JSON: {err}")])?;
        let problems = schema::validate(&self.schema, &value);
        if problems.is_empty() {
            Ok(value)
        } else {
            Err(problems)
        }
    }
}

/// Removes a surrounding Markdown code fence (```json … ```), which models
/// add even in JSON mode.
fn strip_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest.split_once('\n').map_or("", |(_, body)| body);
    rest.trim_end().strip_suffix("```").unwrap_or(rest).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person() -> Value {
        json!({"type": "object", "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
               "required": ["name", "age"], "additionalProperties": false})
    }

    #[test]
    fn object_schemas_are_used_directly() {
        let config = OutputConfig::new(person(), OutputStrategy::Tool);
        assert_eq!(config.tool_spec().input_schema, person());
        let ok = config
            .accept_tool_args(&json!({"name": "Ada", "age": 36}))
            .unwrap();
        assert_eq!(ok["name"], "Ada");
        let err = config
            .accept_tool_args(&json!({"name": "Ada"}))
            .unwrap_err();
        assert_eq!(err, ["age: required"]);
    }

    #[test]
    fn other_schemas_are_wrapped_for_the_tool_strategy() {
        let list = json!({"type": "array", "items": {"type": "string"}});
        let config = OutputConfig::new(list.clone(), OutputStrategy::Tool);
        assert_eq!(config.tool_spec().input_schema["properties"]["value"], list);
        assert_eq!(
            config
                .accept_tool_args(&json!({"value": ["a", "b"]}))
                .unwrap(),
            json!(["a", "b"])
        );
        assert!(config.accept_tool_args(&json!({"value": [1]})).is_err());
        // The native strategy uses the schema as is.
        let native = OutputConfig::new(list, OutputStrategy::Native);
        assert_eq!(native.accept_text("[\"x\"]").unwrap(), json!(["x"]));
    }

    #[test]
    fn native_answers_may_come_in_a_code_fence() {
        let config = OutputConfig::new(person(), OutputStrategy::Native);
        let fenced = "```json\n{\"name\": \"Ada\", \"age\": 36}\n```";
        assert_eq!(config.accept_text(fenced).unwrap()["age"], 36);
        let err = config.accept_text("Ada is 36").unwrap_err();
        assert!(err[0].starts_with("the answer is not valid JSON"));
        let err = config.accept_text("{\"name\": 1, \"age\": 2}").unwrap_err();
        assert_eq!(err, ["name: expected string, got 1"]);
    }
}

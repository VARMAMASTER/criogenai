//! Tool definitions (design 0002 §3).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tool names reserved by Criogen. `final_output` carries structured
/// output (0004 §6).
pub const RESERVED_TOOL_NAMES: &[&str] = &["final_output"];

/// A tool the model may call, as JSON. The SDKs infer it from a function;
/// MCP tools arrive in this form directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolSpec {
    /// `^[a-zA-Z0-9_-]{1,64}$`, unique per agent.
    pub name: String,
    /// What the tool does, for the model.
    #[serde(default)]
    pub description: String,
    /// JSON Schema of the arguments; must be an object schema.
    pub input_schema: Value,
    /// Where the tool runs.
    #[serde(default)]
    pub kind: ToolKind,
    /// Hints that drive runtime policy, e.g. whether retries are safe.
    #[serde(default, skip_serializing_if = "ToolAnnotations::is_default")]
    pub annotations: ToolAnnotations,
    /// Time limit per call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// At most this many calls to this tool run at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// A successful call ends the agent loop (0004 §4).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub return_direct: bool,
    /// Results longer than this are cut (0002 §4.3). Default 100,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_result_chars: Option<u64>,
}

impl ToolSpec {
    /// A local function tool.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            kind: ToolKind::Function,
            annotations: ToolAnnotations::default(),
            timeout_ms: None,
            max_concurrency: None,
            return_direct: false,
            max_result_chars: None,
        }
    }

    /// Sets [`annotations`](Self::annotations).
    pub fn with_annotations(mut self, annotations: ToolAnnotations) -> Self {
        self.annotations = annotations;
        self
    }
}

/// Where a tool runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolKind {
    /// A function in the app (Python, TypeScript or Rust).
    #[default]
    Function,
    /// A tool served by an MCP server.
    Mcp,
    /// Executed by the model provider, never locally.
    Hosted,
}

/// Behaviour hints. They decide, for example, whether a tool may be retried
/// (0002 §5.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ToolAnnotations {
    /// Has no side effects.
    pub read_only: bool,
    /// Calling twice with the same arguments has the same effect as once.
    pub idempotent: bool,
    /// May destroy data; apps may require approval.
    pub destructive: bool,
}

impl ToolAnnotations {
    /// A tool without side effects (also safe to retry).
    pub fn read_only() -> Self {
        Self {
            read_only: true,
            ..Self::default()
        }
    }

    /// A tool that is safe to call again with the same arguments.
    pub fn idempotent() -> Self {
        Self {
            idempotent: true,
            ..Self::default()
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether transient failures may be retried by default (0003 §5.1).
    pub fn retry_safe(&self) -> bool {
        self.read_only || self.idempotent
    }
}

/// A problem with a set of tool definitions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ToolIssue {
    /// The name breaks `^[a-zA-Z0-9_-]{1,64}$`.
    #[error("tool name `{0}` must be 1-64 characters of a-z, A-Z, 0-9, `_` or `-`")]
    InvalidName(String),
    /// Two tools share a name.
    #[error("tool `{0}` is defined more than once")]
    DuplicateName(String),
    /// The name is reserved by Criogen.
    #[error("tool name `{0}` is reserved")]
    ReservedName(String),
    /// `input_schema` is not an object schema.
    #[error("tool `{name}`: input_schema {reason}")]
    InvalidSchema {
        /// The tool.
        name: String,
        /// What is wrong.
        reason: String,
    },
    /// A numeric setting is zero.
    #[error("tool `{name}`: {field} must be at least 1")]
    ZeroLimit {
        /// The tool.
        name: String,
        /// The field.
        field: &'static str,
    },
}

/// Checks a set of tools at registration time (0002 §3). Returns every issue
/// found, in tool order.
pub fn validate_tools(tools: &[ToolSpec]) -> Result<(), Vec<ToolIssue>> {
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    for tool in tools {
        let name = &tool.name;
        if !valid_name(name) {
            issues.push(ToolIssue::InvalidName(name.clone()));
        } else if RESERVED_TOOL_NAMES.contains(&name.as_str()) {
            issues.push(ToolIssue::ReservedName(name.clone()));
        }
        if !seen.insert(name.as_str()) {
            issues.push(ToolIssue::DuplicateName(name.clone()));
        }
        if let Err(reason) = check_schema(&tool.input_schema) {
            issues.push(ToolIssue::InvalidSchema {
                name: name.clone(),
                reason,
            });
        }
        for (field, value) in [
            ("timeout_ms", tool.timeout_ms),
            ("max_concurrency", tool.max_concurrency.map(u64::from)),
            ("max_result_chars", tool.max_result_chars),
        ] {
            if value == Some(0) {
                issues.push(ToolIssue::ZeroLimit {
                    name: name.clone(),
                    field,
                });
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn check_schema(schema: &Value) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        return Err("must be a JSON object".into());
    };
    match object.get("type") {
        Some(Value::String(t)) if t == "object" => {}
        Some(other) => return Err(format!("must have \"type\": \"object\", found {other}")),
        None => return Err("must have \"type\": \"object\"".into()),
    }
    if let Some(properties) = object.get("properties")
        && !properties.is_object()
    {
        return Err("`properties` must be an object".into());
    }
    if let Some(required) = object.get("required")
        && !required
            .as_array()
            .is_some_and(|r| r.iter().all(Value::is_string))
    {
        return Err("`required` must be a list of strings".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]})
    }

    #[test]
    fn valid_tools_pass_and_defaults_apply() {
        let tool: ToolSpec =
            serde_json::from_value(json!({"name": "search", "input_schema": schema()})).unwrap();
        assert_eq!(tool.kind, ToolKind::Function);
        assert!(!tool.annotations.retry_safe());
        assert!(validate_tools(std::slice::from_ref(&tool)).is_ok());
        // Defaults are omitted when serialized.
        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            json!({"name": "search", "description": "", "input_schema": schema(), "kind": "function"})
        );
    }

    #[test]
    fn every_issue_is_reported() {
        let mut zero = ToolSpec::new("slow", "", schema());
        zero.max_concurrency = Some(0);
        let tools = [
            ToolSpec::new("search docs", "", schema()),
            ToolSpec::new("final_output", "", schema()),
            ToolSpec::new("a", "", schema()),
            ToolSpec::new("a", "", json!({"type": "string"})),
            ToolSpec::new("x".repeat(65), "", schema()),
            ToolSpec::new("b", "", json!({"type": "object", "required": "q"})),
            zero,
        ];
        let issues = validate_tools(&tools).unwrap_err();
        assert_eq!(issues.len(), 7, "{issues:#?}");
        assert!(matches!(&issues[0], ToolIssue::InvalidName(n) if n == "search docs"));
        assert!(matches!(&issues[1], ToolIssue::ReservedName(_)));
        assert!(matches!(&issues[2], ToolIssue::DuplicateName(n) if n == "a"));
        assert!(matches!(&issues[3], ToolIssue::InvalidSchema { .. }));
        assert!(matches!(&issues[4], ToolIssue::InvalidName(_)));
        assert!(
            matches!(&issues[5], ToolIssue::InvalidSchema { reason, .. } if reason.contains("required"))
        );
        assert!(matches!(
            &issues[6],
            ToolIssue::ZeroLimit {
                field: "max_concurrency",
                ..
            }
        ));
    }

    #[test]
    fn names_that_differ_by_case_are_different_tools() {
        let tools = [
            ToolSpec::new("Search", "", schema()),
            ToolSpec::new("search", "", schema()),
        ];
        assert!(validate_tools(&tools).is_ok());
    }
}

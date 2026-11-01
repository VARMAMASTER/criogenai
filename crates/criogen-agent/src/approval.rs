//! Human approval of tool calls (design 0004 §7).
//!
//! When the model asks for a gated tool, the agent's `review` node suspends
//! the run **before any call of that message runs** (un-gated calls in the
//! same message wait too). The app shows the [`ApprovalRequest`]s to a
//! person and resumes with one [`Decision`] per request.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::schema;

/// The kinds of decision a reviewer can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecisionKind {
    /// Run the call as the model wrote it.
    Approve,
    /// Run a changed call (other arguments, or another tool).
    Edit,
    /// Do not run it; the model is told it was rejected.
    Reject,
    /// Do not run it; the reviewer's answer is the result.
    Respond,
}

impl DecisionKind {
    /// All four kinds.
    pub const ALL: [DecisionKind; 4] = [Self::Approve, Self::Edit, Self::Reject, Self::Respond];
}

/// A reviewer's decision on one tool call. JSON:
/// `{"type": "approve"}`, `{"type": "edit", "args": {..}, "name": ..}`,
/// `{"type": "reject", "message": ..}`, `{"type": "respond", "message": ..}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decision {
    /// Run as is.
    Approve,
    /// Run with these arguments (and optionally another tool). The result
    /// tells the model what actually ran.
    Edit {
        /// Another tool to call instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The arguments to use.
        args: Map<String, Value>,
    },
    /// Do not run; `message` (or a default) explains why to the model.
    Reject {
        /// Shown to the model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Do not run; the reviewer answers in place of the tool.
    Respond {
        /// The answer the model receives as the tool's result.
        message: String,
    },
}

impl Decision {
    /// Approve the call.
    pub fn approve() -> Self {
        Self::Approve
    }

    /// Run with other arguments.
    pub fn edit(args: Map<String, Value>) -> Self {
        Self::Edit { name: None, args }
    }

    /// Reject with a message for the model.
    pub fn reject(message: impl Into<String>) -> Self {
        Self::Reject {
            message: Some(message.into()),
        }
    }

    /// Answer in place of the tool.
    pub fn respond(message: impl Into<String>) -> Self {
        Self::Respond {
            message: message.into(),
        }
    }

    /// Its kind.
    pub fn kind(&self) -> DecisionKind {
        match self {
            Self::Approve => DecisionKind::Approve,
            Self::Edit { .. } => DecisionKind::Edit,
            Self::Reject { .. } => DecisionKind::Reject,
            Self::Respond { .. } => DecisionKind::Respond,
        }
    }
}

/// One call waiting for a decision (the interrupt payload holds a list).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// The tool call's id; decisions are keyed by it.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// Arguments the model wrote.
    pub args: Map<String, Value>,
    /// Text for the reviewer, from [`ApprovalRule::describe`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Decisions the reviewer may make.
    pub allowed: Vec<DecisionKind>,
}

/// The interrupt payload's `kind`.
pub const APPROVAL_INTERRUPT: &str = "tool_approval";

type Predicate = Arc<dyn Fn(&str, &Map<String, Value>) -> bool + Send + Sync>;
type Describe = Arc<dyn Fn(&str, &Map<String, Value>) -> String + Send + Sync>;

/// Which calls need approval, and what the reviewer may decide.
#[derive(Clone)]
pub struct ApprovalRule {
    pattern: String,
    decisions: Vec<DecisionKind>,
    when: Option<Predicate>,
    describe: Option<Describe>,
}

impl std::fmt::Debug for ApprovalRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalRule")
            .field("pattern", &self.pattern)
            .field("decisions", &self.decisions)
            .field("when", &self.when.is_some())
            .finish_non_exhaustive()
    }
}

impl ApprovalRule {
    /// Gate tools whose name matches `pattern` (`*` matches any text, e.g.
    /// `delete_*`). All four decisions are allowed.
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            decisions: DecisionKind::ALL.to_vec(),
            when: None,
            describe: None,
        }
    }

    /// Restricts the allowed decisions.
    pub fn decisions(mut self, kinds: impl IntoIterator<Item = DecisionKind>) -> Self {
        self.decisions = kinds.into_iter().collect();
        self
    }

    /// Gate only calls for which `f(tool, args)` is true (e.g. payments
    /// above a threshold).
    pub fn when(
        mut self,
        f: impl Fn(&str, &Map<String, Value>) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.when = Some(Arc::new(f));
        self
    }

    /// Text shown to the reviewer for each call.
    pub fn describe(
        mut self,
        f: impl Fn(&str, &Map<String, Value>) -> String + Send + Sync + 'static,
    ) -> Self {
        self.describe = Some(Arc::new(f));
        self
    }
}

/// Human-approval settings for an agent.
#[derive(Debug, Clone, Default)]
pub struct Approval {
    rules: Vec<ApprovalRule>,
}

impl Approval {
    /// No gated tools yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Gate tools matching `pattern`, with all decisions allowed.
    pub fn tool(self, pattern: impl Into<String>) -> Self {
        self.rule(ApprovalRule::new(pattern))
    }

    /// Adds a rule. The first matching rule applies.
    pub fn rule(mut self, rule: ApprovalRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Build-time checks: every rule names at least one tool and allows at
    /// least one decision.
    pub(crate) fn check(&self, tools: &[&str]) -> Result<(), String> {
        for rule in &self.rules {
            if rule.decisions.is_empty() {
                return Err(format!(
                    "approval rule `{}` allows no decisions",
                    rule.pattern
                ));
            }
            if !tools.iter().any(|t| glob(&rule.pattern, t)) {
                return Err(format!("approval rule `{}` matches no tool", rule.pattern));
            }
        }
        Ok(())
    }

    /// The rule gating this call, if any.
    pub(crate) fn rule_for(&self, tool: &str, args: &Map<String, Value>) -> Option<&ApprovalRule> {
        self.rules.iter().find(|rule| {
            glob(&rule.pattern, tool) && rule.when.as_ref().is_none_or(|when| when(tool, args))
        })
    }

    /// The approval request for a call, if it is gated.
    pub(crate) fn request(
        &self,
        call_id: &str,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Option<ApprovalRequest> {
        let rule = self.rule_for(tool, args)?;
        Some(ApprovalRequest {
            call_id: call_id.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
            description: rule.describe.as_ref().map(|d| d(tool, args)),
            allowed: rule.decisions.clone(),
        })
    }
}

/// Checks decisions against the requests they answer (0004 §7.2): exactly
/// one per request, of an allowed kind, with valid edited arguments.
pub(crate) fn check_decisions(
    requests: &[ApprovalRequest],
    decisions: &BTreeMap<String, Decision>,
    schemas: &HashMap<String, Value>,
) -> Result<(), String> {
    for request in requests {
        let Some(decision) = decisions.get(&request.call_id) else {
            return Err(format!(
                "no decision for call `{}` ({})",
                request.call_id, request.tool
            ));
        };
        if !request.allowed.contains(&decision.kind()) {
            return Err(format!(
                "decision `{:?}` is not allowed for call `{}`; allowed: {:?}",
                decision.kind(),
                request.call_id,
                request.allowed
            ));
        }
        match decision {
            Decision::Edit { name, args } => {
                let tool = name.as_deref().unwrap_or(&request.tool);
                let schema = schemas.get(tool).ok_or_else(|| {
                    format!(
                        "edited call `{}` names unknown tool `{tool}`",
                        request.call_id
                    )
                })?;
                let problems = schema::validate(schema, &Value::Object(args.clone()));
                if !problems.is_empty() {
                    return Err(format!(
                        "edited arguments for call `{}` are invalid: {}",
                        request.call_id,
                        problems.join("; ")
                    ));
                }
            }
            Decision::Respond { message } if message.trim().is_empty() => {
                return Err(format!(
                    "the response for call `{}` is empty",
                    request.call_id
                ));
            }
            _ => {}
        }
    }
    if let Some(extra) = decisions
        .keys()
        .find(|id| !requests.iter().any(|r| &r.call_id == *id))
    {
        return Err(format!("call `{extra}` is not waiting for a decision"));
    }
    Ok(())
}

/// `*` matches any (possibly empty) text; everything else literally.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn glob_patterns() {
        assert!(glob("send_email", "send_email"));
        assert!(!glob("send_email", "send_emails"));
        assert!(glob("delete_*", "delete_user"));
        assert!(glob("delete_*", "delete_"));
        assert!(!glob("delete_*", "undelete_user"));
        assert!(glob("*_admin", "make_admin"));
        assert!(glob("a*b*c", "a-x-b-y-c"));
        assert!(!glob("a*b*c", "a-c-b"));
        assert!(glob("*", "anything"));
        assert!(!glob("ab*ba", "aba"), "parts must not overlap");
    }

    #[test]
    fn decisions_json_forms() {
        for (decision, value) in [
            (Decision::approve(), json!({"type": "approve"})),
            (
                Decision::reject("no"),
                json!({"type": "reject", "message": "no"}),
            ),
            (
                Decision::respond("42"),
                json!({"type": "respond", "message": "42"}),
            ),
            (
                Decision::edit(Map::from_iter([("to".into(), json!("a@b"))])),
                json!({"type": "edit", "args": {"to": "a@b"}}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&decision).unwrap(), value);
            assert_eq!(serde_json::from_value::<Decision>(value).unwrap(), decision);
        }
        let bare: Decision = serde_json::from_value(json!({"type": "reject"})).unwrap();
        assert_eq!(bare, Decision::Reject { message: None });
    }

    #[test]
    fn first_matching_rule_with_a_true_predicate_applies() {
        let approval = Approval::new()
            .rule(
                ApprovalRule::new("pay")
                    .when(|_, args| args["amount"].as_f64().unwrap_or(0.0) > 100.0)
                    .decisions([DecisionKind::Approve, DecisionKind::Reject])
                    .describe(|_, args| format!("Pay {}", args["amount"])),
            )
            .tool("delete_*");
        let small = Map::from_iter([("amount".into(), json!(5))]);
        let large = Map::from_iter([("amount".into(), json!(500))]);
        assert!(approval.request("c", "pay", &small).is_none());
        let request = approval.request("c", "pay", &large).unwrap();
        assert_eq!(request.description.as_deref(), Some("Pay 500"));
        assert_eq!(
            request.allowed,
            [DecisionKind::Approve, DecisionKind::Reject]
        );
        assert!(approval.request("c", "delete_row", &Map::new()).is_some());
        assert!(approval.check(&["pay", "delete_row"]).is_ok());
        assert!(approval.check(&["pay"]).unwrap_err().contains("delete_*"));
        let none = Approval::new().rule(ApprovalRule::new("pay").decisions([]));
        assert!(none.check(&["pay"]).is_err());
    }
}

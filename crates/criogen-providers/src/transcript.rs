//! Transcript validity (design 0002 §6).
//!
//! > Before every model call, each `tool_call` in the transcript has exactly
//! > one tool message with the same `call_id`, placed after its assistant
//! > message and before the next assistant message.
//!
//! Failed or cancelled runs, apps editing state and old snapshots can break
//! this. [`repair`] fixes a transcript; [`check`] only reports.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::message::{ContentBlock, Message, Role, ToolResultError, ToolStatus};

/// Text of the result inserted for a call that never completed.
pub const CANCELLED_TEXT: &str = "This call did not complete.";

/// One change [`repair`] made (also the payload of a `transcript_repaired`
/// event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TranscriptChange {
    /// A call had no result; one was inserted (`cancelled` for a tool call,
    /// `error` for an invalid tool call).
    InsertedResult {
        /// The call.
        call_id: String,
        /// The inserted message's id.
        message_id: String,
        /// Its status.
        status: ToolStatus,
    },
    /// A result answered no call of the preceding assistant message; removed.
    RemovedOrphan {
        /// The removed message.
        message_id: String,
        /// Its `call_id`.
        call_id: String,
    },
    /// A second result for the same call; removed (the first is kept).
    RemovedDuplicate {
        /// The removed message.
        message_id: String,
        /// Its `call_id`.
        call_id: String,
    },
}

/// A repaired transcript and what changed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Repaired {
    /// The valid transcript.
    pub messages: Vec<Message>,
    /// Changes in transcript order. Empty if it was already valid.
    pub changes: Vec<TranscriptChange>,
}

/// Lists what [`repair`] would change. Empty means valid.
pub fn check(messages: &[Message]) -> Vec<TranscriptChange> {
    repair(messages.to_vec()).changes
}

/// Makes a transcript valid (0002 §6):
///
/// | Violation | Repair |
/// |---|---|
/// | tool call without a result | insert a `cancelled` result |
/// | invalid tool call without a result | insert an `error` result |
/// | result without a matching call | remove it |
/// | two results for one call | keep the first |
///
/// Inserted results go right after the tool messages that directly follow
/// the assistant message, in call order. Their ids are derived from the call
/// id (`msg_repair_<call_id>`), so repairing twice gives the same messages.
pub fn repair(messages: Vec<Message>) -> Repaired {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut changes = Vec::new();
    let mut turn: Option<Turn> = None;

    for message in messages {
        match message.role {
            Role::Assistant => {
                if let Some(done) = turn.take() {
                    done.finish(&mut out, &mut changes);
                }
                let mut calls = Vec::new();
                let mut seen = HashSet::new();
                for block in &message.content {
                    let call = match block {
                        ContentBlock::ToolCall { id, name, .. } => Pending {
                            id: id.clone(),
                            tool: name.clone(),
                            parse_error: None,
                        },
                        ContentBlock::InvalidToolCall {
                            id, name, error, ..
                        } => Pending {
                            id: id.clone(),
                            tool: name.clone().unwrap_or_else(|| "unknown".into()),
                            parse_error: Some(error.clone()),
                        },
                        _ => continue,
                    };
                    if seen.insert(call.id.clone()) {
                        calls.push(call);
                    }
                }
                out.push(message);
                turn = Some(Turn {
                    calls,
                    answered: HashSet::new(),
                    block_end: out.len(),
                    block_open: true,
                });
            }
            Role::Tool => {
                let call_id = message.call_id.clone().unwrap_or_default();
                let Some(current) = turn.as_mut() else {
                    changes.push(TranscriptChange::RemovedOrphan {
                        message_id: message.id,
                        call_id,
                    });
                    continue;
                };
                if !current.calls.iter().any(|c| c.id == call_id) {
                    changes.push(TranscriptChange::RemovedOrphan {
                        message_id: message.id,
                        call_id,
                    });
                    continue;
                }
                if !current.answered.insert(call_id.clone()) {
                    changes.push(TranscriptChange::RemovedDuplicate {
                        message_id: message.id,
                        call_id,
                    });
                    continue;
                }
                out.push(message);
                if current.block_open {
                    current.block_end = out.len();
                }
            }
            Role::System | Role::User => {
                if let Some(current) = turn.as_mut() {
                    current.block_open = false;
                }
                out.push(message);
            }
        }
    }
    if let Some(done) = turn.take() {
        done.finish(&mut out, &mut changes);
    }
    Repaired {
        messages: out,
        changes,
    }
}

struct Pending {
    id: String,
    tool: String,
    /// Set for an invalid tool call.
    parse_error: Option<String>,
}

/// The calls of one assistant message and the results seen so far.
struct Turn {
    calls: Vec<Pending>,
    answered: HashSet<String>,
    /// Where missing results go: after the assistant message and the tool
    /// messages directly following it.
    block_end: usize,
    block_open: bool,
}

impl Turn {
    fn finish(self, out: &mut Vec<Message>, changes: &mut Vec<TranscriptChange>) {
        let mut at = self.block_end;
        for call in self.calls {
            if self.answered.contains(&call.id) {
                continue;
            }
            let mut result = match call.parse_error {
                None => Message::tool_result(
                    call.id.clone(),
                    call.tool,
                    ToolStatus::Cancelled,
                    vec![ContentBlock::text(CANCELLED_TEXT)],
                ),
                Some(error) => Message::tool_error(
                    call.id.clone(),
                    call.tool,
                    ToolResultError::new(
                        "invalid_arguments",
                        format!("Arguments could not be parsed: {error}"),
                    ),
                ),
            };
            result.id = format!("msg_repair_{}", call.id);
            changes.push(TranscriptChange::InsertedResult {
                call_id: call.id,
                message_id: result.id.clone(),
                status: result.status.expect("tool results have a status"),
            });
            out.insert(at, result);
            at += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;

    fn call(id: &str) -> ContentBlock {
        ContentBlock::tool_call(id, "search", Map::new())
    }

    fn assistant(id: &str, blocks: Vec<ContentBlock>) -> Message {
        Message::with_content(Role::Assistant, blocks).with_id(id)
    }

    fn result(id: &str, call_id: &str) -> Message {
        Message::tool_result(call_id, "search", ToolStatus::Success, vec![]).with_id(id)
    }

    fn ids(messages: &[Message]) -> Vec<&str> {
        messages.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn valid_transcript_is_unchanged() {
        let transcript = vec![
            Message::user("find x").with_id("u1"),
            assistant("a1", vec![call("c1"), call("c2")]),
            result("t2", "c2"),
            result("t1", "c1"),
            assistant("a2", vec![ContentBlock::text("done")]),
        ];
        assert!(check(&transcript).is_empty());
        let repaired = repair(transcript.clone());
        assert_eq!(repaired.messages, transcript);
    }

    #[test]
    fn missing_results_are_inserted_after_the_tool_block_in_call_order() {
        let transcript = vec![
            assistant("a1", vec![call("c1"), call("c2"), call("c3")]),
            result("t2", "c2"),
            Message::user("hurry up").with_id("u1"),
        ];
        let repaired = repair(transcript);
        assert_eq!(
            ids(&repaired.messages),
            ["a1", "t2", "msg_repair_c1", "msg_repair_c3", "u1"]
        );
        let inserted = &repaired.messages[2];
        assert_eq!(inserted.status, Some(ToolStatus::Cancelled));
        assert_eq!(inserted.text(), CANCELLED_TEXT);
        assert!(inserted.validate().is_ok());
        assert_eq!(repaired.changes.len(), 2);
    }

    #[test]
    fn invalid_tool_call_gets_an_error_result() {
        let transcript = vec![assistant(
            "a1",
            vec![ContentBlock::invalid_tool_call(
                "c1",
                Some("search".into()),
                "{\"q\": ",
                "truncated",
            )],
        )];
        let repaired = repair(transcript);
        let inserted = &repaired.messages[1];
        assert_eq!(inserted.status, Some(ToolStatus::Error));
        assert_eq!(inserted.error.as_ref().unwrap().code, "invalid_arguments");
        assert_eq!(
            inserted.text(),
            "Error: Arguments could not be parsed: truncated"
        );
    }

    #[test]
    fn orphans_and_duplicates_are_removed() {
        let transcript = vec![
            result("t0", "c0"), // before any assistant message
            assistant("a1", vec![call("c1")]),
            result("t1", "c1"),
            result("t1b", "c1"),
            result("tx", "c9"),
            assistant("a2", vec![]),
            result("late", "c1"), // answers a call of an earlier message
        ];
        let repaired = repair(transcript);
        assert_eq!(ids(&repaired.messages), ["a1", "t1", "a2"]);
        assert_eq!(
            repaired.changes,
            [
                TranscriptChange::RemovedOrphan {
                    message_id: "t0".into(),
                    call_id: "c0".into()
                },
                TranscriptChange::RemovedDuplicate {
                    message_id: "t1b".into(),
                    call_id: "c1".into()
                },
                TranscriptChange::RemovedOrphan {
                    message_id: "tx".into(),
                    call_id: "c9".into()
                },
                TranscriptChange::RemovedOrphan {
                    message_id: "late".into(),
                    call_id: "c1".into()
                },
            ]
        );
    }

    #[test]
    fn repair_is_idempotent() {
        let transcript = vec![
            assistant("a1", vec![call("c1"), call("c2")]),
            result("t1", "c1"),
            result("t1b", "c1"),
        ];
        let once = repair(transcript);
        let twice = repair(once.messages.clone());
        assert!(twice.changes.is_empty());
        assert_eq!(twice.messages, once.messages);
    }

    #[test]
    fn result_after_a_user_message_is_still_valid() {
        let transcript = vec![
            assistant("a1", vec![call("c1")]),
            Message::user("still there?").with_id("u1"),
            result("t1", "c1"),
        ];
        assert!(check(&transcript).is_empty());
    }

    #[test]
    fn changes_serialize_for_events() {
        let change = TranscriptChange::InsertedResult {
            call_id: "c1".into(),
            message_id: "msg_repair_c1".into(),
            status: ToolStatus::Cancelled,
        };
        assert_eq!(
            serde_json::to_value(&change).unwrap(),
            json!({"change": "inserted_result", "call_id": "c1", "message_id": "msg_repair_c1", "status": "cancelled"})
        );
    }
}

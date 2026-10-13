//! Reasoning inside `<think>…</think>` tags at the start of the answer
//! text, as reasoning models print it when the server has no reasoning
//! parser (e.g. DeepSeek-R1 on Ollama 0.6). It becomes a reasoning block, so
//! the answer holds only the answer.

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// What a piece of text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    Reasoning,
    Text,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No content seen yet; it may still start with `<think>`.
    #[default]
    Start,
    /// Inside the tags.
    Thinking,
    /// After the reasoning (or there was none): plain text.
    Answer,
}

/// Splits answer text that arrives in pieces. Pieces may cut a tag
/// anywhere; partial tags are held back until they are decided.
#[derive(Debug, Default)]
pub(crate) struct ThinkSplitter {
    phase: Phase,
    held: String,
    /// Drop whitespace at the start of the answer after `</think>`.
    trim_answer: bool,
}

impl ThinkSplitter {
    /// Splits one piece. Returns the parts it decided, in order.
    pub(crate) fn push(&mut self, piece: &str) -> Vec<(Part, String)> {
        let mut out = Vec::new();
        self.held.push_str(piece);
        loop {
            match self.phase {
                Phase::Start => {
                    let trimmed = self.held.trim_start();
                    if let Some(rest) = trimmed.strip_prefix(OPEN) {
                        self.held = rest.to_string();
                        self.phase = Phase::Thinking;
                    } else if OPEN.starts_with(trimmed) {
                        return out; // empty, whitespace or a partial `<think>`
                    } else {
                        self.phase = Phase::Answer;
                    }
                }
                Phase::Thinking => match self.held.find(CLOSE) {
                    Some(at) => {
                        let reasoning = self.held[..at].to_string();
                        self.held = self.held[at + CLOSE.len()..].to_string();
                        emit(&mut out, Part::Reasoning, &reasoning);
                        self.phase = Phase::Answer;
                        self.trim_answer = true;
                    }
                    None => {
                        // Keep a tail that could be the start of `</think>`.
                        let keep = partial_suffix(&self.held, CLOSE);
                        let ready: String = self.held.drain(..self.held.len() - keep).collect();
                        emit(&mut out, Part::Reasoning, &ready);
                        return out;
                    }
                },
                Phase::Answer => {
                    let mut text = std::mem::take(&mut self.held);
                    if self.trim_answer {
                        let trimmed = text.trim_start();
                        if trimmed.is_empty() {
                            return out; // still only whitespace
                        }
                        text = trimmed.to_string();
                        self.trim_answer = false;
                    }
                    emit(&mut out, Part::Text, &text);
                    return out;
                }
            }
        }
    }

    /// The end of the text: whatever is held is decided now. Unclosed
    /// reasoning stays reasoning; an unfinished `<think` is text.
    pub(crate) fn finish(&mut self) -> Vec<(Part, String)> {
        let mut out = Vec::new();
        let held = std::mem::take(&mut self.held);
        match self.phase {
            Phase::Thinking => emit(&mut out, Part::Reasoning, &held),
            Phase::Start | Phase::Answer if self.trim_answer => {
                emit(&mut out, Part::Text, held.trim_start())
            }
            Phase::Start | Phase::Answer => emit(&mut out, Part::Text, &held),
        }
        self.phase = Phase::Answer;
        out
    }
}

/// Splits complete answer text: the reasoning (if it starts with
/// `<think>`) and the answer.
pub(crate) fn split(text: &str) -> (Option<String>, String) {
    let mut splitter = ThinkSplitter::default();
    let mut parts = splitter.push(text);
    parts.extend(splitter.finish());
    let mut reasoning: Option<String> = None;
    let mut answer = String::new();
    for (part, piece) in parts {
        match part {
            Part::Reasoning => reasoning.get_or_insert_with(String::new).push_str(&piece),
            Part::Text => answer.push_str(&piece),
        }
    }
    (reasoning, answer)
}

fn emit(out: &mut Vec<(Part, String)>, part: Part, text: &str) {
    if text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some((last, piece)) if *last == part => piece.push_str(text),
        _ => out.push((part, text.to_string())),
    }
}

/// The length of the longest end of `text` that is a start of `tag` (both
/// ASCII at that point, so the cut is a char boundary).
fn partial_suffix(text: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&n| text.ends_with(&tag[..n]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `pieces` through a splitter and joins the parts.
    fn stream(pieces: &[&str]) -> (Option<String>, String) {
        let mut splitter = ThinkSplitter::default();
        let mut parts = Vec::new();
        for piece in pieces {
            parts.extend(splitter.push(piece));
        }
        parts.extend(splitter.finish());
        let mut reasoning: Option<String> = None;
        let mut answer = String::new();
        for (part, piece) in parts {
            match part {
                Part::Reasoning => reasoning.get_or_insert_with(String::new).push_str(&piece),
                Part::Text => answer.push_str(&piece),
            }
        }
        (reasoning, answer)
    }

    #[test]
    fn reasoning_at_the_start_is_split_off() {
        let (reasoning, answer) =
            split("<think>\nThe user asks 2+2. That is 4.\n</think>\n\nIt is **4**.");
        // The reasoning is kept exactly as written.
        assert_eq!(
            reasoning.as_deref(),
            Some("\nThe user asks 2+2. That is 4.\n")
        );
        assert_eq!(answer, "It is **4**.");
    }

    #[test]
    fn other_text_is_left_alone() {
        for text in [
            "Plain answer.",
            "A <think> tag in the middle stays </think> text.",
            "<thinking>not our tag</thinking>",
            "<b>bold</b>",
            "<",
            "",
            "  leading spaces are kept",
        ] {
            assert_eq!(split(text), (None, text.to_string()), "{text:?}");
        }
    }

    #[test]
    fn leading_whitespace_before_the_tag_is_allowed() {
        let (reasoning, answer) = split("\n  <think>hmm</think>answer");
        assert_eq!(reasoning.as_deref(), Some("hmm"));
        assert_eq!(answer, "answer");
    }

    #[test]
    fn unclosed_reasoning_stays_reasoning() {
        // Cut off by max_tokens while thinking.
        let (reasoning, answer) = split("<think>still thinking about");
        assert_eq!(reasoning.as_deref(), Some("still thinking about"));
        assert_eq!(answer, "");
        // Empty reasoning, only an answer.
        assert_eq!(split("<think></think>Hi"), (None, "Hi".to_string()));
    }

    #[test]
    fn any_way_of_cutting_the_stream_gives_the_same_result() {
        let samples = [
            "<think>\nWeigh the options: café ☕ or tea 🍵.\n</think>\n\nCoffee ☕, définitivement.",
            "  <think>a</think>b",
            "<think>x < y but </thin is not the end</think>done",
            "No reasoning here, just </think> text <think>.",
            "<thin",
            "<think>never closed </thi",
        ];
        for text in samples {
            let whole = split(text);
            let bounds: Vec<usize> = (0..=text.len())
                .filter(|&i| text.is_char_boundary(i))
                .collect();
            // Every single cut point.
            for &cut in &bounds {
                assert_eq!(
                    stream(&[&text[..cut], &text[cut..]]),
                    whole,
                    "{text:?} cut at {cut}"
                );
            }
            // Every char as its own piece.
            let chars: Vec<String> = text.chars().map(String::from).collect();
            let pieces: Vec<&str> = chars.iter().map(String::as_str).collect();
            assert_eq!(stream(&pieces), whole, "{text:?} char by char");
        }
    }
}

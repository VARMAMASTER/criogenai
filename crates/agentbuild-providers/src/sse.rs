//! Incremental Server-Sent Events parser (WHATWG HTML §9.2.6), for
//! streaming model responses (design 0006 §5).
//!
//! Bytes may arrive cut anywhere: inside a line, between `\r` and `\n`, or
//! inside a multi-byte UTF-8 character. The parser buffers until a line is
//! complete, so the result never depends on where the cuts are.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SseEvent {
    /// The `event:` field; `None` means the default type, `message`.
    pub event: Option<String>,
    /// The `data:` lines joined with `\n`.
    pub data: String,
    /// The last `id:` seen so far in the stream.
    pub id: Option<String>,
    /// A `retry:` value in milliseconds, if this event set one.
    pub retry: Option<u64>,
}

impl SseEvent {
    /// The event type (`message` if none was given).
    pub fn event_type(&self) -> &str {
        self.event.as_deref().unwrap_or("message")
    }
}

/// Feeds bytes in, gets complete events out.
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    /// The previous chunk ended with `\r`; a leading `\n` belongs to it.
    skip_lf: bool,
    /// The byte-order mark has been handled (only allowed at the start).
    started: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    last_id: Option<String>,
    retry: Option<u64>,
}

impl SseParser {
    /// A parser at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes `chunk` and returns the events it completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mut bytes = chunk;
        if !self.started && !bytes.is_empty() {
            // The BOM may itself be split across chunks.
            let bom = [0xEF, 0xBB, 0xBF];
            let have = self.line.len();
            let take = (bom.len() - have).min(bytes.len());
            if bom[have..have + take] == bytes[..take] {
                if have + take < bom.len() {
                    self.line.extend_from_slice(&bytes[..take]);
                    return events;
                }
                self.line.clear();
                bytes = &bytes[take..];
            }
            self.started = true;
        }
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' => self.end_line(&mut events),
                b'\r' => {
                    self.end_line(&mut events);
                    self.skip_lf = true;
                }
                _ => self.line.push(byte),
            }
        }
        events
    }

    /// Ends the stream. An event without its terminating blank line is
    /// discarded, as the specification requires.
    pub fn finish(self) {}

    fn end_line(&mut self, events: &mut Vec<SseEvent>) {
        // A partial BOM followed by a line end was ordinary content.
        self.started = true;
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            self.dispatch(events);
            return;
        }
        let line = String::from_utf8_lossy(&line);
        if line.starts_with(':') {
            return; // comment
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.last_id = Some(value.to_string()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry = value.parse().ok();
            }
            _ => {} // unknown fields are ignored
        }
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        let event = self.event.take();
        let retry = self.retry.take();
        if !std::mem::take(&mut self.has_data) {
            return;
        }
        events.push(SseEvent {
            event: event.filter(|e| !e.is_empty()),
            data: std::mem::take(&mut self.data),
            id: self.last_id.clone(),
            retry,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[u8]) -> Vec<SseEvent> {
        SseParser::new().feed(input)
    }

    fn data(events: &[SseEvent]) -> Vec<&str> {
        events.iter().map(|e| e.data.as_str()).collect()
    }

    #[test]
    fn basic_fields() {
        let events = parse(
            b"event: delta\nid: 7\ndata: {\"a\":1}\n\n: keep-alive\n\ndata: x\nretry: 3000\n\n",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), "delta");
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].event_type(), "message");
        assert_eq!(events[1].id.as_deref(), Some("7"), "id persists");
        assert_eq!(events[1].retry, Some(3000));
    }

    #[test]
    fn multi_line_data_and_space_rules() {
        let events = parse(b"data:a\ndata:  b\ndata\n\n");
        assert_eq!(events[0].data, "a\n b\n");
    }

    #[test]
    fn all_line_endings() {
        let events = parse(b"data: 1\r\rdata: 2\r\n\r\ndata: 3\n\n");
        assert_eq!(data(&events), ["1", "2", "3"]);
    }

    #[test]
    fn crlf_does_not_end_the_event_early() {
        let events = parse(b"event: a\r\ndata: x\r\ndata: y\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "a");
        assert_eq!(events[0].data, "x\ny");
    }

    #[test]
    fn events_without_data_are_not_dispatched() {
        let events = parse(b"event: ping\n\nid: 1\n\ndata: real\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "message", "event type was reset");
    }

    #[test]
    fn unterminated_tail_is_dropped() {
        let mut parser = SseParser::new();
        assert!(parser.feed(b"data: complete\n\ndata: partial").len() == 1);
        parser.finish();
    }

    #[test]
    fn bom_is_stripped_only_at_the_start() {
        let events = parse("\u{feff}data: a\n\n".as_bytes());
        assert_eq!(data(&events), ["a"]);
        let events = parse("data: \u{feff}a\n\n".as_bytes());
        assert_eq!(data(&events), ["\u{feff}a"]);
    }

    #[test]
    fn invalid_retry_and_id_with_nul_are_ignored() {
        let events = parse(b"retry: 12a\nid: a\0b\ndata: x\n\n");
        assert_eq!(events[0].retry, None);
        assert_eq!(events[0].id, None);
    }

    /// The same events, whatever the chunk boundaries (deterministic
    /// pseudo-random cuts; no proptest dependency).
    #[test]
    fn random_chunk_cuts_give_identical_events() {
        let input = "\u{feff}event: a\r\ndata: héllo 世界\r\n\r\n: c\ndata: 1\ndata: 2\r\rid: 9\ndata: ok\n\nretry: 5\ndata: 😀\n\ndata: tail".as_bytes();
        let expected = parse(input);
        assert_eq!(expected.len(), 4);
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..2000 {
            let mut parser = SseParser::new();
            let mut events = Vec::new();
            let mut rest = input;
            while !rest.is_empty() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let cut = 1 + (seed % 7) as usize;
                let (chunk, tail) = rest.split_at(cut.min(rest.len()));
                events.extend(parser.feed(chunk));
                rest = tail;
            }
            assert_eq!(events, expected);
        }
    }

    #[test]
    fn one_byte_at_a_time() {
        let input = b"data: [DONE]\r\n\r\n";
        let mut parser = SseParser::new();
        let events: Vec<SseEvent> = input.iter().flat_map(|b| parser.feed(&[*b])).collect();
        assert_eq!(data(&events), ["[DONE]"]);
    }
}

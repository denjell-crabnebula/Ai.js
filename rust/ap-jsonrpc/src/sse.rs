// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Incremental Server-Sent Events (SSE) parser and formatter.
//!
//! The parser follows the WHATWG EventSource processing model: input is split
//! into lines on `\r\n`, `\n` or `\r`; each line is a `field: value` pair; an
//! empty line dispatches the accumulated event; lines starting with `:` are
//! comments and are ignored. Only the `event`, `data`, `id` and `retry`
//! fields are recognised.
//!
//! Feed bytes as they arrive from the network with [`SseParser::feed`]. It
//! returns every event that became complete with that chunk.

use std::fmt;

/// One dispatched SSE event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, if any. Absent means the default `message` type.
    pub event: Option<String>,
    /// The joined `data:` lines (joined with `\n`, trailing newline removed).
    pub data: String,
    /// The last `id:` field seen on this event.
    pub id: Option<String>,
    /// The `retry:` field in milliseconds, if it parsed as an integer.
    pub retry: Option<u64>,
}

impl SseEvent {
    /// Build an event of the default type with the given data.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            ..Default::default()
        }
    }

    /// Build a typed event.
    pub fn typed(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
            ..Default::default()
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Serialize to wire format, including the terminating blank line.
    /// Multi-line data is emitted as multiple `data:` lines.
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        if let Some(ev) = &self.event {
            out.push_str("event: ");
            out.push_str(ev);
            out.push('\n');
        }
        if let Some(id) = &self.id {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }
        if let Some(retry) = self.retry {
            out.push_str("retry: ");
            out.push_str(&retry.to_string());
            out.push('\n');
        }
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

impl fmt::Display for SseEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

/// Format an SSE comment line (used for keepalives). Includes the newline.
pub fn comment(text: &str) -> String {
    format!(": {text}\n")
}

/// Streaming SSE parser. Retains partial lines between calls.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event_type: Option<String>,
    data: Vec<String>,
    id: Option<String>,
    retry: Option<u64>,
    saw_data_field: bool,
    bom_checked: bool,
    last_was_cr: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes. Returns the events completed by this chunk.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((line, consumed)) = self.next_line() {
            if let Some(ev) = self.process_line(&line) {
                events.push(ev);
            }
            self.buf.drain(..consumed);
        }
        events
    }

    /// Flush any pending partial line and event at end of stream.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            if let Some(ev) = self.process_line(&line) {
                return Some(ev);
            }
        }
        // Per spec a stream ending without a blank line does not dispatch.
        // Many servers omit the final blank line though, so we dispatch
        // whatever is pending as a convenience.
        self.dispatch()
    }

    /// Extract the next complete line (without its terminator) from the buffer.
    fn next_line(&mut self) -> Option<(String, usize)> {
        if !self.bom_checked {
            if self.buf.len() < 3 {
                if !self.buf.is_empty() && self.buf.len() < 3 && !self.buf.starts_with(&[0xEF]) {
                    self.bom_checked = true;
                } else {
                    return None;
                }
            } else {
                self.bom_checked = true;
                if self.buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
                    self.buf.drain(..3);
                }
            }
        }
        // A `\r` that ended the previous chunk may be followed by `\n` now.
        if self.last_was_cr {
            self.last_was_cr = false;
            if self.buf.first() == Some(&b'\n') {
                self.buf.remove(0);
            }
        }
        for (i, b) in self.buf.iter().enumerate() {
            match b {
                b'\n' => {
                    let line = String::from_utf8_lossy(&self.buf[..i]).into_owned();
                    return Some((line, i + 1));
                }
                b'\r' => {
                    let line = String::from_utf8_lossy(&self.buf[..i]).into_owned();
                    if i + 1 < self.buf.len() {
                        let consumed = if self.buf[i + 1] == b'\n' { i + 2 } else { i + 1 };
                        return Some((line, consumed));
                    }
                    self.last_was_cr = true;
                    return Some((line, i + 1));
                }
                _ => {}
            }
        }
        None
    }

    fn process_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(idx) => {
                let field = &line[..idx];
                let mut value = &line[idx + 1..];
                if let Some(stripped) = value.strip_prefix(' ') {
                    value = stripped;
                }
                (field, value)
            }
            None => (line, ""),
        };
        match field {
            "event" => self.event_type = Some(value.to_string()),
            "data" => {
                self.data.push(value.to_string());
                self.saw_data_field = true;
            }
            "id" => {
                if !value.contains('\0') {
                    self.id = Some(value.to_string());
                }
            }
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.retry = Some(ms);
                }
            }
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.saw_data_field {
            self.event_type = None;
            self.data.clear();
            return None;
        }
        let event = SseEvent {
            event: self.event_type.take(),
            data: self.data.join("\n"),
            id: self.id.clone(),
            retry: self.retry.take(),
        };
        self.data.clear();
        self.saw_data_field = false;
        Some(event)
    }
}

/// Parse a complete SSE document in one go.
pub fn parse_all(text: &str) -> Vec<SseEvent> {
    let mut p = SseParser::new();
    let mut events = p.feed(text.as_bytes());
    if let Some(last) = p.finish() {
        events.push(last);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn basic_event() -> TestResult {
        let events = parse_all("data: hello\n\n");
        assert_eq!(events, vec![SseEvent::data("hello")]);
        Ok(())
    }

    #[test]
    fn typed_event_with_id_and_multiline_data() -> TestResult {
        let events = parse_all("event: message\nid: 7\ndata: {\"a\":\ndata: 1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].data, "{\"a\":\n1}");
        Ok(())
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() -> TestResult {
        let events = parse_all(": keepalive\nfoo: bar\ndata: x\n\n");
        assert_eq!(events, vec![SseEvent::data("x")]);
        Ok(())
    }

    #[test]
    fn blank_line_without_data_does_not_dispatch() -> TestResult {
        let events = parse_all("event: ping\n\n: comment\n\n");
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn crlf_and_cr_line_endings() -> TestResult {
        let events = parse_all("data: a\r\n\r\ndata: b\r\rdata: c\n\n");
        let data: Vec<_> = events.iter().map(|e| e.data.as_str()).collect();
        assert_eq!(data, vec!["a", "b", "c"]);
        Ok(())
    }

    #[test]
    fn incremental_feeding_across_chunk_boundaries() -> TestResult {
        let mut p = SseParser::new();
        let mut all = Vec::new();
        let doc = b"data: first\n\nevent: e\ndata: sec\nond\n\n";
        // Feed one byte at a time to exercise every boundary.
        for b in doc.iter() {
            all.extend(p.feed(&[*b]));
        }
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].data, "first");
        assert_eq!(all[1].event.as_deref(), Some("e"));
        assert_eq!(all[1].data, "sec");
        Ok(())
    }

    #[test]
    fn cr_split_across_chunks() -> TestResult {
        let mut p = SseParser::new();
        let mut all = p.feed(b"data: a\r");
        all.extend(p.feed(b"\n\r\n"));
        assert_eq!(all, vec![SseEvent::data("a")]);
        Ok(())
    }

    #[test]
    fn retry_field_and_id_persist() -> TestResult {
        let events = parse_all("retry: 500\nid: 1\ndata: a\n\ndata: b\n\n");
        assert_eq!(events[0].retry, Some(500));
        assert_eq!(events[0].id.as_deref(), Some("1"));
        assert_eq!(events[1].retry, None);
        assert_eq!(events[1].id.as_deref(), Some("1"));
        Ok(())
    }

    #[test]
    fn strips_utf8_bom() -> TestResult {
        let events = parse_all("\u{feff}data: x\n\n");
        assert_eq!(events, vec![SseEvent::data("x")]);
        Ok(())
    }

    #[test]
    fn finish_flushes_unterminated_event() -> TestResult {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: tail").is_empty());
        assert_eq!(p.finish(), Some(SseEvent::data("tail")));
        Ok(())
    }

    #[test]
    fn wire_format_roundtrip() -> TestResult {
        let ev = SseEvent::typed("status", "line1\nline2").with_id("9");
        let wire = ev.to_wire();
        assert_eq!(wire, "event: status\nid: 9\ndata: line1\ndata: line2\n\n");
        assert_eq!(parse_all(&wire), vec![ev]);
        assert_eq!(comment("keepalive"), ": keepalive\n");
        Ok(())
    }
}

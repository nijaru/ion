//! Server-Sent Events (SSE) parser.
//!
//! Follows the SSE specification for parsing event streams from LLM APIs.

/// A parsed SSE event.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// Event type (from `event:` line), if present.
    pub event: Option<String>,
    /// Event data (from `data:` line).
    pub data: String,
}

/// Incremental SSE parser.
///
/// Buffers partial data and emits complete events.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    /// Create a new SSE parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of data and return any complete events.
    ///
    /// SSE events are delimited by double newlines (`\n\n`).
    /// Each event can have:
    /// - `event: <type>` - optional event type
    /// - `data: <content>` - event data (required)
    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();

        // Process complete events (delimited by \n\n)
        while let Some(pos) = self.buffer.find("\n\n") {
            let event_text = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + 2..].to_string();

            if let Some(event) = Self::parse_event(&event_text) {
                events.push(event);
            }
        }

        events
    }

    /// Parse a single SSE event from its text representation.
    fn parse_event(text: &str) -> Option<SseEvent> {
        let mut event_type = None;
        let mut data_parts = Vec::new();

        for line in text.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event_type = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                data_parts.push(value.trim_start().to_string());
            } else if line.starts_with(':') {
                // Comment line, ignore
            }
        }

        if data_parts.is_empty() {
            return None;
        }

        // Join multiple data lines with newlines
        let data = data_parts.join("\n");

        Some(SseEvent {
            event: event_type,
            data,
        })
    }

    /// Clear the internal buffer.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Check if there's pending data in the buffer.
    pub fn has_pending(&self) -> bool {
        !self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_event() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: hello world\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello world");
        assert!(events[0].event.is_none());
    }

    #[test]
    fn test_event_with_type() {
        let mut parser = SseParser::new();
        let events = parser.feed("event: message\ndata: {\"text\": \"hi\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data, "{\"text\": \"hi\"}");
    }

    #[test]
    fn test_multiple_events() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: first\n\ndata: second\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    #[test]
    fn test_partial_event() {
        let mut parser = SseParser::new();

        // First chunk: incomplete event
        let events = parser.feed("data: partial");
        assert!(events.is_empty());
        assert!(parser.has_pending());

        // Second chunk: completes the event
        let events = parser.feed(" message\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "partial message");
    }

    #[test]
    fn test_multiline_data() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: line1\ndata: line2\ndata: line3\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2\nline3");
    }

    #[test]
    fn test_comment_ignored() {
        let mut parser = SseParser::new();
        let events = parser.feed(": this is a comment\ndata: actual data\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "actual data");
    }

    #[test]
    fn test_empty_data_line() {
        let mut parser = SseParser::new();
        let events = parser.feed("data:\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn test_anthropic_style_event() {
        let mut parser = SseParser::new();
        let events = parser.feed(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n"
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("content_block_delta"));
        assert!(events[0].data.contains("Hello"));
    }

    #[test]
    fn test_openai_style_event() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n");
        assert_eq!(events.len(), 1);
        assert!(events[0].event.is_none());
        assert!(events[0].data.contains("Hi"));
    }

    #[test]
    fn test_done_marker() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: [DONE]\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "[DONE]");
    }
}

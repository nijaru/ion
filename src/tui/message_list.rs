use crate::agent::AgentEvent;
use crate::provider::{ContentBlock, Message, Role};
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    User,
    Agent,
    Tool,
    System,
}

/// Max lines to show in tool output (tail).
const TOOL_RESULT_MAX_LINES: usize = 5;
/// Max chars per line in tool output.
const TOOL_RESULT_LINE_MAX: usize = 120;

/// Extract the key argument from a tool call for display.
pub(crate) fn extract_key_arg(tool_name: &str, args: &serde_json::Value) -> String {
    let obj = match args.as_object() {
        Some(o) => o,
        None => return String::new(),
    };

    // Tool-specific key arguments
    let key = match tool_name {
        "read" | "write" | "edit" => "file_path",
        "bash" => "command",
        "glob" | "grep" => "pattern",
        _ => {
            // Fall back to first string argument
            return obj
                .values()
                .find_map(|v| v.as_str())
                .map(|s| truncate_for_display(s, 50))
                .unwrap_or_default();
        }
    };

    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| truncate_for_display(s, 50))
        .unwrap_or_default()
}

/// Truncate a string for display, showing the end for paths.
fn truncate_for_display(s: &str, max: usize) -> String {
    let s = s.lines().next().unwrap_or(s); // First line only
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else if max <= 3 {
        take_head(s, max)
    } else if s.contains('/') {
        // For paths, show the end
        format!("...{}", take_tail(s, max - 3))
    } else {
        // For other strings, show the beginning
        format!("{}...", take_head(s, max - 3))
    }
}

/// Format tool result showing tail of content with overflow indicator at top.
fn format_tool_result(result: &str) -> String {
    let result = result.trim();

    if result.is_empty() {
        return "OK".to_string();
    }

    let lines: Vec<&str> = result.lines().collect();
    let total = lines.len();

    if total <= TOOL_RESULT_MAX_LINES {
        // Show all lines, truncating long ones
        lines
            .iter()
            .map(|line| truncate_line(line, TOOL_RESULT_LINE_MAX))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        // Show overflow indicator at top, then last N lines (tail)
        let hidden = total - TOOL_RESULT_MAX_LINES;
        let mut output = vec![format!("… +{} lines", hidden)];
        output.extend(
            lines
                .iter()
                .skip(hidden)
                .map(|line| truncate_line(line, TOOL_RESULT_LINE_MAX)),
        );
        output.join("\n")
    }
}

/// Truncate a line to max chars.
fn truncate_line(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    format!("{}…", take_head(s, max - 1))
}

/// Strip redundant "Error:" prefixes from error messages.
#[must_use]
pub fn strip_error_prefixes(message: &str) -> &str {
    let mut out = message.trim_start();
    while let Some(stripped) = out.strip_prefix("Error:") {
        out = stripped.trim_start();
    }
    out
}

/// Sanitize tool name from model garbage (embedded args, XML artifacts).
#[must_use]
pub fn sanitize_tool_name(name: &str) -> &str {
    // Strip embedded arguments: "tool(args)" -> "tool"
    let name = name.split('(').next().unwrap_or(name);
    // Strip XML artifacts: "tool</tag>" -> "tool"
    let name = name.split('<').next().unwrap_or(name);
    name.trim()
}

fn take_head(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn take_tail(s: &str, max: usize) -> String {
    s.chars()
        .rev()
        .take(max)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessagePart {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Clone)]
pub struct MessageEntry {
    pub sender: Sender,
    pub parts: Vec<MessagePart>,
    markdown_cache: Option<String>,
}

impl MessageEntry {
    #[must_use]
    pub fn new(sender: Sender, content: String) -> Self {
        let mut entry = Self {
            sender,
            parts: vec![MessagePart::Text(content)],
            markdown_cache: None,
        };
        entry.update_cache();
        entry
    }

    #[must_use]
    pub fn new_thinking(sender: Sender, content: String) -> Self {
        let mut entry = Self {
            sender,
            parts: vec![MessagePart::Thinking(content)],
            markdown_cache: None,
        };
        entry.update_cache();
        entry
    }

    /// Appends a text delta and invalidates cache.
    pub fn append_text(&mut self, delta: &str) {
        if let Some(MessagePart::Text(text)) = self.parts.last_mut() {
            text.push_str(delta);
        } else {
            self.parts.push(MessagePart::Text(delta.to_string()));
        }
        self.update_cache();
    }

    /// Appends a thinking delta and invalidates cache.
    pub fn append_thinking(&mut self, delta: &str) {
        if let Some(MessagePart::Thinking(thinking)) = self.parts.last_mut() {
            thinking.push_str(delta);
        } else {
            self.parts.push(MessagePart::Thinking(delta.to_string()));
        }
        self.update_cache();
    }

    pub fn update_cache(&mut self) {
        let mut full = String::new();
        for part in &self.parts {
            match part {
                MessagePart::Text(t) => full.push_str(t),
                MessagePart::Thinking(t) => {
                    if !full.is_empty() && !full.ends_with('\n') {
                        full.push('\n');
                    }
                    full.push_str("\n> *Reasoning*\n");
                    for line in t.lines() {
                        full.push_str("> ");
                        full.push_str(line);
                        full.push('\n');
                    }
                    full.push('\n');
                }
            }
        }
        self.markdown_cache = Some(full);
    }

    #[must_use]
    pub fn content_as_markdown(&self) -> &str {
        self.markdown_cache.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Default)]
pub struct MessageList {
    pub entries: Vec<MessageEntry>,
    /// Lines scrolled up from the bottom (0 = at bottom, showing newest)
    pub scroll_offset: usize,
    /// Whether to auto-scroll when new content arrives
    pub auto_scroll: bool,
}

impl MessageList {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
        }
    }

    /// Scroll up by n lines (towards older messages)
    pub fn scroll_up(&mut self, n: usize) {
        // Cap at a reasonable maximum to prevent overflow
        // Actual content length is handled during render
        self.scroll_offset = (self.scroll_offset + n).min(10000);
        self.auto_scroll = false;
    }

    /// Scroll down by n lines (towards newer messages)
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Jump to top (oldest messages)
    pub fn scroll_to_top(&mut self) {
        // Set to max - render will clamp to actual content
        self.scroll_offset = 10000;
        self.auto_scroll = false;
    }

    /// Jump to bottom (newest messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Returns true if currently at bottom
    #[must_use]
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    pub fn push_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(delta) => {
                if let Some(last) = self.entries.last_mut()
                    && last.sender == Sender::Agent
                {
                    last.append_text(&delta);
                    return;
                }
                // Skip empty/whitespace deltas when creating new entries
                if !delta.trim().is_empty() {
                    self.push_entry(MessageEntry::new(Sender::Agent, delta));
                }
            }
            AgentEvent::ToolCallStart(_id, name, args) => {
                // Sanitize tool name (strip model garbage like embedded args, XML)
                let clean_name = sanitize_tool_name(&name);
                // Format: tool_name(key_arg)
                let key_arg = extract_key_arg(clean_name, &args);
                let display = if key_arg.is_empty() {
                    clean_name.to_string()
                } else {
                    format!("{clean_name}({key_arg})")
                };
                self.push_entry(MessageEntry::new(Sender::Tool, display));
            }
            AgentEvent::ToolCallResult(_id, result, is_error) => {
                // Check if this is a context-gathering tool (collapse output)
                let is_collapsed_tool = self.entries.last().is_some_and(|e| {
                    e.sender == Sender::Tool
                        && (e.content_as_markdown().starts_with("read(")
                            || e.content_as_markdown().starts_with("glob(")
                            || e.content_as_markdown().starts_with("grep(")
                            || e.content_as_markdown().starts_with("list("))
                });

                let result_content = if is_error {
                    // Clean up error message - keep it concise
                    let msg = strip_error_prefixes(&result).trim();
                    format!("⎿ Error: {}", truncate_line(msg, TOOL_RESULT_LINE_MAX))
                } else if is_collapsed_tool {
                    // Collapsed tools: just show line count or OK
                    let line_count = result.lines().count();
                    if line_count > 1 {
                        format!("⎿ {line_count} lines")
                    } else if result.trim().is_empty() {
                        "⎿ OK".to_string()
                    } else {
                        format!("⎿ {}", truncate_line(result.trim(), 60))
                    }
                } else {
                    // Full output: format with tail display
                    let formatted = format_tool_result(&result);
                    // Prefix first line with ⎿, indent rest
                    let mut lines = formatted.lines();
                    let mut output = String::new();
                    if let Some(first) = lines.next() {
                        let _ = write!(output, "⎿ {first}");
                    }
                    for line in lines {
                        let _ = write!(output, "\n  {line}");
                    }
                    output
                };

                // Append result to last tool entry if it exists
                if let Some(last) = self.entries.last_mut()
                    && last.sender == Sender::Tool
                {
                    last.append_text(&format!("\n{result_content}"));
                } else {
                    self.push_entry(MessageEntry::new(Sender::Tool, result_content));
                }
            }
            AgentEvent::PlanGenerated(plan) => {
                let mut content = String::from("### 📋 Proposed Plan\n\n");
                for task in &plan.tasks {
                    let _ = writeln!(content, "- **{}**: {}", task.title, task.description);
                    if !task.dependencies.is_empty() {
                        let _ = writeln!(
                            content,
                            "  *(Depends on: {})*",
                            task.dependencies.join(", ")
                        );
                    }
                }
                self.push_entry(MessageEntry::new(Sender::System, content));
            }
            AgentEvent::Finished(msg) => {
                self.push_entry(MessageEntry::new(Sender::System, msg));
            }
            AgentEvent::Error(e) => {
                self.push_entry(MessageEntry::new(Sender::System, format!("Error: {e}")));
            }
            // ThinkingDelta: tracked in session.rs for progress display, not rendered
            // CompactionStatus: handled by TUI main loop for status bar
            // Other events: ignored
            _ => {}
        }
    }

    /// Push an entry, maintaining scroll position if user scrolled up.
    pub fn push_entry(&mut self, entry: MessageEntry) {
        // If user has scrolled up, keep their position stable by adding
        // estimated line count for this entry (header + content + blank)
        if !self.auto_scroll {
            let content_lines = entry.content_as_markdown().lines().count();
            // +2 for header line and trailing blank line
            self.scroll_offset += content_lines + 2;
        }
        self.entries.push(entry);
    }

    pub fn push_user_message(&mut self, content: String) {
        // User message always scrolls to bottom
        self.scroll_to_bottom();
        self.entries.push(MessageEntry::new(Sender::User, content));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Load entries from persisted session messages (for resume).
    pub fn load_from_messages(&mut self, messages: &[Message]) {
        self.entries.clear();

        for msg in messages {
            match msg.role {
                Role::User => {
                    // Extract text content
                    for block in msg.content.iter() {
                        if let ContentBlock::Text { text } = block {
                            self.entries
                                .push(MessageEntry::new(Sender::User, text.clone()));
                        }
                    }
                }
                Role::Assistant => {
                    // Collect text into entry (skip thinking blocks in history)
                    let mut parts = Vec::new();
                    for block in msg.content.iter() {
                        match block {
                            ContentBlock::Text { text } => {
                                parts.push(MessagePart::Text(text.clone()));
                            }
                            ContentBlock::ToolCall {
                                id: _,
                                name,
                                arguments,
                            } => {
                                // Format: tool_name(key_arg)
                                let key_arg = extract_key_arg(name, arguments);
                                let display = if key_arg.is_empty() {
                                    name.clone()
                                } else {
                                    format!("{name}({key_arg})")
                                };
                                self.entries.push(MessageEntry::new(Sender::Tool, display));
                            }
                            // Thinking blocks not displayed in history
                            _ => {}
                        }
                    }
                    if !parts.is_empty() {
                        let mut entry = MessageEntry {
                            sender: Sender::Agent,
                            parts,
                            markdown_cache: None,
                        };
                        entry.update_cache();
                        self.entries.push(entry);
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            tool_call_id: _,
                            content,
                            is_error,
                        } = block
                        {
                            let display = if *is_error {
                                let msg = strip_error_prefixes(content).trim();
                                format!("⎿ Error: {}", truncate_line(msg, TOOL_RESULT_LINE_MAX))
                            } else {
                                // Format result with actual content
                                let formatted = format_tool_result(content);
                                let mut lines = formatted.lines();
                                let mut output = String::new();
                                if let Some(first) = lines.next() {
                                    let _ = write!(output, "⎿ {first}");
                                }
                                for line in lines {
                                    let _ = write!(output, "\n  {line}");
                                }
                                output
                            };
                            self.entries.push(MessageEntry::new(Sender::Tool, display));
                        }
                    }
                }
                Role::System => {} // Don't display system messages in history
            }
        }
    }
}

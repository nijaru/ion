use crate::agent::AgentEvent;
use crate::provider::{ContentBlock, Message, Role, format_api_error};
use std::collections::HashMap;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    User,
    Agent,
    Tool,
    System,
}

/// Max lines to show in tool output (tail).
const TOOL_RESULT_MAX_LINES: usize = 10;
/// Max chars per line in tool output.
const TOOL_RESULT_LINE_MAX: usize = 120;

/// Extract the key argument from a tool call for display.
/// Returns the main argument plus any secondary params in Claude Code style.
pub(crate) fn extract_key_arg(tool_name: &str, args: &serde_json::Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };

    match tool_name {
        "read" | "write" | "edit" => obj
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| truncate_for_display(&relative_display_path(s), 60))
            .unwrap_or_default(),
        "bash" => {
            let cmd = obj
                .get("command")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_display(s, 60))
                .unwrap_or_default();
            // Only show dir if it differs from cwd
            if let Some(dir) = obj.get("directory").and_then(|v| v.as_str()) {
                let is_cwd = std::env::current_dir()
                    .map(|cwd| cwd.to_string_lossy() == dir)
                    .unwrap_or(false);
                if !is_cwd {
                    let rel = relative_display_path(dir);
                    return format!("{cmd}, dir={}", truncate_for_display(&rel, 40));
                }
            }
            cmd
        }
        "glob" => {
            let pattern = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_display(s, 50))
                .unwrap_or_default();

            if let Some(path) = obj.get("path").and_then(|v| v.as_str())
                && path != "."
                && !path.is_empty()
            {
                let rel = relative_display_path(path);
                return format!("{pattern} in {}", truncate_for_display(&rel, 40));
            }
            pattern
        }
        "grep" => {
            let pattern = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_display(s, 50))
                .unwrap_or_default();

            let mut extras = Vec::new();
            if let Some(path) = obj.get("path").and_then(|v| v.as_str())
                && path != "."
                && !path.is_empty()
            {
                let rel = relative_display_path(path);
                extras.push(format!("in {}", truncate_for_display(&rel, 40)));
            }
            if let Some(typ) = obj.get("type").and_then(|v| v.as_str()) {
                extras.push(format!("type={typ}"));
            }
            if let Some(mode) = obj.get("output_mode").and_then(|v| v.as_str())
                && mode != "content"
            {
                extras.push(format!("mode={mode}"));
            }

            if extras.is_empty() {
                pattern
            } else {
                format!("{pattern} {}", extras.join(" "))
            }
        }
        _ => obj
            .values()
            .find_map(|v| v.as_str())
            .map(|s| truncate_for_display(s, 60))
            .unwrap_or_default(),
    }
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
    // Single pass: reverse, take, reverse back
    s.chars()
        .rev()
        .take(max)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Convert an absolute path to relative (strips cwd prefix).
fn relative_display_path(path: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy();
        if let Some(rel) = path.strip_prefix(cwd_str.as_ref()) {
            let rel = rel.strip_prefix('/').unwrap_or(rel);
            if !rel.is_empty() {
                return rel.to_string();
            }
        }
    }
    path.to_string()
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

/// Tracks a pending tool call awaiting its result.
#[derive(Debug)]
struct PendingCall {
    entry_idx: usize,
    key_arg: String,
    grouped: bool,
}

/// Tracks the current sequence of same-name tool calls being grouped.
#[derive(Debug)]
struct ActiveToolGroup {
    entry_idx: usize,
    tool_name: String,
    count: usize,
}

/// Unit name for grouped tool call headers.
fn group_unit(tool_name: &str) -> &str {
    match tool_name {
        "read" | "write" | "edit" => "files",
        "bash" => "commands",
        "glob" => "patterns",
        "grep" => "queries",
        _ => "calls",
    }
}

/// Format a grouped tool call result line: `⎿ key_arg ✓ summary`.
fn format_grouped_result(key_arg: &str, result: &str, is_error: bool) -> String {
    if is_error {
        let msg = strip_error_prefixes(result).trim();
        format!("⎿ {key_arg} ✗ {}", truncate_line(msg, 80))
    } else if result.trim().is_empty() {
        format!("⎿ {key_arg} ✓")
    } else {
        let line_count = result.lines().count();
        if line_count > 1 {
            format!("⎿ {key_arg} ✓ {line_count} lines")
        } else {
            format!("⎿ {key_arg} ✓ {}", truncate_line(result.trim(), 60))
        }
    }
}

/// Extract tool name from a tool entry.
fn tool_name_from_entry(entry: &MessageEntry) -> Option<String> {
    if entry.sender == Sender::Tool {
        let md = entry.content_as_markdown();
        let name = md.split('(').next().unwrap_or("");
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

/// Classify tool result display style by tool name.
enum ResultStyle {
    /// Collapsed: show line/item count only (read, glob, grep, list)
    Collapsed(&'static str),
    /// Diff summary: "Added X lines, removed Y lines" + diff (edit, write)
    DiffSummary,
    /// Full tail output (bash, etc.)
    Full,
}

fn result_style(tool_name: Option<&str>) -> ResultStyle {
    match tool_name {
        Some("read") => ResultStyle::Collapsed("lines"),
        Some("list" | "glob") => ResultStyle::Collapsed("items"),
        Some("grep") => ResultStyle::Collapsed("matches"),
        Some("edit" | "write") => ResultStyle::DiffSummary,
        _ => ResultStyle::Full,
    }
}

/// Format a single (non-grouped) tool result, using the entry at the given index.
fn format_single_result(
    entry_idx: usize,
    entries: &[MessageEntry],
    result: &str,
    is_error: bool,
) -> String {
    let tool_name = entries.get(entry_idx).and_then(tool_name_from_entry);
    format_result_content(tool_name.as_deref(), result, is_error)
}

/// Format a single tool result, using the last entry for tool name detection.
fn format_single_result_last(entries: &[MessageEntry], result: &str, is_error: bool) -> String {
    let tool_name = entries.last().and_then(tool_name_from_entry);
    format_result_content(tool_name.as_deref(), result, is_error)
}

/// Count added/removed lines in a unified diff.
fn count_diff_lines(result: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in result.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    (added, removed)
}

/// Format a diff summary string like "Added 4 lines, removed 1 line".
fn format_diff_summary(added: usize, removed: usize) -> String {
    match (added, removed) {
        (0, 0) => " ✓".to_string(),
        (a, 0) => format!(" ⎿ Added {a} line{}", if a == 1 { "" } else { "s" }),
        (0, r) => format!(" ⎿ Removed {r} line{}", if r == 1 { "" } else { "s" }),
        (a, r) => format!(
            " ⎿ Added {a} line{}, removed {r} line{}",
            if a == 1 { "" } else { "s" },
            if r == 1 { "" } else { "s" },
        ),
    }
}

/// Common formatting for single-call tool results.
fn format_result_content(tool_name: Option<&str>, result: &str, is_error: bool) -> String {
    if is_error {
        let msg = strip_error_prefixes(result).trim();
        return format!(" ✗ {}", truncate_line(msg, TOOL_RESULT_LINE_MAX));
    }

    match result_style(tool_name) {
        ResultStyle::Collapsed(unit) => {
            let line_count = result.lines().count();
            if line_count > 1 {
                format!(" ✓ {line_count} {unit}")
            } else if result.trim().is_empty() {
                " ✓".to_string()
            } else {
                format!(" ✓ {}", truncate_line(result.trim(), 60))
            }
        }
        ResultStyle::DiffSummary => {
            let (added, removed) = count_diff_lines(result);
            let summary = format_diff_summary(added, removed);
            // Append the diff content below the summary for rendering
            let formatted = format_tool_result(result);
            let mut output = summary;
            for line in formatted.lines() {
                let _ = write!(output, "\n   {line}");
            }
            output
        }
        ResultStyle::Full => {
            let formatted = format_tool_result(result);
            let mut lines = formatted.lines();
            let mut output = String::new();
            if let Some(first) = lines.next() {
                let _ = write!(output, " ✓ {first}");
            }
            for line in lines {
                let _ = write!(output, "\n   {line}");
            }
            output
        }
    }
}

#[derive(Debug, Default)]
pub struct MessageList {
    pub entries: Vec<MessageEntry>,
    /// Lines scrolled up from the bottom (0 = at bottom, showing newest)
    pub scroll_offset: usize,
    /// Whether to auto-scroll when new content arrives
    pub auto_scroll: bool,
    /// Pending tool calls awaiting results, keyed by call ID.
    pending_tool_calls: HashMap<String, PendingCall>,
    /// Active grouping state for consecutive same-name tool calls.
    active_group: Option<ActiveToolGroup>,
}

impl MessageList {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            pending_tool_calls: HashMap::new(),
            active_group: None,
        }
    }

    /// Scroll up by n lines (towards older messages)
    pub fn scroll_up(&mut self, n: usize) {
        // Saturating add - render will clamp to actual content length
        self.scroll_offset = self.scroll_offset.saturating_add(n);
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
        self.scroll_offset = usize::MAX;
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
                self.active_group = None;
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
            AgentEvent::ToolCallStart(id, name, args) => {
                let clean_name = sanitize_tool_name(&name);
                let key_arg = extract_key_arg(clean_name, &args);
                let key_arg_display = if key_arg.is_empty() {
                    clean_name.to_string()
                } else {
                    key_arg
                };

                // Check if this should be grouped with previous same-name tool call
                if let Some(ref mut group) = self.active_group
                    && group.tool_name == clean_name
                {
                    group.count += 1;

                    // Mark first call as grouped when transitioning from single
                    if group.count == 2 {
                        for call in self.pending_tool_calls.values_mut() {
                            if call.entry_idx == group.entry_idx {
                                call.grouped = true;
                            }
                        }
                    }

                    // Update group header
                    let unit = group_unit(clean_name);
                    let header = format!("{clean_name}({} {unit})", group.count);
                    if let Some(entry) = self.entries.get_mut(group.entry_idx) {
                        entry.parts = vec![MessagePart::Text(header)];
                        entry.update_cache();
                    }

                    self.pending_tool_calls.insert(
                        id,
                        PendingCall {
                            entry_idx: group.entry_idx,
                            key_arg: key_arg_display,
                            grouped: true,
                        },
                    );
                    return;
                }

                // New tool call (not grouped with previous)
                let display = format!("{clean_name}({key_arg_display})");
                let entry_idx = self.entries.len();
                self.push_entry(MessageEntry::new(Sender::Tool, display));

                self.active_group = Some(ActiveToolGroup {
                    entry_idx,
                    tool_name: clean_name.to_string(),
                    count: 1,
                });

                self.pending_tool_calls.insert(
                    id,
                    PendingCall {
                        entry_idx,
                        key_arg: key_arg_display,
                        grouped: false,
                    },
                );
            }
            AgentEvent::ToolCallResult(id, result, is_error) => {
                self.active_group = None;

                if let Some(call) = self.pending_tool_calls.remove(&id) {
                    if call.grouped {
                        // Grouped result: brief per-item line
                        let result_line = format_grouped_result(&call.key_arg, &result, is_error);
                        if let Some(entry) = self.entries.get_mut(call.entry_idx) {
                            entry.append_text(&format!("\n{result_line}"));
                        }
                    } else {
                        // Single call: format based on tool type
                        let result_content =
                            format_single_result(call.entry_idx, &self.entries, &result, is_error);
                        if let Some(entry) = self.entries.get_mut(call.entry_idx) {
                            entry.append_text(&format!("\n{result_content}"));
                        }
                    }
                } else {
                    // Fallback: no pending call tracked, append to last Tool entry
                    let result_content =
                        format_single_result_last(&self.entries, &result, is_error);
                    if let Some(last) = self.entries.last_mut()
                        && last.sender == Sender::Tool
                    {
                        last.append_text(&format!("\n{result_content}"));
                    } else {
                        self.push_entry(MessageEntry::new(Sender::Tool, result_content));
                    }
                }
            }
            AgentEvent::Warning(msg) => {
                self.active_group = None;
                self.push_entry(MessageEntry::new(Sender::System, format!("Warning: {msg}")));
            }
            AgentEvent::Finished(msg) => {
                self.active_group = None;
                self.push_entry(MessageEntry::new(Sender::System, msg));
            }
            AgentEvent::Error(e) => {
                self.active_group = None;
                let formatted = format_api_error(&e);
                self.push_entry(MessageEntry::new(
                    Sender::System,
                    format!("Error: {formatted}"),
                ));
            }
            // ThinkingDelta, ProviderUsage: tracked in update.rs
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
        self.pending_tool_calls.clear();
        self.active_group = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- extract_key_arg tests ---

    #[test]
    fn test_extract_key_arg_read() {
        // Short path - no truncation
        let args = json!({"file_path": "/home/user/test.rs"});
        assert_eq!(extract_key_arg("read", &args), "/home/user/test.rs");
    }

    #[test]
    fn test_extract_key_arg_read_long_path() {
        // Long path (>60 chars) - truncated from end (paths show suffix)
        let args = json!({"file_path": "/home/user/projects/really/very/long/nested/path/to/some/deeply/buried/file.rs"});
        let result = extract_key_arg("read", &args);
        assert!(
            result.starts_with("..."),
            "Long paths should start with ..., got: {result}"
        );
        assert!(
            result.ends_with("file.rs"),
            "Should preserve filename, got: {result}"
        );
        assert!(
            result.chars().count() <= 60,
            "Should be truncated to 60 chars, got: {}",
            result.chars().count()
        );
    }

    #[test]
    fn test_extract_key_arg_bash() {
        let args = json!({"command": "cargo test"});
        assert_eq!(extract_key_arg("bash", &args), "cargo test");
    }

    #[test]
    fn test_extract_key_arg_glob() {
        let args = json!({"pattern": "**/*.rs"});
        assert_eq!(extract_key_arg("glob", &args), "**/*.rs");
    }

    #[test]
    fn test_extract_key_arg_unknown_tool() {
        let args = json!({"query": "search term"});
        assert_eq!(extract_key_arg("custom_tool", &args), "search term");
    }

    #[test]
    fn test_extract_key_arg_empty() {
        let args = json!({});
        assert_eq!(extract_key_arg("read", &args), "");
    }

    #[test]
    fn test_extract_key_arg_non_object() {
        let args = json!("string");
        assert_eq!(extract_key_arg("read", &args), "");
    }

    // --- truncate_for_display tests ---

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate_for_display("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_path_shows_end() {
        let path = "/home/user/projects/myapp/src/main.rs";
        let truncated = truncate_for_display(path, 20);
        assert!(truncated.starts_with("..."));
        assert!(truncated.ends_with("main.rs"));
    }

    #[test]
    fn test_truncate_non_path_shows_beginning() {
        let text = "This is a very long string that needs truncation";
        let truncated = truncate_for_display(text, 20);
        assert!(truncated.starts_with("This"));
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_truncate_multiline_uses_first() {
        let text = "first line\nsecond line";
        assert_eq!(truncate_for_display(text, 50), "first line");
    }

    // --- format_tool_result tests ---

    #[test]
    fn test_format_tool_result_empty() {
        assert_eq!(format_tool_result(""), "OK");
        assert_eq!(format_tool_result("   "), "OK");
    }

    #[test]
    fn test_format_tool_result_short() {
        assert_eq!(format_tool_result("success"), "success");
    }

    #[test]
    fn test_format_tool_result_few_lines() {
        let input = "line1\nline2\nline3";
        let output = format_tool_result(input);
        assert_eq!(output, "line1\nline2\nline3");
    }

    #[test]
    fn test_format_tool_result_many_lines_shows_tail() {
        let lines: Vec<String> = (0..20).map(|i| format!("line{i}")).collect();
        let input = lines.join("\n");
        let output = format_tool_result(&input);
        assert!(output.starts_with("… +10 lines"));
        assert!(output.contains("line19"));
        assert!(!output.contains("line0\n"));
    }

    // --- strip_error_prefixes tests ---

    #[test]
    fn test_strip_error_prefixes_single() {
        assert_eq!(
            strip_error_prefixes("Error: something went wrong"),
            "something went wrong"
        );
    }

    #[test]
    fn test_strip_error_prefixes_multiple() {
        assert_eq!(
            strip_error_prefixes("Error: Error: nested error"),
            "nested error"
        );
    }

    #[test]
    fn test_strip_error_prefixes_none() {
        assert_eq!(
            strip_error_prefixes("no error prefix here"),
            "no error prefix here"
        );
    }

    #[test]
    fn test_strip_error_prefixes_whitespace() {
        assert_eq!(strip_error_prefixes("  Error:  message"), "message");
    }

    // --- sanitize_tool_name tests ---

    #[test]
    fn test_sanitize_tool_name_clean() {
        assert_eq!(sanitize_tool_name("read"), "read");
    }

    #[test]
    fn test_sanitize_tool_name_with_args() {
        assert_eq!(sanitize_tool_name("read(file.txt)"), "read");
    }

    #[test]
    fn test_sanitize_tool_name_with_xml() {
        assert_eq!(sanitize_tool_name("bash</tool>"), "bash");
    }

    #[test]
    fn test_sanitize_tool_name_with_both() {
        assert_eq!(sanitize_tool_name("edit(file)</tag>"), "edit");
    }

    #[test]
    fn test_sanitize_tool_name_whitespace() {
        assert_eq!(sanitize_tool_name("  read  "), "read");
    }

    // --- truncate_line tests ---

    #[test]
    fn test_truncate_line_short() {
        assert_eq!(truncate_line("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_line_exact() {
        assert_eq!(truncate_line("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_line_long() {
        assert_eq!(truncate_line("hello world", 5), "hell…");
    }

    #[test]
    fn test_truncate_line_zero() {
        assert_eq!(truncate_line("hello", 0), "");
    }

    #[test]
    fn test_truncate_line_one() {
        assert_eq!(truncate_line("hello", 1), "…");
    }

    // --- MessageList scroll tests ---

    #[test]
    fn test_message_list_new() {
        let list = MessageList::new();
        assert!(list.entries.is_empty());
        assert_eq!(list.scroll_offset, 0);
        assert!(list.auto_scroll);
    }

    #[test]
    fn test_message_list_scroll_up() {
        let mut list = MessageList::new();
        list.scroll_up(5);
        assert_eq!(list.scroll_offset, 5);
        assert!(!list.auto_scroll);
    }

    #[test]
    fn test_message_list_scroll_down() {
        let mut list = MessageList::new();
        list.scroll_offset = 10;
        list.auto_scroll = false;
        list.scroll_down(3);
        assert_eq!(list.scroll_offset, 7);
        assert!(!list.auto_scroll);
    }

    #[test]
    fn test_message_list_scroll_down_to_bottom() {
        let mut list = MessageList::new();
        list.scroll_offset = 5;
        list.auto_scroll = false;
        list.scroll_down(10);
        assert_eq!(list.scroll_offset, 0);
        assert!(list.auto_scroll);
    }

    #[test]
    fn test_message_list_scroll_to_top() {
        let mut list = MessageList::new();
        list.scroll_to_top();
        assert_eq!(list.scroll_offset, usize::MAX);
        assert!(!list.auto_scroll);
    }

    #[test]
    fn test_message_list_scroll_to_bottom() {
        let mut list = MessageList::new();
        list.scroll_offset = 100;
        list.auto_scroll = false;
        list.scroll_to_bottom();
        assert_eq!(list.scroll_offset, 0);
        assert!(list.auto_scroll);
    }

    #[test]
    fn test_message_list_is_at_bottom() {
        let mut list = MessageList::new();
        assert!(list.is_at_bottom());
        list.scroll_up(5);
        assert!(!list.is_at_bottom());
    }

    // --- MessageEntry tests ---

    #[test]
    fn test_message_entry_new() {
        let entry = MessageEntry::new(Sender::User, "hello".to_string());
        assert_eq!(entry.sender, Sender::User);
        assert_eq!(entry.content_as_markdown(), "hello");
    }

    #[test]
    fn test_message_entry_append_text() {
        let mut entry = MessageEntry::new(Sender::Agent, "hello".to_string());
        entry.append_text(" world");
        assert_eq!(entry.content_as_markdown(), "hello world");
    }

    #[test]
    fn test_message_entry_thinking() {
        let entry = MessageEntry::new_thinking(Sender::Agent, "reasoning here".to_string());
        let md = entry.content_as_markdown();
        assert!(md.contains("*Reasoning*"));
        assert!(md.contains("> reasoning here"));
    }

    #[test]
    fn test_message_entry_append_thinking() {
        let mut entry = MessageEntry::new(Sender::Agent, "response".to_string());
        entry.append_thinking("thought");
        let md = entry.content_as_markdown();
        assert!(md.contains("response"));
        assert!(md.contains("*Reasoning*"));
    }

    // --- take_head / take_tail tests ---

    #[test]
    fn test_take_head() {
        assert_eq!(take_head("hello world", 5), "hello");
        assert_eq!(take_head("hi", 10), "hi");
        assert_eq!(take_head("test", 0), "");
    }

    #[test]
    fn test_take_tail() {
        assert_eq!(take_tail("hello world", 5), "world");
        assert_eq!(take_tail("hi", 10), "hi");
        assert_eq!(take_tail("test", 2), "st");
    }

    #[test]
    fn test_take_tail_unicode() {
        // Ensure we handle unicode correctly (char-based, not byte-based)
        assert_eq!(take_tail("héllo", 3), "llo");
    }

    // --- Parallel tool call grouping tests ---

    #[test]
    fn test_single_tool_call_no_grouping() {
        let mut list = MessageList::new();
        list.push_event(AgentEvent::ToolCallStart(
            "id1".into(),
            "read".into(),
            json!({"file_path": "a.rs"}),
        ));
        list.push_event(AgentEvent::ToolCallResult(
            "id1".into(),
            "content".into(),
            false,
        ));

        assert_eq!(list.entries.len(), 1);
        let md = list.entries[0].content_as_markdown();
        assert!(md.starts_with("read(a.rs)"), "got: {md}");
        assert!(md.contains("✓"), "should have success marker");
    }

    #[test]
    fn test_parallel_tool_calls_grouped() {
        let mut list = MessageList::new();

        // 3 parallel reads
        list.push_event(AgentEvent::ToolCallStart(
            "id1".into(),
            "read".into(),
            json!({"file_path": "a.rs"}),
        ));
        list.push_event(AgentEvent::ToolCallStart(
            "id2".into(),
            "read".into(),
            json!({"file_path": "b.rs"}),
        ));
        list.push_event(AgentEvent::ToolCallStart(
            "id3".into(),
            "read".into(),
            json!({"file_path": "c.rs"}),
        ));

        // Should be 1 entry with group header
        assert_eq!(list.entries.len(), 1);
        let md = list.entries[0].content_as_markdown();
        assert!(
            md.contains("read(3 files)"),
            "header should show count, got: {md}"
        );

        // Results come back
        list.push_event(AgentEvent::ToolCallResult(
            "id1".into(),
            "line1\nline2".into(),
            false,
        ));
        list.push_event(AgentEvent::ToolCallResult(
            "id2".into(),
            "content".into(),
            false,
        ));
        list.push_event(AgentEvent::ToolCallResult("id3".into(), "".into(), false));

        // Still 1 entry, results appended
        assert_eq!(list.entries.len(), 1);
        let md = list.entries[0].content_as_markdown();
        assert!(
            md.contains("⎿ a.rs ✓"),
            "should have per-file result for a.rs"
        );
        assert!(
            md.contains("⎿ b.rs ✓"),
            "should have per-file result for b.rs"
        );
        assert!(
            md.contains("⎿ c.rs ✓"),
            "should have per-file result for c.rs"
        );
    }

    #[test]
    fn test_mixed_tool_calls_not_grouped() {
        let mut list = MessageList::new();

        list.push_event(AgentEvent::ToolCallStart(
            "id1".into(),
            "read".into(),
            json!({"file_path": "a.rs"}),
        ));
        list.push_event(AgentEvent::ToolCallStart(
            "id2".into(),
            "bash".into(),
            json!({"command": "ls"}),
        ));

        // Different tool names → separate entries
        assert_eq!(list.entries.len(), 2);
        assert!(list.entries[0].content_as_markdown().starts_with("read("));
        assert!(list.entries[1].content_as_markdown().starts_with("bash("));
    }

    #[test]
    fn test_grouped_error_result() {
        let mut list = MessageList::new();

        list.push_event(AgentEvent::ToolCallStart(
            "id1".into(),
            "read".into(),
            json!({"file_path": "a.rs"}),
        ));
        list.push_event(AgentEvent::ToolCallStart(
            "id2".into(),
            "read".into(),
            json!({"file_path": "missing.rs"}),
        ));

        list.push_event(AgentEvent::ToolCallResult(
            "id1".into(),
            "content".into(),
            false,
        ));
        list.push_event(AgentEvent::ToolCallResult(
            "id2".into(),
            "file not found".into(),
            true,
        ));

        let md = list.entries[0].content_as_markdown();
        assert!(md.contains("⎿ a.rs ✓"), "success result");
        assert!(md.contains("⎿ missing.rs ✗"), "error result");
    }

    #[test]
    fn test_result_routing_without_grouping() {
        // Two different tools — results should go to correct entries
        let mut list = MessageList::new();

        list.push_event(AgentEvent::ToolCallStart(
            "id1".into(),
            "read".into(),
            json!({"file_path": "a.rs"}),
        ));
        list.push_event(AgentEvent::ToolCallStart(
            "id2".into(),
            "bash".into(),
            json!({"command": "ls"}),
        ));

        // Result for id1 should go to the read entry, not the bash entry
        list.push_event(AgentEvent::ToolCallResult(
            "id1".into(),
            "file content".into(),
            false,
        ));
        list.push_event(AgentEvent::ToolCallResult(
            "id2".into(),
            "dir listing".into(),
            false,
        ));

        assert_eq!(list.entries.len(), 2);
        let read_md = list.entries[0].content_as_markdown();
        let bash_md = list.entries[1].content_as_markdown();
        assert!(read_md.contains("✓"), "read should have result");
        assert!(bash_md.contains("✓"), "bash should have result");
    }
}

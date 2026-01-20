use crate::agent::AgentEvent;
use crate::provider::{ContentBlock, Message, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    User,
    Agent,
    Tool,
    System,
}

const TOOL_RESULT_PREVIEW_LEN: usize = 100;

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
    pub fn new(sender: Sender, content: String) -> Self {
        let mut entry = Self {
            sender,
            parts: vec![MessagePart::Text(content)],
            markdown_cache: None,
        };
        entry.update_cache();
        entry
    }

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
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
        }
    }

    /// Scroll up by n lines (towards older messages)
    pub fn scroll_up(&mut self, n: usize) {
        let max_offset = self.entries.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + n).min(max_offset);
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
        self.scroll_offset = self.entries.len().saturating_sub(1);
        self.auto_scroll = false;
    }

    /// Jump to bottom (newest messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Returns true if currently at bottom
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Get the visible slice of entries for the given viewport height.
    /// Returns (start_index, end_index) into entries.
    pub fn visible_range(&self, viewport_height: usize) -> (usize, usize) {
        let total = self.entries.len();
        if total == 0 {
            return (0, 0);
        }

        // End is total - scroll_offset (where we're "looking" from the bottom)
        let end = total.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(viewport_height);

        (start, end)
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
                self.push_entry(MessageEntry::new(Sender::Agent, delta));
            }
            AgentEvent::ThinkingDelta(delta) => {
                if let Some(last) = self.entries.last_mut()
                    && last.sender == Sender::Agent
                {
                    last.append_thinking(&delta);
                    return;
                }
                self.push_entry(MessageEntry::new_thinking(Sender::Agent, delta));
            }
            AgentEvent::ToolCallStart(_, name) => {
                self.push_entry(MessageEntry::new(
                    Sender::Tool,
                    format!("Executing {}...", name),
                ));
            }
            AgentEvent::ToolCallResult(_, result, is_error) => {
                let content = if is_error {
                    format!("Error: {}", result)
                } else {
                    // Use char-based truncation to avoid UTF-8 boundary panics
                    let truncated = if result.chars().count() > TOOL_RESULT_PREVIEW_LEN {
                        let preview: String =
                            result.chars().take(TOOL_RESULT_PREVIEW_LEN).collect();
                        format!("{}... (truncated)", preview)
                    } else {
                        result
                    };
                    format!("Result: {}", truncated)
                };
                self.push_entry(MessageEntry::new(Sender::Tool, content));
            }
            AgentEvent::PlanGenerated(plan) => {
                let mut content = String::from("### 📋 Proposed Plan\n\n");
                for task in &plan.tasks {
                    content.push_str(&format!("- **{}**: {}\n", task.title, task.description));
                    if !task.dependencies.is_empty() {
                        content.push_str(&format!(
                            "  *(Depends on: {})*\n",
                            task.dependencies.join(", ")
                        ));
                    }
                }
                self.push_entry(MessageEntry::new(Sender::System, content));
            }
            AgentEvent::CompactionStatus { .. } => {
                // Handled by TUI main loop for status bar
            }
            AgentEvent::MemoryRetrieval { results_count, .. } => {
                if results_count > 0 {
                    self.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Recalled {} relevant memories", results_count),
                    ));
                }
            }
            AgentEvent::Finished(msg) => {
                self.push_entry(MessageEntry::new(Sender::System, msg));
            }
            AgentEvent::Error(e) => {
                self.push_entry(MessageEntry::new(Sender::System, format!("Error: {}", e)));
            }
            _ => {}
        }
    }

    /// Push an entry, maintaining scroll position if user scrolled up.
    pub fn push_entry(&mut self, entry: MessageEntry) {
        // If user has scrolled up, keep their position stable
        if !self.auto_scroll {
            self.scroll_offset += 1;
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
                    // Collect all text/thinking into one entry
                    let mut parts = Vec::new();
                    for block in msg.content.iter() {
                        match block {
                            ContentBlock::Text { text } => {
                                parts.push(MessagePart::Text(text.clone()))
                            }
                            ContentBlock::Thinking { thinking } => {
                                parts.push(MessagePart::Thinking(thinking.clone()))
                            }
                            ContentBlock::ToolCall { name, .. } => {
                                self.entries.push(MessageEntry::new(
                                    Sender::Tool,
                                    format!("Called: {}", name),
                                ));
                            }
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
                            content, is_error, ..
                        } = block
                        {
                            let display = if *is_error {
                                format!("Error: {}", content)
                            } else {
                                let truncated = if content.chars().count() > TOOL_RESULT_PREVIEW_LEN
                                {
                                    let preview: String =
                                        content.chars().take(TOOL_RESULT_PREVIEW_LEN).collect();
                                    format!("{}... (truncated)", preview)
                                } else {
                                    content.clone()
                                };
                                format!("Result: {}", truncated)
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

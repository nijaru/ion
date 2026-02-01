//! Session loading, resuming, and listing.

use crate::provider::{ContentBlock, Role};
use crate::session::Session;
use crate::tui::App;
use crate::tui::message_list::{
    MessageEntry, Sender, extract_key_arg, sanitize_tool_name, strip_error_prefixes,
};
use anyhow::Result;
use tokio_util::sync::CancellationToken;

impl App {
    /// Resume an existing session by ID.
    pub fn resume_session(
        &mut self,
        session_id: &str,
    ) -> Result<(), crate::session::SessionStoreError> {
        let loaded = self.store.load(session_id)?;
        self.message_list.load_from_messages(&loaded.messages);
        self.render_state.reset_for_session_load();
        self.session = loaded;
        Ok(())
    }

    /// List recent sessions for display.
    pub fn list_recent_sessions(&self, limit: usize) -> Vec<crate::session::SessionSummary> {
        self.store.list_recent(limit).unwrap_or_default()
    }

    /// Load a session by ID and restore its state.
    pub fn load_session(&mut self, session_id: &str) -> Result<()> {
        let loaded = self.store.load(session_id)?;

        // Restore session state
        self.session = Session {
            id: loaded.id,
            working_dir: loaded.working_dir.clone(),
            model: loaded.model.clone(),
            messages: loaded.messages,
            abort_token: CancellationToken::new(),
            no_sandbox: self.permissions.no_sandbox,
        };

        // Update file completer working directory
        self.file_completer.set_working_dir(loaded.working_dir);

        // Update model display
        self.config.model = Some(loaded.model);

        // Rebuild message list from session messages
        self.message_list.clear();
        self.render_state.reset_for_session_load();
        for msg in &self.session.messages {
            match msg.role {
                Role::User => {
                    for block in msg.content.iter() {
                        if let ContentBlock::Text { text } = block {
                            self.message_list.push_user_message(text.clone());
                        }
                    }
                }
                Role::Assistant => {
                    for block in msg.content.iter() {
                        match block {
                            ContentBlock::Text { text } => {
                                self.message_list
                                    .push_entry(MessageEntry::new(Sender::Agent, text.clone()));
                            }
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => {
                                // Sanitize tool name (models sometimes embed args or XML artifacts)
                                let clean_name = sanitize_tool_name(name);
                                // Format tool call with key argument, same as live display
                                let key_arg = extract_key_arg(clean_name, arguments);
                                let display = if key_arg.is_empty() {
                                    clean_name.to_string()
                                } else {
                                    format!("{clean_name}({key_arg})")
                                };
                                self.message_list
                                    .push_entry(MessageEntry::new(Sender::Tool, display));
                            }
                            _ => {}
                        }
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            content, is_error, ..
                        } = block
                        {
                            let display = if *is_error {
                                let msg = strip_error_prefixes(content).trim();
                                let first_line = msg.lines().next().unwrap_or("");
                                format!("Error: {first_line}")
                            } else {
                                let line_count = content.lines().count();
                                if line_count > 1 {
                                    format!("{line_count} lines")
                                } else {
                                    content.chars().take(60).collect::<String>()
                                }
                            };
                            // Append to previous tool entry if exists
                            if let Some(last) = self.message_list.entries.last_mut()
                                && last.sender == Sender::Tool
                            {
                                last.append_text(&format!("\n{display}"));
                                continue;
                            }
                            self.message_list
                                .push_entry(MessageEntry::new(Sender::Tool, display));
                        }
                    }
                }
                Role::System => {} // System messages not displayed in chat
            }
        }

        Ok(())
    }
}

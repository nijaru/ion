//! Session loading and state restoration.

use crate::compaction::TokenCounter;
use crate::provider::{ContentBlock, Provider, Role};
use crate::session::Session;
use crate::tui::App;
use crate::tui::message_list::{
    MessageEntry, Sender, extract_key_arg, sanitize_tool_name, strip_error_prefixes,
};
use anyhow::Result;
use tokio_util::sync::CancellationToken;

impl App {
    /// Load a session by ID and restore its state.
    pub fn load_session(&mut self, session_id: &str) -> Result<()> {
        let loaded = self.store.load(session_id)?;

        // Restore session state
        let saved_provider = loaded.provider.clone();
        self.session = Session {
            id: loaded.id,
            working_dir: loaded.working_dir.clone(),
            model: loaded.model.clone(),
            provider: loaded.provider,
            messages: loaded.messages,
            abort_token: CancellationToken::new(),
            no_sandbox: self.permissions.no_sandbox,
        };
        self.refresh_startup_header_cache();

        // Restore provider if it differs from current
        let mut provider_warning: Option<String> = None;
        if !saved_provider.is_empty() {
            if let Some(provider) = Provider::from_id(&saved_provider) {
                if provider != self.api_provider
                    && let Err(e) = self.set_provider(provider)
                {
                    provider_warning = Some(format!(
                        "Warning: could not restore provider '{saved_provider}': {e}"
                    ));
                }
            } else {
                provider_warning = Some(format!(
                    "Warning: unknown provider '{saved_provider}' in saved session"
                ));
            }
        }

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

        // Post-rebuild warnings (after message list is populated so they appear at the end)
        if !self.session.working_dir.exists() {
            self.message_list.push_entry(MessageEntry::new(
                Sender::System,
                format!(
                    "Warning: session directory no longer exists: {}",
                    self.session.working_dir.display()
                ),
            ));
        }
        if let Some(warning) = provider_warning {
            self.message_list
                .push_entry(MessageEntry::new(Sender::System, warning));
        }

        // Compute token usage so the status line shows context % immediately.
        let ctx_window = self.agent.context_window();
        if ctx_window > 0 {
            let counter = TokenCounter::new();
            let used = counter.count_messages(&self.session.messages).total;
            self.token_usage = Some((used, ctx_window));
        }

        Ok(())
    }
}

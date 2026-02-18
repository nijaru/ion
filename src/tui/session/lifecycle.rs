//! Session loading and state restoration.

use crate::compaction::TokenCounter;
use crate::provider::{ContentBlock, Provider, Role};
use crate::session::Session;
use crate::tui::App;
use crate::tui::message_list::{
    MessageEntry, Sender, ToolMeta, extract_key_arg, format_result_content, sanitize_tool_name,
};
use anyhow::Result;
use std::collections::HashMap;
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

        // Rebuild message list from session messages.
        // Track tool call entries by ID so results can be matched to the correct call.
        self.message_list.clear();
        self.render_state.reset_for_session_load();
        let mut tool_entry_map: HashMap<String, (usize, String)> = HashMap::new();

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
                                id, name, arguments, ..
                            } => {
                                let clean_name = sanitize_tool_name(name);
                                let key_arg = extract_key_arg(clean_name, arguments);
                                let display = if key_arg.is_empty() {
                                    clean_name.to_string()
                                } else {
                                    format!("{clean_name}({key_arg})")
                                };
                                let entry_idx = self.message_list.entries.len();
                                self.message_list
                                    .push_entry(MessageEntry::new(Sender::Tool, display));
                                tool_entry_map.insert(
                                    id.clone(),
                                    (entry_idx, clean_name.to_string()),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                            ..
                        } = block
                        {
                            let (entry_idx, tool_name) =
                                if let Some(entry) = tool_entry_map.remove(tool_call_id) {
                                    entry
                                } else {
                                    // Fallback: append to last tool entry
                                    let Some(idx) = self
                                        .message_list
                                        .entries
                                        .iter()
                                        .rposition(|e| e.sender == Sender::Tool)
                                    else {
                                        continue;
                                    };
                                    (idx, String::new())
                                };

                            let expanded = self.message_list.tools_expanded;
                            let display = format_result_content(
                                Some(&tool_name),
                                content,
                                *is_error,
                                expanded,
                            );
                            if let Some(entry) = self.message_list.entries.get_mut(entry_idx)
                                && entry.sender == Sender::Tool
                            {
                                let header = entry.content_as_markdown().to_string();
                                entry.tool_meta = Some(ToolMeta {
                                    header,
                                    tool_name: tool_name.clone(),
                                    raw_result: content.clone(),
                                    is_error: *is_error,
                                });
                                entry.append_text(&format!("\n{display}"));
                            }
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

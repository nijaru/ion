//! Frame update logic - polling events and checking timeouts.

use crate::agent::AgentEvent;
use crate::tui::App;
use crate::tui::types::{CANCEL_WINDOW, Mode, SelectorPage};
use crate::tui::util::format_status_error;
use std::time::Instant;
use tracing::debug;

fn clears_retry_status(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::ToolCallStart(..) | AgentEvent::ThinkingDelta(_) | AgentEvent::TextDelta(_)
    )
}

impl App {
    /// Update state on each frame (poll events, check timeouts).
    #[allow(clippy::too_many_lines)]
    pub fn update(&mut self) {
        self.frame_count = self.frame_count.wrapping_add(1);

        // Re-trigger setup flow if needed (e.g., user pressed Esc)
        if self.needs_setup && self.mode == Mode::Input {
            if self.config.provider.is_none() {
                self.open_provider_selector();
            } else {
                self.open_model_selector();
            }
        }

        // Start fetching models if in setup mode and model selector needs them
        if self.needs_setup
            && self.mode == Mode::Selector
            && self.selector_page == SelectorPage::Model
            && !self.model_picker.has_models()
            && !self.setup_fetch_started
            && self.model_picker.error.is_none()
        {
            self.setup_fetch_started = true;
            self.model_picker.is_loading = true;
            self.fetch_models();
        }

        // Poll agent events
        while let Ok(event) = self.agent_rx.try_recv() {
            if clears_retry_status(&event) {
                self.task.retry_status = None;
            }

            match &event {
                AgentEvent::Finished(_) => {
                    self.save_task_summary(false);
                    self.is_running = false;
                    self.interaction.cancel_pending = None;
                    self.last_error = None;
                    self.message_queue = None;
                    // End thinking tracking
                    if let Some(start) = self.task.thinking_start.take() {
                        self.task.last_thinking_duration = Some(start.elapsed());
                    }
                    self.task.clear();
                    // Auto-scroll to bottom so user sees completion
                    self.message_list.scroll_to_bottom();
                }
                AgentEvent::Error(msg) => {
                    // Check if this was a cancellation
                    let was_cancelled = msg.contains("Cancelled");
                    self.save_task_summary(was_cancelled);
                    self.is_running = false;
                    self.interaction.cancel_pending = None;
                    self.message_queue = None;
                    self.task.clear();
                    if !was_cancelled {
                        self.last_error = Some(format_status_error(msg));
                        // Auto-scroll to bottom so user sees error
                        self.message_list.scroll_to_bottom();
                        self.message_list.push_event(event);
                    }
                }
                AgentEvent::ModelsFetched(models) => {
                    debug!("Received ModelsFetched event with {} models", models.len());
                    self.model_picker.set_models(models.clone());
                    if let Some(model) = models.iter().find(|m| m.id == self.session.model) {
                        if model.context_window > 0 {
                            let ctx_window = model.context_window as usize;
                            self.model_context_window = Some(ctx_window);
                            // Update agent's compaction config
                            self.agent.set_context_window(ctx_window);
                        }
                        self.agent.set_supports_vision(model.supports_vision);
                        self.model_pricing = model.pricing.clone();
                    }
                    // Select cheap model for summarization (newest among cheapest).
                    // Only update when not previewing another provider's models.
                    if self.pending_provider.is_none() {
                        let summ_model =
                            crate::provider::ModelRegistry::select_summarization_model(models);
                        if let Some(m) = summ_model {
                            debug!(
                                "Selected summarization model: {} (${}/M in)",
                                m.id, m.pricing.input
                            );
                            self.agent.set_summarization_model(Some(m.id.clone()));
                        }
                    }
                    self.last_error = None; // Clear error on success
                    // Show all models directly (user can type to filter/search)
                    self.model_picker.start_all_models();
                }
                AgentEvent::ModelFetchError(err) => {
                    debug!("Received ModelFetchError: {}", err);
                    self.model_picker.set_error(err.clone());
                    self.last_error = Some(err.clone());
                }
                AgentEvent::TokenUsage { used, max } => {
                    self.token_usage = Some((*used, *max));
                }
                AgentEvent::InputTokens(count) => {
                    // Store latest turn's input (context size), not accumulated
                    self.task.input_tokens = *count;
                }
                AgentEvent::OutputTokensDelta(count) => {
                    self.task.output_tokens += count;
                }
                AgentEvent::ProviderUsage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                } => {
                    // Provider-reported counts override local estimates
                    if *input_tokens > 0 {
                        self.task.input_tokens = *input_tokens;
                    }
                    if *output_tokens > 0 {
                        self.task.output_tokens = *output_tokens;
                    }
                    // Accumulate cost for this API call
                    let p = &self.model_pricing;
                    let cost = (*input_tokens as f64 * p.input
                        + *output_tokens as f64 * p.output
                        + *cache_read_tokens as f64 * p.cache_read.unwrap_or(0.0)
                        + *cache_write_tokens as f64 * p.cache_write.unwrap_or(0.0))
                        / 1_000_000.0;
                    self.task.cost += cost;
                }
                AgentEvent::ToolCallStart(_, name, _) => {
                    self.task.current_tool = Some(name.clone());
                    // End thinking if in progress
                    if let Some(start) = self.task.thinking_start.take() {
                        self.task.last_thinking_duration = Some(start.elapsed());
                    }
                    self.message_list.push_event(event);
                }
                AgentEvent::ToolCallResult(..) => {
                    self.task.current_tool = None;
                    self.message_list.push_event(event);
                }
                AgentEvent::ThinkingDelta(_) => {
                    // Start tracking thinking time if not already
                    if self.task.thinking_start.is_none() {
                        self.task.thinking_start = Some(Instant::now());
                    }
                    // Don't push to message_list - we don't render thinking content
                }
                AgentEvent::TextDelta(_) => {
                    // End thinking if in progress (text output started)
                    if let Some(start) = self.task.thinking_start.take() {
                        self.task.last_thinking_duration = Some(start.elapsed());
                    }
                    self.message_list.push_event(event);
                }
                AgentEvent::Retry(reason, delay) => {
                    // Show retry status in progress line (not in chat)
                    self.task.retry_status =
                        Some((reason.clone(), *delay, std::time::Instant::now()));
                }
                AgentEvent::Warning(_) => {
                    self.message_list.push_event(event);
                }
                AgentEvent::CompactionStatus { before, after } => {
                    let saved = before.saturating_sub(*after);
                    tracing::info!("Compacted: {before} -> {after} tokens ({saved} freed)");

                    use crate::tui::message_list::{MessageEntry, Sender};
                    let format_k = |n: &usize| -> String {
                        if *n >= 1000 {
                            format!("{}k", n / 1000)
                        } else {
                            n.to_string()
                        }
                    };
                    self.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!(
                            "Context compacted ({} -> {} tokens, {} freed)",
                            format_k(before),
                            format_k(after),
                            format_k(&saved),
                        ),
                    ));
                }
            }
        }

        // Poll session updates (preserves conversation history).
        // Don't re-save summary — AgentEvent::Finished already saved it
        // and cleared task.start_time, so a second save would overwrite
        // last_task_summary with None.
        if let Ok(updated_session) = self.session_rx.try_recv() {
            self.is_running = false;
            self.interaction.cancel_pending = None;
            self.message_queue = None;
            self.task.clear();

            // Auto-save to persistent storage
            if let Err(e) = self.store.save(&updated_session) {
                tracing::warn!("Failed to save session: {}", e);
            }
            self.session = updated_session;
        }

        // Clear expired cancel prompt
        if let Some(when) = self.interaction.cancel_pending
            && when.elapsed() > CANCEL_WINDOW
        {
            self.interaction.cancel_pending = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clears_retry_status;
    use crate::agent::AgentEvent;

    #[test]
    fn retry_status_clears_on_progress_events() {
        assert!(clears_retry_status(&AgentEvent::ThinkingDelta("...".to_string())));
        assert!(clears_retry_status(&AgentEvent::TextDelta("ok".to_string())));
        assert!(clears_retry_status(&AgentEvent::ToolCallStart(
            "id".to_string(),
            "read".to_string(),
            serde_json::json!({})
        )));
    }

    #[test]
    fn retry_status_preserved_on_non_progress_events() {
        assert!(!clears_retry_status(&AgentEvent::Retry(
            "Rate limited".to_string(),
            2
        )));
        assert!(!clears_retry_status(&AgentEvent::Finished(
            "done".to_string()
        )));
        assert!(!clears_retry_status(&AgentEvent::Error(
            "boom".to_string()
        )));
    }
}

//! Agent task execution and summary tracking.

use crate::tui::App;
use crate::tui::attachment::parse_attachments;
use crate::tui::types::TaskSummary;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::error;

impl App {
    /// Save task summary before clearing task state.
    pub(in crate::tui) fn save_task_summary(&mut self, was_cancelled: bool) {
        if let Some(start) = self.task.start_time {
            self.session_cost += self.task.cost;
            self.last_task_summary = Some(TaskSummary {
                elapsed: start.elapsed(),
                input_tokens: self.task.input_tokens,
                output_tokens: self.task.output_tokens,
                cost: self.task.cost,
                was_cancelled,
            });
        }
    }

    /// Run an agent task with the given input.
    #[allow(clippy::needless_pass_by_value)]
    pub(in crate::tui) fn run_agent_task(&mut self, input: String) {
        self.is_running = true;
        self.render_state.streaming_carryover.reset();
        self.task.reset();
        self.last_task_summary = None;
        self.last_error = None;

        // Reset cancellation token for new task (tokens are single-use)
        self.session.abort_token = CancellationToken::new();

        // Create shared message queue for mid-task steering
        let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.message_queue = Some(queue.clone());

        let working_dir = self.session.working_dir.clone();
        let no_sandbox = self.session.no_sandbox;
        let agent = self.agent.clone();
        let session = self.session.clone();
        let event_tx = self.agent_tx.clone();
        let session_tx = self.session_tx.clone();

        // Build thinking config from current level
        let thinking =
            self.thinking_level
                .budget_tokens()
                .map(|budget| crate::provider::ThinkingConfig {
                    enabled: true,
                    budget_tokens: Some(budget),
                });

        tokio::spawn(async move {
            let user_content = parse_attachments(&input, &working_dir, no_sandbox).await;

            let (updated_session, error) = agent
                .run_task(
                    session,
                    user_content,
                    event_tx.clone(),
                    Some(queue),
                    thinking,
                )
                .await;

            if let Some(e) = error {
                let _ = event_tx
                    .send(crate::agent::AgentEvent::Error(e.to_string()))
                    .await;
            } else {
                let _ = event_tx
                    .send(crate::agent::AgentEvent::Finished(
                        "Task completed".to_string(),
                    ))
                    .await;
            }
            // Always send session back - contains whatever work was done
            let _ = session_tx.send(updated_session).await;
        });
    }

    /// Quit the application, saving session.
    pub(in crate::tui) fn quit(&mut self) {
        self.should_quit = true;

        // Final session save (skip empty sessions)
        if let Err(e) = self.store.save(&self.session) {
            error!("Failed to save session on quit: {}", e);
        }
    }
}

//! Agent task execution and summary tracking.

use crate::tui::App;
use crate::tui::image_attachment::parse_image_attachments;
use crate::tui::types::TaskSummary;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::error;

impl App {
    /// Save task summary before clearing task state.
    pub(in crate::tui) fn save_task_summary(&mut self, was_cancelled: bool) {
        if let Some(start) = self.task_start_time {
            self.last_task_summary = Some(TaskSummary {
                elapsed: start.elapsed(),
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                was_cancelled,
            });
        }
    }

    /// Run an agent task with the given input.
    #[allow(clippy::needless_pass_by_value)]
    pub(in crate::tui) fn run_agent_task(&mut self, input: String) {
        self.is_running = true;
        self.task_start_time = Some(Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.last_task_summary = None;
        self.last_error = None;
        self.thinking_start = None;
        self.last_thinking_duration = None;

        // Reset cancellation token for new task (tokens are single-use)
        self.session.abort_token = CancellationToken::new();

        // Create shared message queue for mid-task steering
        let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.message_queue = Some(queue.clone());

        // Parse image attachments from input
        let user_content = parse_image_attachments(&input, &self.session.working_dir);

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
                    .send(crate::agent::AgentEvent::Finished("Task completed".to_string()))
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

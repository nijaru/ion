//! Agent task execution and summary tracking.

use crate::agent::AgentEvent;
use crate::tui::App;
use crate::tui::attachment::parse_attachments;
use crate::tui::message_list::{MessagePart, Sender};
use crate::tui::types::TaskSummary;
use std::fmt::Write as _;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::error;

impl App {
    /// Save task summary before clearing task state.
    pub(in crate::tui) fn save_task_summary(&mut self, was_cancelled: bool) {
        if let Some(start) = self.task.start_time {
            self.session_cost += self.task.cost;
            let summary = TaskSummary {
                elapsed: start.elapsed(),
                input_tokens: self.task.input_tokens,
                output_tokens: self.task.output_tokens,
                cost: self.task.cost,
                was_cancelled,
            };
            // Persist completion state so the progress line survives session resume.
            // Only persist completed (non-cancelled) summaries.
            if !was_cancelled {
                let _ = self.store.save_completion(
                    &self.session.id,
                    summary.elapsed.as_secs(),
                    summary.input_tokens,
                    summary.output_tokens,
                    summary.cost,
                );
            }
            self.last_task_summary = Some(summary);
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

    /// Run a shell command directly, bypassing the agent loop.
    ///
    /// Output is streamed back through `agent_tx` as a `TextDelta` + `Finished`
    /// so the existing event loop handles display and `is_running` teardown.
    pub(in crate::tui) fn run_bash_passthrough(&mut self, cmd: String) {
        self.message_list.push_user_message(format!("! {cmd}"));
        self.is_running = true;
        self.task.reset();

        let tx = self.agent_tx.clone();
        let working_dir = self.session.working_dir.clone();

        tokio::spawn(async move {
            use std::time::Duration;

            let result = tokio::time::timeout(
                Duration::from_secs(30),
                tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(&cmd)
                    .current_dir(&working_dir)
                    .output(),
            )
            .await;

            let output_str: Option<String> = match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let combined = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
                        (true, true) if out.status.success() => None,
                        (true, true) => {
                            Some(format!("exit {}", out.status.code().unwrap_or(-1)))
                        }
                        (false, true) => Some(stdout.trim_end().to_string()),
                        (true, false) => Some(stderr.trim_end().to_string()),
                        (false, false) => {
                            Some(format!("{}\n{}", stdout.trim_end(), stderr.trim_end()))
                        }
                    };
                    combined
                }
                Ok(Err(e)) => Some(format!("Error: {e}")),
                Err(_) => Some("Timed out after 30 seconds".to_string()),
            };

            if let Some(text) = output_str {
                let _ = tx.send(AgentEvent::TextDelta(text)).await;
            }
            let _ = tx.send(AgentEvent::Finished(String::new())).await;
        });
    }

    /// Serialize and persist display entries for the current session.
    ///
    /// System-sender entries are excluded: they are environment-specific
    /// (missing-dir warnings, provider warnings) and regenerated fresh on
    /// each load.
    pub(in crate::tui) fn persist_display_entries(&self, session_id: &str) {
        let entries: Vec<_> = self
            .message_list
            .entries
            .iter()
            .filter(|e| e.sender != Sender::System)
            .collect();
        match serde_json::to_string(&entries) {
            Ok(json) => {
                if let Err(e) = self.store.save_display_entries(session_id, &json) {
                    error!("Failed to save display entries: {e}");
                }
            }
            Err(e) => error!("Failed to serialize display entries: {e}"),
        }
    }

    /// Export the current session to a markdown file in the working directory.
    pub(in crate::tui) fn export_session_markdown(&mut self) {
        use crate::tui::message_list::MessageEntry;

        let mut md = String::new();
        let model = &self.session.model;
        let provider = &self.session.provider;
        let _ = writeln!(md, "# ion session export");
        let _ = writeln!(md);
        let _ = writeln!(md, "**Model:** {provider}/{model}");
        let _ = writeln!(md);
        let _ = writeln!(md, "---");
        let _ = writeln!(md);

        for entry in &self.message_list.entries {
            match entry.sender {
                Sender::System => continue, // skip internal messages
                Sender::User => {
                    let _ = writeln!(md, "## User");
                    let _ = writeln!(md);
                    for part in &entry.parts {
                        if let MessagePart::Text(text) = part {
                            let _ = writeln!(md, "{text}");
                        }
                    }
                    let _ = writeln!(md);
                }
                Sender::Agent => {
                    let _ = writeln!(md, "## Agent");
                    let _ = writeln!(md);
                    for part in &entry.parts {
                        match part {
                            MessagePart::Text(text) => {
                                let _ = writeln!(md, "{text}");
                            }
                            MessagePart::Thinking(thinking) => {
                                let _ = writeln!(md, "<details>");
                                let _ = writeln!(md, "<summary>Thinking</summary>");
                                let _ = writeln!(md);
                                let _ = writeln!(md, "{thinking}");
                                let _ = writeln!(md, "</details>");
                            }
                        }
                    }
                    let _ = writeln!(md);
                }
                Sender::Tool => {
                    if let Some(ref meta) = entry.tool_meta {
                        let _ = writeln!(md, "## Tool: {}", meta.tool_name);
                        let _ = writeln!(md);
                        let _ = writeln!(md, "**{}**", meta.header);
                        let _ = writeln!(md);
                        if meta.is_error {
                            let _ = writeln!(md, "> **Error:**");
                        }
                        let _ = writeln!(md, "```");
                        let _ = writeln!(md, "{}", meta.raw_result);
                        let _ = write!(md, "```");
                        let _ = writeln!(md);
                    } else {
                        // Fallback for entries without ToolMeta
                        let _ = writeln!(md, "## Tool");
                        let _ = writeln!(md);
                        for part in &entry.parts {
                            if let MessagePart::Text(text) = part {
                                let _ = writeln!(md, "{text}");
                            }
                        }
                    }
                    let _ = writeln!(md);
                }
            }
        }

        // Write to working directory
        let now = chrono::Local::now();
        let filename = format!("ion-export-{}.md", now.format("%Y%m%d-%H%M%S"));
        let path = self.session.working_dir.join(&filename);
        match std::fs::write(&path, &md) {
            Ok(()) => {
                self.message_list.push_entry(MessageEntry::new(
                    Sender::System,
                    format!("Exported to {filename}"),
                ));
            }
            Err(e) => {
                self.message_list.push_entry(MessageEntry::new(
                    Sender::System,
                    format!("Export failed: {e}"),
                ));
            }
        }
    }

    /// Quit the application, saving session.
    pub(in crate::tui) fn quit(&mut self) {
        self.should_quit = true;

        // Final session save (skip empty sessions)
        if let Err(e) = self.store.save(&self.session) {
            error!("Failed to save session on quit: {}", e);
        }
        self.persist_display_entries(&self.session.id.clone());
    }
}

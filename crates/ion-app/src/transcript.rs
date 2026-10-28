//! Selected, committed Session facts exported as text or offline HTML.
use std::{
    fs::OpenOptions,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use ion_ai::{Content, ImageContent, Message, Role};
use ion_core::{SessionEntry, SessionView};

mod html;
#[cfg(test)]
mod tests;
mod text;

/// The two renderers consume this same fact walk; neither interprets Session state.
trait Document {
    fn metadata(&mut self, label: &str, value: &str);
    fn heading(&mut self, value: &str);
    fn label(&mut self, value: &str);
    fn literal(&mut self, value: &str);
    fn markdown(&mut self, value: &str);
    fn thinking(&mut self, value: &str);
    fn image(&mut self, image: &ImageContent);
    fn finish(self) -> String;

    fn json(&mut self, value: &serde_json::Value) {
        self.literal(&serde_json::to_string_pretty(value).expect("JSON values serialize"));
    }
}

pub fn render(view: &SessionView) -> String {
    document(view, text::Text::new())
}

fn document(view: &SessionView, mut doc: impl Document) -> String {
    doc.metadata("Session", &view.id.to_string());
    if let Some(cwd) = &view.cwd {
        doc.metadata("Directory", &cwd.display().to_string());
    }
    if let Some(name) = &view.name {
        doc.metadata("Name", name);
    }
    if view
        .entries
        .iter()
        .any(|entry| matches!(entry, SessionEntry::ContextSelected { .. }))
    {
        doc.metadata(
            "Selected context",
            &format!(
                "through entry {} (other paths omitted; selection does not restore files)",
                view.context_position().head_entry
            ),
        );
    }
    let selected: std::collections::BTreeSet<_> = view.context_path.iter().copied().collect();
    for (entry_index, entry) in view.entries.iter().enumerate() {
        if !selected.contains(&(entry_index as u64 + 1)) {
            continue;
        }
        match entry {
            SessionEntry::Completion { turn, notices } => {
                doc.heading(&crate::transcript_render::readiness_text(*turn, notices));
            }
            SessionEntry::TurnStarted {
                turn,
                input,
                completion,
                ..
            } => {
                if completion.is_empty() {
                    doc.heading(&format!("Turn {turn} · user"));
                    message(&mut doc, input);
                } else {
                    doc.heading(&crate::transcript_render::readiness_text(*turn, completion));
                }
            }
            SessionEntry::Steering { input, .. } => {
                doc.heading("Steering");
                message(&mut doc, input);
            }
            SessionEntry::Assistant {
                message: answer, ..
            } => {
                doc.heading("Assistant");
                message(&mut doc, answer);
            }
            SessionEntry::ToolResult { result, .. } => {
                let output = result.outcome.inspection_output();
                let state = match &result.outcome {
                    ion_core::ToolOutcome::Observed { output, .. } => {
                        if output.is_error {
                            "error"
                        } else {
                            "ok"
                        }
                    }
                    ion_core::ToolOutcome::NotDispatched { .. } => "not dispatched",
                    ion_core::ToolOutcome::Unknown => "external effect unknown",
                };
                doc.heading(&format!(
                    "Tool result · {} · {} · {state}",
                    result.name, result.call_id
                ));
                if let Some(notice) = result
                    .outcome
                    .projection()
                    .and_then(ion_core::ToolResultProjection::notice)
                {
                    doc.literal(notice);
                }
                doc.json(&output.value);
                for image in &output.images {
                    doc.image(image);
                }
            }
            SessionEntry::ChildToolAdmitted { intent, .. } => {
                doc.heading(&format!(
                    "Child tool · {} · {} · parent {}:{} · child {}",
                    intent.call.name,
                    intent.call.id,
                    intent.parent.assistant_entry,
                    intent.parent.ordinal,
                    intent.child
                ));
                doc.json(&intent.call.arguments);
            }
            SessionEntry::ChildToolResult {
                parent,
                child,
                outcome,
                ..
            } => {
                let output = outcome.clone().inspection_output();
                let state = match outcome {
                    ion_core::ChildOutcome::Observed { .. } => {
                        if output.is_error {
                            "error"
                        } else {
                            "ok"
                        }
                    }
                    ion_core::ChildOutcome::NotDispatched { .. } => "not dispatched",
                    ion_core::ChildOutcome::Unknown => "external effect unknown",
                };
                doc.heading(&format!(
                    "Child result · parent {}:{} · child {} · {state}",
                    parent.assistant_entry, parent.ordinal, child
                ));
                doc.json(&output.value);
                for image in &output.images {
                    doc.image(image);
                }
            }
            SessionEntry::UserShellAdmitted {
                command,
                exclude_from_context,
            } => {
                let outcome = view.user_shell_outcome(entry_index as u64 + 1);
                let state = match outcome {
                    ion_core::UserShellOutcome::Observed { is_error: true, .. } => "error",
                    ion_core::UserShellOutcome::Observed { .. } => "ok",
                    ion_core::UserShellOutcome::Unknown => "external effect unknown",
                };
                doc.heading(&format!(
                    "User shell · {} · {state}",
                    if *exclude_from_context {
                        "not shared with model"
                    } else {
                        "shared with model"
                    }
                ));
                doc.literal(&format!("$ {command}"));
                match outcome {
                    ion_core::UserShellOutcome::Observed { output, .. } => doc.json(output),
                    ion_core::UserShellOutcome::Unknown => {
                        doc.literal(ion_core::UserShellOutcome::unknown_notice())
                    }
                }
            }
            SessionEntry::TurnEnded { reason, .. } => {
                doc.heading(&format!("Turn ended · {reason:?}"))
            }
            SessionEntry::Compacted { through_entry, .. } => {
                doc.heading(&format!("Context summarized through entry {through_entry}"))
            }
            SessionEntry::ProviderReplayRebased { .. } => {
                doc.heading("Provider reasoning context reset")
            }
            SessionEntry::ContextSelected { .. }
            | SessionEntry::CacheWarm { .. }
            | SessionEntry::ModelContextChanged { .. }
            | SessionEntry::TurnResumed { .. }
            | SessionEntry::ModelSelected { .. }
            | SessionEntry::ReasoningSelected { .. }
            | SessionEntry::EffectiveModelChanged { .. }
            | SessionEntry::UserShellSettled { .. }
            | SessionEntry::DelegationAdmitted { .. }
            | SessionEntry::DelegationTransferred { .. }
            | SessionEntry::DelegationSettled { .. }
            | SessionEntry::ChildSessionOf { .. } => {}
        }
    }
    doc.finish()
}

fn message(doc: &mut impl Document, message: &Message) {
    for part in &message.content {
        match part {
            Content::Thinking(value) => doc.thinking(value),
            Content::Text(value) if message.role == Role::Assistant => doc.markdown(value),
            Content::Text(value) => doc.literal(value),
            Content::Image(image) => doc.image(image),
            Content::ToolCall(call) => {
                doc.label(&format!("Tool call · {} · {}", call.name, call.id));
                doc.json(&call.arguments);
            }
            Content::ToolResult(result) => {
                doc.label(&format!(
                    "Tool result · {} · {}",
                    result.name, result.call_id
                ));
                doc.json(&result.result);
                for image in &result.images {
                    doc.image(image);
                }
            }
        }
    }
}

pub fn save_new(view: &SessionView, path: &Path) -> io::Result<()> {
    let is_html = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
        });
    let content = if is_html {
        document(view, html::Html::new())
    } else {
        render(view)
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if let Err(error) = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

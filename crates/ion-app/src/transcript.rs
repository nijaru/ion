//! Readable export of committed Session facts for a user-controlled file or pipe.
use std::{
    fmt::Write as _,
    fs::OpenOptions,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use ion_ai::{Content, Message};
use ion_core::{SessionEntry, SessionView};

pub fn render(view: &SessionView) -> String {
    let mut text = String::from("Ion transcript\n");
    let _ = writeln!(text, "Directory: {}", view.cwd.display());
    if let Some(name) = &view.name {
        let _ = writeln!(text, "Name: {name}");
    }
    for (entry_index, entry) in view.entries.iter().enumerate() {
        match entry {
            SessionEntry::TurnStarted { turn, input, .. } => {
                let _ = write!(text, "\nTurn {turn} · user\n");
                message(&mut text, input);
            }
            SessionEntry::Steering { input, .. } => {
                let _ = write!(text, "\nSteering\n");
                message(&mut text, input);
            }
            SessionEntry::Assistant {
                message: answer, ..
            } => {
                let _ = write!(text, "\nAssistant\n");
                message(&mut text, answer);
            }
            SessionEntry::ToolResult {
                result, projection, ..
            } => {
                let _ = writeln!(
                    text,
                    "\nTool result · {} · {}",
                    result.name,
                    if result.is_error { "error" } else { "ok" }
                );
                if let Some(notice) = projection.notice() {
                    let _ = writeln!(text, "  {notice}");
                }
                json_value(&mut text, &result.result);
                for image in &result.images {
                    let _ = writeln!(text, "  [image: {}]", image.mime_type().as_str());
                }
            }
            SessionEntry::ChildToolAdmitted { intent, .. } => {
                let _ = writeln!(
                    text,
                    "\nChild tool · {} · {} · parent {}:{} · child {}",
                    intent.call.name,
                    intent.call.id,
                    intent.parent.assistant_entry,
                    intent.parent.ordinal,
                    intent.child
                );
                json_value(&mut text, &intent.call.arguments);
            }
            SessionEntry::ChildToolResult {
                parent,
                child,
                outcome,
                ..
            } => {
                let output = outcome.clone().inspection_output();
                let _ = writeln!(
                    text,
                    "\nChild result · parent {}:{} · child {} · {}",
                    parent.assistant_entry,
                    parent.ordinal,
                    child,
                    if output.is_error { "error" } else { "ok" }
                );
                json_value(&mut text, &output.value);
                for image in &output.images {
                    let _ = writeln!(text, "  [image: {}]", image.mime_type().as_str());
                }
            }
            SessionEntry::UserShellAdmitted {
                command,
                exclude_from_context,
            } => {
                let outcome = view.user_shell_outcome(entry_index as u64 + 1);
                let _ = writeln!(
                    text,
                    "\nUser shell · {} · {}\n$ {command}",
                    if *exclude_from_context {
                        "not shared with model"
                    } else {
                        "shared with model"
                    },
                    match outcome {
                        ion_core::UserShellOutcome::Observed { is_error: true, .. } => "error",
                        ion_core::UserShellOutcome::Observed { .. } => "ok",
                        ion_core::UserShellOutcome::Unknown => "external effect unknown",
                    }
                );
                match outcome {
                    ion_core::UserShellOutcome::Observed { output, .. } => {
                        json_value(&mut text, output)
                    }
                    ion_core::UserShellOutcome::Unknown => {
                        let _ = writeln!(text, "{}", ion_core::UserShellOutcome::unknown_notice());
                    }
                }
            }
            SessionEntry::TurnEnded { reason, .. } => {
                let _ = writeln!(text, "\nTurn ended · {reason:?}");
            }
            SessionEntry::Compacted { through_entry, .. } => {
                let _ = writeln!(text, "\nContext summarized through entry {through_entry}");
            }
            SessionEntry::ProviderReplayRebased { .. } => {
                let _ = writeln!(text, "\nProvider reasoning context reset");
            }
            SessionEntry::CacheWarm { .. }
            | SessionEntry::ModelContextChanged { .. }
            | SessionEntry::ModelSelected { .. }
            | SessionEntry::EffectiveModelChanged { .. }
            | SessionEntry::UserShellSettled { .. } => {}
        }
    }
    text.chars()
        .map(|ch| {
            if ch == '\n' || ch == '\t' || !ch.is_control() {
                ch
            } else {
                '�'
            }
        })
        .collect()
}

fn message(text: &mut String, message: &Message) {
    for part in &message.content {
        match part {
            Content::Thinking(value) => {
                let _ = writeln!(text, "  Thinking:");
                for line in value.lines() {
                    let _ = writeln!(text, "    {line}");
                }
            }
            Content::Text(value) => {
                for line in value.lines() {
                    let _ = writeln!(text, "  {line}");
                }
            }
            Content::Image(image) => {
                let _ = writeln!(text, "  [image: {}]", image.mime_type().as_str());
            }
            Content::ToolCall(call) => {
                let _ = writeln!(text, "  Tool call · {} · {}", call.name, call.id);
                json_value(text, &call.arguments);
            }
            Content::ToolResult(result) => {
                let _ = writeln!(text, "  Tool result · {} · {}", result.name, result.call_id);
                json_value(text, &result.result);
                for image in &result.images {
                    let _ = writeln!(text, "  [image: {}]", image.mime_type().as_str());
                }
            }
        }
    }
}

fn json_value(text: &mut String, value: &serde_json::Value) {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    for line in rendered.lines() {
        let _ = writeln!(text, "  {line}");
    }
}

pub fn save_new(view: &SessionView, path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if let Err(error) = file.write_all(render(view).as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Message, Role};
    use ion_core::TurnEndReason;
    use std::path::PathBuf;

    #[test]
    fn export_marks_images_without_emitting_payloads() {
        let image = serde_json::from_value(serde_json::json!({
            "mime_type":"image/png",
            "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        }))
        .unwrap();
        let view = SessionView {
            cwd: PathBuf::from("/tmp/project"),
            name: None,
            entries: vec![
                SessionEntry::TurnStarted {
                    turn: 1,
                    input: Message {
                        role: Role::User,
                        content: vec![
                            Content::Text("inspect\u{1b}[31m this".into()),
                            Content::Image(image),
                        ],
                        provider_replay: None,
                    },
                    model: ion_ai::ModelRef {
                        provider: "test".into(),
                        model: "test".into(),
                    },
                },
                SessionEntry::TurnEnded {
                    turn: 1,
                    reason: TurnEndReason::Cancelled,
                },
            ],
            messages: vec![],
            unfinished_turn: None,
            unfinished_user_shell: None,
            last_end: None,
            last_model: None,
            last_effective_model: None,
            last_context: None,
            compacted_through: None,
            last_execution: None,
            last_usage: None,
        };
        let rendered = render(&view);
        assert!(rendered.contains("[image: image/png]"));
        assert!(!rendered.contains("iVBORw0KGgo"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("Cancelled"));
    }
}

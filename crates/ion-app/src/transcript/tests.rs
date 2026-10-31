use super::*;
use ion_ai::{Message, Role};
use ion_core::TurnEndReason;
use std::path::PathBuf;

fn fixture() -> SessionView {
    let image = serde_json::from_value(serde_json::json!({
            "mime_type":"image/png",
            "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        }))
        .unwrap();
    SessionView {
        id: Default::default(),
        cwd: Some(PathBuf::from("/tmp/project")),
        name: None,
        entries: vec![
            SessionEntry::TurnStarted {
                completion: Vec::new(),
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
                request: None,
                recovery: None,
            },
            SessionEntry::TurnEnded {
                turn: 1,
                reason: TurnEndReason::Cancelled,
            },
        ],
        context_path: vec![1, 2],
        messages: vec![],
        unfinished_turn: None,
        unfinished_user_shell: None,
        last_end: None,
        last_model: None,
        reasoning: ion_ai::Reasoning::ProviderDefault,
        last_effective_model: None,
        last_context: None,
        compacted_through: None,
        last_execution: None,
        last_usage: None,
    }
}

#[test]
fn export_marks_images_without_emitting_payloads() {
    let rendered = render(&fixture());
    assert!(rendered.contains("[image: image/png]"));
    assert!(!rendered.contains("iVBORw0KGgo"));
    assert!(!rendered.contains('\u{1b}'));
    assert!(rendered.contains("Cancelled"));
}

#[test]
fn html_keeps_untrusted_content_inert_and_source_available() {
    let mut doc = html::Html::new();
    let hostile = r#"</pre><script>globalThis.ION_XSS = true</script><img src="https://invalid.test/leak" onerror="alert(1)">"#;
    doc.metadata("Name", hostile);
    doc.literal(hostile);
    doc.thinking(hostile);
    doc.markdown(&format!(
            "# Heading\n\n**bold** and [safe](https://example.com/)\n\n{hostile}\n\n[bad](javascript:alert%281%29) [encoded](jav&#x09;ascript:alert%281%29)\n\n![remote](https://invalid.test/image)\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n```html\n{hostile}\n```"
        ));
    let result = doc.finish();
    assert!(result.contains("<h1>Heading</h1>"));
    assert!(result.contains("<strong>bold</strong>"));
    assert!(result.contains("<table>"));
    assert!(result.contains("href=\"https://example.com/\""));
    assert!(result.contains("Markdown source"));
    assert!(
        result.contains("javascript:alert%281%29"),
        "rejected URL lost from source"
    );
    assert!(!result.contains("<script>"));
    assert!(!result.contains("<img "));
    assert!(!result.contains("href=\"javascript:"));
    assert!(!result.contains("href=\"jav&#"));
    assert!(result.contains("&lt;/pre&gt;&lt;script&gt;"));
}

#[test]
fn both_formats_omit_inactive_facts_and_preserve_effect_states() {
    use ion_core::{ChildOutcome, ToolOccurrence, ToolOutcome, ToolSettlement};
    let mut view = fixture();
    let hidden = SessionEntry::Steering {
        turn: 1,
        input: Message::user_input("HIDDEN_ALTERNATIVE".into(), []),
    };
    view.entries.push(hidden);
    for (name, outcome) in [
        (
            "rejected",
            ToolOutcome::NotDispatched {
                reason: "NO_AUTHORITY".into(),
            },
        ),
        ("lost", ToolOutcome::Unknown),
    ] {
        view.entries.push(SessionEntry::ToolResult {
            turn: 1,
            result: ToolSettlement {
                call_id: format!("{name}-call"),
                name: name.into(),
                outcome,
            },
        });
    }
    view.entries.push(SessionEntry::ChildToolResult {
        turn: 1,
        parent: ToolOccurrence {
            assistant_entry: 4,
            ordinal: 2,
        },
        child: 3,
        outcome: ChildOutcome::Unknown,
    });
    view.entries.push(SessionEntry::UserShellAdmitted {
        command: "echo LOST_SHELL".into(),
        exclude_from_context: true,
    });
    view.context_path = vec![1, 2, 4, 5, 6, 7];
    for result in [render(&view), document(&view, html::Html::new())] {
        assert!(!result.contains("HIDDEN_ALTERNATIVE"));
        assert!(result.contains("Tool result · rejected · rejected-call · not dispatched"));
        assert!(result.contains("NO_AUTHORITY"));
        assert!(result.contains("Tool result · lost · lost-call · external effect unknown"));
        assert!(result.contains("Child result · parent 4:2 · child 3 · external effect unknown"));
        assert!(result.contains("not shared with model · external effect unknown"));
        assert!(result.contains("LOST_SHELL"));
    }
}

#[test]
fn withheld_model_images_and_raw_child_results_remain_exportable() {
    use ion_core::{
        ChildOutcome, ToolOccurrence, ToolOutcome, ToolOutput, ToolResultProjection, ToolSettlement,
    };
    let mut view = fixture();
    let SessionEntry::TurnStarted { input, .. } = &view.entries[0] else {
        panic!("fixture starts a Turn")
    };
    let Content::Image(image) = &input.content[1] else {
        panic!("fixture contains an image")
    };
    let output = ToolOutput {
        value: serde_json::json!({"raw":"RAW_OBSERVATION"}),
        images: vec![image.clone()],
        is_error: false,
    };
    view.entries.push(SessionEntry::ToolResult {
        turn: 1,
        result: ToolSettlement {
            call_id: "read".into(),
            name: "read".into(),
            outcome: ToolOutcome::Observed {
                output: output.clone(),
                projection: ToolResultProjection::ImagesUnsupported,
            },
        },
    });
    view.entries.push(SessionEntry::ChildToolResult {
        turn: 1,
        parent: ToolOccurrence {
            assistant_entry: 2,
            ordinal: 0,
        },
        child: 1,
        outcome: ChildOutcome::Observed { output },
    });
    view.context_path = vec![1, 2, 3, 4];
    let result = document(&view, html::Html::new());
    assert_eq!(result.matches("data:image/png;base64,").count(), 3);
    assert_eq!(result.matches("RAW_OBSERVATION").count(), 2);
    assert!(result.contains("Tool result · read · read · ok"));
    assert!(result.contains("Image result not shared with this text-only model"));
    assert!(result.contains("Child result · parent 2:0 · child 1 · ok"));
}

#[test]
fn html_file_embeds_images_and_refuses_to_overwrite() {
    use std::os::unix::fs::PermissionsExt;
    let directory = std::env::temp_dir().join(format!("ion-export-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("transcript.HTML");
    save_new(&fixture(), &path).unwrap();
    let result = std::fs::read_to_string(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    let error = save_new(&fixture(), &path).unwrap_err();
    let unchanged = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(result.starts_with("<!DOCTYPE html>"));
    assert!(result.contains("data:image/png;base64,iVBORw0KGgo"));
    assert_eq!(mode, 0o600);
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(result, unchanged);
}

//! Client custody/projection checks. Host's focused tests own queue, expansion,
//! stop/acceptance races and decoder lifetimes; do not duplicate those internals.
use super::activity::OperationKind;
use super::*;
use ion_host::{ControlUpdate, Host, OperationOutcome, SavedSelection, Wire};
use std::sync::Arc;

#[test]
fn settlement_program_status_distinguishes_done_and_cancelled() {
    assert_eq!(
        settlement_program_status(&OperationOutcome::Turn(Ok("answer".into()))),
        ProgramStatus::Done
    );
    assert_eq!(
        settlement_program_status(&OperationOutcome::Turn(Err(
            ion_core::CodingAgentError::Cancelled
        ))),
        ProgramStatus::Idle
    );
    assert_eq!(
        settlement_program_status(&OperationOutcome::Compact(Err(
            ion_core::CodingAgentError::Cancelled
        ))),
        ProgramStatus::Idle
    );
    assert_eq!(
        settlement_program_status(&OperationOutcome::Turn(Err(
            ion_core::CodingAgentError::EmptyPrompt
        ))),
        ProgramStatus::Error
    );
}

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) control: SessionControl<EditorDraft>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ion-terminal-control-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".git")).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let selected = host
            .models()
            .save_default(&SavedSelection {
                provider: "fixture".into(),
                model: "fixture".into(),
                endpoint: Some("http://127.0.0.1:1".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                image_input: true,
            })
            .unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let setup = ion_host::CodingApplication::new(host, None)
            .setup(session, selected, None)
            .unwrap();
        Self {
            root,
            control: setup.into_control(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn drain(
    control: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    activity: &mut Activity,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = control.next().await;
            let idle = matches!(update, ControlUpdate::Idle);
            assert!(activity.update(ui, control, update).unwrap().is_none());
            if idle {
                break;
            }
        }
    })
    .await
    .unwrap();
}

fn image(note: &str) -> ImageSource {
    let mut image = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
    image.note = Some(note.into());
    image.into()
}
fn assert_note(image: &ImageSource, expected: &str) {
    assert!(matches!(image, ImageSource::Normalized { note: Some(note), .. } if note == expected));
}

#[test]
fn returned_drafts_never_merge_identical_text_or_attachment_associations() {
    let mut ui = Frontend {
        draft: "unsent".into(),
        cursor: 2,
        images: vec![image("unsent")],
        ..Frontend::default()
    };
    ui.return_draft(
        EditorDraft {
            literal: "  same\n".into(),
            cursor: 1,
        },
        vec![image("first")],
    );
    ui.return_draft(
        EditorDraft {
            literal: "  same\n".into(),
            cursor: 4,
        },
        vec![image("second")],
    );
    assert_eq!(
        (ui.draft.as_str(), ui.cursor, ui.recovered.len()),
        ("unsent", 2, 2)
    );
    for (text, cursor, note) in [
        ("  same\n", 1, "first"),
        ("  same\n", 4, "second"),
        ("unsent", 2, "unsent"),
    ] {
        ui.recover_draft();
        assert_eq!((ui.draft.as_str(), ui.cursor), (text, cursor));
        assert_eq!(ui.images.len(), 1);
        assert_note(&ui.images[0], note);
    }
    assert_eq!(ui.recovered.len(), 2);
}

#[test]
fn returned_draft_ends_history_browsing_without_losing_unsent_input() {
    let mut ui = Frontend {
        prompt_history: vec!["older".into()],
        draft: "unsent".into(),
        cursor: 2,
        ..Frontend::default()
    };
    ui.history_previous();
    ui.return_draft(EditorDraft::at_end("returned".into()), vec![]);
    ui.history_next();
    assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent", 2));
    ui.recover_draft();
    ui.history_next();
    assert_eq!(ui.draft, "returned");
}

#[tokio::test]
async fn host_rejection_restores_exact_editor_and_images() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend {
        images: vec![image("coordinates")],
        ..Frontend::default()
    };
    let literal = "  !!shell stays literal\n";
    // No coding operation is targeted by this direct compaction ID.
    let operation = fixture
        .control
        .compact(EditorDraft::at_end("compact".into()))
        .unwrap();
    submit_editor(
        &mut fixture.control,
        &mut ui,
        Delivery::Steer(operation),
        EditorDraft {
            literal: literal.into(),
            cursor: 3,
        },
    );
    assert_eq!((ui.draft.as_str(), ui.cursor), (literal, 3));
    assert_note(&ui.images[0], "coordinates");
    assert!(ui.recovered.is_empty());
    assert!(ui.status.contains("not queued"));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn asynchronous_preparation_return_retains_raw_source_and_other_edits() {
    let mut fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".ion/prompts")).unwrap();
    fs::write(fixture.root.join(".ion/prompts/check.md"), "Expanded $1").unwrap();
    fixture.control.reload_resources().unwrap();
    let literal = "  /check café 🦀\n";
    let mut ui = Frontend {
        images: vec![ImageSource::Path(fixture.root.join("missing.png"))],
        ..Frontend::default()
    };
    submit_editor(
        &mut fixture.control,
        &mut ui,
        Delivery::Start,
        EditorDraft {
            literal: literal.into(),
            cursor: 2,
        },
    );
    assert!(ui.images.is_empty());
    ui.insert("typed while preparing");
    let mut activity = Activity::default();
    drain(&mut fixture.control, &mut ui, &mut activity).await;
    assert_eq!(ui.draft, "typed while preparing");
    ui.recover_draft();
    assert_eq!((ui.draft.as_str(), ui.cursor), (literal, 2));
    assert!(matches!(&ui.images[0], ImageSource::Path(path) if path.ends_with("missing.png")));
    assert!(
        ui.notices
            .iter()
            .any(|notice| notice.contains("preparation failed") && notice.contains("missing.png"))
    );
    // Recovery is passive: an empty Host stays empty until explicit resubmission.
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn settled_shell_is_reconciled_and_shutdown_errors_remain_visible() {
    let mut fixture = Fixture::new();
    fixture
        .control
        .shell(
            "printf terminal-control".into(),
            false,
            EditorDraft::at_end("  !printf terminal-control  ".into()),
        )
        .unwrap();
    let mut ui = Frontend::default();
    let mut activity = Activity::default();
    drain(&mut fixture.control, &mut ui, &mut activity).await;
    assert!(
        matches!(&ui.history.items[..], [TranscriptItem::UserShell(shell)] if shell.command == "printf terminal-control")
    );
    fixture
        .control
        .compact(EditorDraft::at_end("compact".into()))
        .unwrap();
    let report = fixture.control.shutdown().await;
    let mut outcome = Err(anyhow::anyhow!("terminal output disconnected"));
    for mut update in report.updates {
        if let ControlUpdate::Settled(settlement) = &mut update {
            // Inject a concurrent settlement failure at the actual UI boundary.
            settlement.outcome = OperationOutcome::Compact(Err(
                ion_core::CodingAgentError::InvalidSummary("observation commit failed"),
            ));
        }
        activity.cleanup_update(&mut ui, &fixture.control, update, &mut outcome);
    }
    let error = format!("{:#}", outcome.unwrap_err());
    assert!(error.contains("terminal output disconnected"));
    assert!(error.contains("observation commit failed"));
    assert!(ui.recovered.is_empty());
    assert!(
        ui.draft.is_empty(),
        "a settled command must not revive in the composer"
    );
}

#[tokio::test]
async fn unfinished_resync_facts_remain_inspectable_but_not_irreversibly_published() {
    let mut fixture = Fixture::new();
    let item = |turn| {
        TranscriptItem::User(ion_core::TranscriptMessage {
            turn: Some(turn),
            steering: false,
            parts: Vec::new(),
        })
    };
    for shell in [false, true] {
        let mut view = fixture.control.session().view().unwrap();
        let active = if shell {
            view.unfinished_user_shell = Some(ion_core::UnobservedUserShell { admission_entry: 2 });
            TranscriptItem::UserShell(ion_core::UserShellActivity {
                command: "pending".into(),
                exclude_from_context: false,
                outcome: ion_core::UserShellOutcome::Unknown,
            })
        } else {
            view.unfinished_turn = Some(2);
            item(2)
        };
        let mut ui = Frontend {
            history: TranscriptProjection {
                items: vec![item(1), active.clone()],
            },
            history_published_items: 1,
            pending_history_target: 2,
            pending_history_items: vec![active],
            ..Frontend::default()
        };
        ui.defer_unsettled_publication(&view);
        assert_eq!(ui.history.items.len(), 2);
        assert_eq!(ui.pending_history_target, 1);
        assert!(ui.pending_history_items.is_empty());
        // A passive reopened unknown-effect row already in native history may
        // not be rewound merely because new preparation has operation identity.
        ui.history_published_items = 2;
        ui.defer_unsettled_publication(&view);
        assert_eq!(ui.pending_history_target, 2);
    }
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn clipboard_image_keeps_typed_coordinates_and_refuses_unsupported_routes() {
    let mut fixture = Fixture::new();
    let mut loaded = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
    loaded.note = Some("clipboard coordinates".into());
    let prepared = prepare_clipboard(
        fixture.control.agent().model(),
        fixture.control.agent().limits().image_input,
        crate::clipboard_reader::PasteContent::Image {
            content: loaded.content.clone(),
            note: loaded.note.clone(),
        },
    )
    .unwrap();
    let mut ui = Frontend::default();
    apply_clipboard(&mut ui, prepared).unwrap();
    assert!(
        matches!(&ui.images[..], [ImageSource::Normalized { content, note: Some(note) }] if content == &loaded.content && note == "clipboard coordinates")
    );
    assert!(
        prepare_clipboard(
            fixture.control.agent().model(),
            false,
            crate::clipboard_reader::PasteContent::Image {
                content: loaded.content,
                note: loaded.note,
            }
        )
        .is_err()
    );
    assert_eq!(ui.images.len(), 1);
    fixture.control.shutdown().await;
}

#[test]
fn fork_selection_preserves_unsent_literal_input_and_typed_images() {
    let mut ui = Frontend {
        draft: "  unsent\n".into(),
        cursor: 3,
        images: vec![image("unsent")],
        ..Frontend::default()
    };
    let mut selected = ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255]).unwrap();
    selected.note = Some("selected".into());
    ui.restore_input(Message::user_input("selected input".into(), [selected]))
        .unwrap();
    assert_eq!(ui.draft, "selected input");
    assert_note(&ui.images[0], "selected");
    ui.recover_draft();
    assert_eq!((ui.draft.as_str(), ui.cursor), ("  unsent\n", 3));
    assert_note(&ui.images[0], "unsent");
}

#[test]
fn operation_status_keeps_controls_and_stop_intent_visible() {
    let ui = Frontend {
        status: "Details closed".into(),
        ..Frontend::default()
    };
    for (operation, label) in [
        (OperationKind::Coding, "Working"),
        (OperationKind::Shell, "Running shell"),
        (OperationKind::Compaction, "Summarizing context"),
    ] {
        let status = visible_status(
            &ui,
            Some(ActiveOperation {
                kind: operation,
                stop_requested: false,
                pending_follow_ups: 0,
            }),
        )
        .unwrap();
        assert!(status.starts_with(label));
        assert!(status.contains("Ctrl-C cancels") && status.contains("Details closed"));
    }
    let status = visible_status(
        &ui,
        Some(ActiveOperation {
            kind: OperationKind::Coding,
            stop_requested: true,
            pending_follow_ups: 0,
        }),
    )
    .unwrap();
    assert!(status.starts_with("Cancelling turn…") && !status.contains("Ctrl-C cancels"));
}

#[test]
fn shell_syntax_and_output_settings_remain_editor_operations() {
    for (literal, excluded) in [("  !echo ok  ", false), ("  !!echo 🦀  ", true)] {
        let mut ui = Frontend {
            draft: literal.into(),
            cursor: 1,
            ..Frontend::default()
        };
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None), Action::Shell { exclude_from_context, source, .. } if exclude_from_context == excluded && source.literal == literal && source.cursor == 1)
        );
    }
    let mut ui = Frontend {
        history_published_items: 7,
        pending_history_target: 7,
        ..Frontend::default()
    };
    ui.output_settings("expanded").unwrap();
    ui.output_settings("thinking show").unwrap();
    assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Expanded);
    assert_eq!(
        ui.thinking_visibility,
        crate::transcript_render::ThinkingVisibility::Visible
    );
    assert!(ui.output_settings("thinking sometimes").is_err());
    assert_eq!(
        (ui.history_published_items, ui.pending_history_target),
        (7, 7)
    );
    assert!(ui.pending_history_items.is_empty() && ui.recovered.is_empty());
}

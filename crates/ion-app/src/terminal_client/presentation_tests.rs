use super::picker::PickerItem;
use super::*;
use ion_ai::ModelRef;
use ion_core::{
    ActivityGroup, ActivityResult, ActivityState, ToolActivity, ToolActivityKind,
    ToolResultProjection, TranscriptActivity, TranscriptMessage, TranscriptPart,
};

fn history_with_tools(activities: Vec<TranscriptActivity>) -> TranscriptProjection {
    TranscriptProjection {
        items: vec![
            TranscriptItem::Assistant(TranscriptMessage {
                turn: Some(1),
                steering: false,
                parts: vec![TranscriptPart::Text("LAST_ASSISTANT_ANSWER".into())],
            }),
            TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities,
            }),
        ],
    }
}

fn tool(call_id: &str, result: Option<ActivityResult>) -> TranscriptActivity {
    TranscriptActivity {
        call_id: call_id.into(),
        name: "edit".into(),
        activity: ToolActivity {
            kind: ToolActivityKind::Edit,
            subject: Some("src/lib.rs".into()),
        },
        arguments: serde_json::json!({"path": "src/lib.rs"}),
        state: ActivityState::Completed,
        result,
        children: Vec::new(),
    }
}

#[test]
fn selected_tool_copy_is_unwrapped_recorded_evidence_and_keeps_inspector_state() {
    let long_line = format!("+{}", "x".repeat(240));
    let diff =
        format!("--- src/lib.rs\n+++ src/lib.rs\n@@ -1 +1 @@\n-old\tvalue\n{long_line}\u{1b}[2J\n");
    let mut activity = tool(
        "edit-call",
        Some(ActivityResult {
            projection: Some(ToolResultProjection::RequestLimitExceeded),
            value: serde_json::json!({
                "path": "src/lib.rs",
                "diff": {"text": diff, "truncated": true}
            }),
            image_mime_types: vec!["image/png".into()],
            is_error: false,
        }),
    );
    activity.children.push(TranscriptActivity {
        call_id: "child-call".into(),
        name: "read".into(),
        activity: ToolActivity {
            kind: ToolActivityKind::Read,
            subject: Some("src/child.rs".into()),
        },
        arguments: serde_json::json!({"path": "src/child.rs"}),
        state: ActivityState::Completed,
        result: Some(ActivityResult {
            projection: Some(ToolResultProjection::Observed),
            value: serde_json::json!({"stdout": "CHILD_RESULT"}),
            image_mime_types: vec!["image/jpeg".into()],
            is_error: false,
        }),
        children: Vec::new(),
    });
    let history = history_with_tools(vec![activity, tool("other-call", None)]);
    let mut ui = Frontend {
        draft: "unsent 🦀 draft".into(),
        cursor: "unsent ".len(),
        scroll: 9,
        images: vec![
            ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255])
                .unwrap()
                .into(),
        ],
        status: "inspector ready".into(),
        ..Frontend::default()
    };
    ui.history = history;
    ui.open_details(Some(1));
    let view = ui.details.as_mut().unwrap();
    view.prepare(&ui.history, None, 13);
    let narrow = view.copy_evidence().unwrap().1.to_owned();
    view.prepare(&ui.history, None, 200);
    let wide = view.copy_evidence().unwrap().1.to_owned();
    view.scroll = 4;

    assert_eq!(narrow, wide);
    assert!(wide.contains("Tool 1: edit"));
    assert!(wide.contains("Call ID: edit-call"));
    assert!(wide.contains("Arguments"));
    assert!(wide.contains("Result not shared with model: exceeds request limit"));
    assert!(wide.contains("Recorded edit diff · capture truncated"));
    assert!(wide.contains(&long_line));
    assert!(wide.contains('\t') && wide.contains("\u{1b}[2J"));
    assert!(wide.contains("Child activity · not included in model context"));
    assert!(wide.contains("Tool 1.1: read"));
    assert!(wide.contains("Call ID: child-call"));
    assert!(wide.contains("CHILD_RESULT"));
    assert!(wide.contains("[image: image/png]") && wide.contains("[image: image/jpeg]"));
    assert!(!wide.contains("LAST_ASSISTANT_ANSWER"));
    assert!(!wide.contains("Call ID: other-call"));

    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('x'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::CopyDetail
    ));
    assert!(ui.details.is_some());
    assert_eq!(
        (ui.draft.as_str(), ui.cursor),
        ("unsent 🦀 draft", "unsent ".len())
    );
    assert_eq!(ui.images.len(), 1);
    assert_eq!(ui.history.items.len(), 2);
    assert_eq!(ui.status, "inspector ready");
    let view = ui.details.as_ref().unwrap();
    assert_eq!(view.scroll, 4);
    assert_eq!(view.copy_evidence().unwrap().1, wide);
}

#[test]
fn conversation_ctrl_x_does_not_copy_an_assistant_answer() {
    let mut ui = Frontend {
        status: "inspector ready".into(),
        ..Frontend::default()
    };
    ui.history = history_with_tools(vec![tool("edit-call", None)]);
    ui.open_details(None);
    ui.details.as_mut().unwrap().prepare(&ui.history, None, 80);

    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('x'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::None
    ));
    assert!(ui.details.is_some());
    assert!(ui.details.as_ref().unwrap().copy_evidence().is_none());
    assert_eq!(ui.status, "No selected tool evidence; open /tool N to copy");
}

#[test]
fn selected_tool_without_a_result_copies_its_recorded_absence() {
    let mut ui = Frontend {
        history: history_with_tools(vec![tool("pending-call", None)]),
        ..Frontend::default()
    };
    ui.open_details(Some(1));
    ui.details.as_mut().unwrap().prepare(&ui.history, None, 80);

    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('x'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::CopyDetail
    ));
    let evidence = ui.details.as_ref().unwrap().copy_evidence().unwrap().1;
    assert!(evidence.contains("Call ID: pending-call"));
    assert!(evidence.contains("[no committed result]"));
}

#[test]
fn consecutive_notes_are_coalesced_without_hiding_distinct_messages() {
    let mut ui = Frontend::default();
    ui.note("same".into());
    ui.note("same".into());
    let text = |ui: &Frontend| {
        ui.pending_history_rows(80)
            .iter()
            .map(ToString::to_string)
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>()
    };
    assert_eq!(text(&ui), ["same"]);
    ui.note("different".into());
    ui.note("same".into());
    assert_eq!(text(&ui), ["same", "different", "same"]);
}

#[test]
fn local_output_keeps_history_order_across_publication_and_reflow() {
    let item = |turn, text: &str| {
        TranscriptItem::User(ion_core::TranscriptMessage {
            turn: Some(turn),
            steering: false,
            parts: vec![ion_core::TranscriptPart::Text(text.into())],
        })
    };
    let mut ui = Frontend::default();
    ui.print("before".into());
    ui.history.items.push(item(1, "first"));
    ui.pending_history_target = 1;
    ui.print("middle".into());
    let first = ui
        .pending_history_rows(24)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first.find("before").unwrap() < first.find("first").unwrap());
    assert!(first.find("first").unwrap() < first.find("middle").unwrap());
    ui.finish_history_commit();
    ui.history.items.push(item(2, "second"));
    ui.pending_history_target = 2;
    ui.print("after".into());
    // More than the former 16-entry chrome cap must not drop unpublished output.
    for index in 0..17 {
        ui.note(format!("diag {index}"));
    }
    let pending = ui
        .pending_history_rows(24)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !pending.contains("before") && !pending.contains("first") && !pending.contains("middle")
    );
    for text in ["second", "after", "diag 0", "diag 16"] {
        assert!(pending.contains(text));
    }
    ui.finish_history_commit();
    assert!(ui.pending_history_rows(24).is_empty());
    // Fullscreen uses the same source order, including already-published output.
    for width in [8, 80] {
        let rows = ui.output.rows(
            &ui.history.items,
            0..2,
            false,
            width,
            &crate::tool_output::RenderOptions::new(
                ui.output_detail,
                &ui.keys,
                ui.themes.theme(),
                ui.thinking_visibility,
            ),
        );
        assert!(rows.iter().all(|row| row.width() <= width));
        let text = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let positions =
            ["before", "first", "middle", "second", "after"].map(|key| text.find(key).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }
}

#[test]
fn short_session_labels_preserve_ascii_and_utf8() {
    for (id, label) in [
        ("", ""),
        ("short", "short"),
        ("0123456789abcdef", "0123456789ab"),
        ("01234567890🦀", "01234567890🦀"),
        ("01234567890🦀tail", "01234567890🦀"),
        ("🦀é", "🦀é"),
    ] {
        assert_eq!(super::short_session_id(id), label);
    }
}

#[test]
fn editor_line_shortcuts_keep_other_lines_and_graphemes_intact() {
    for (draft, cursor, code, expected, expected_cursor) in [
        (
            "first\ncafé 👩‍💻 tail",
            "first\ncafé 👩‍💻".len(),
            'u',
            "first\n tail",
            6,
        ),
        (
            "first\ncafé 👩‍💻 tail",
            "first\ncafé".len(),
            'k',
            "first\ncafé",
            "first\ncafé".len(),
        ),
        ("first\nsecond", 6, 'u', "firstsecond", 5),
        ("first\nsecond", 5, 'k', "firstsecond", 5),
        ("café 👩‍💻", "café 👩‍💻".len(), 'w', "café ", "café ".len()),
        ("first\nsecond", 8, 'a', "first\nsecond", 6),
        ("first\nsecond", 2, 'e', "first\nsecond", 5),
    ] {
        let mut ui = Frontend {
            draft: draft.into(),
            cursor,
            ..Frontend::default()
        };
        assert!(matches!(
            ui.key(
                KeyEvent::new(KeyCode::Char(code), Modifiers::CONTROL),
                None,
                None
            ),
            Action::None
        ));
        assert_eq!(
            (ui.draft.as_str(), ui.cursor),
            (expected, expected_cursor),
            "Ctrl-{code}"
        );
    }
}

#[test]
fn resume_history_tail_keeps_the_last_six_turns() {
    let items = (1..=9)
        .map(|turn| {
            TranscriptItem::User(ion_core::TranscriptMessage {
                turn: Some(turn),
                steering: false,
                parts: vec![ion_core::TranscriptPart::Text(format!("turn-{turn}"))],
            })
        })
        .collect::<Vec<_>>();
    let tail = resume_history_tail(&items);
    assert_eq!(tail.start, 3);
    assert_eq!(tail.omitted_turns, 3);
    assert_eq!(transcript_item_turn(&items[tail.start]), Some(4));
}

#[test]
fn resume_history_tail_bounds_shell_only_history() {
    let items = (0..40)
        .map(|index| {
            TranscriptItem::UserShell(ion_core::UserShellActivity {
                command: format!("echo {index}"),
                outcome: ion_core::UserShellOutcome::Observed {
                    output: serde_json::Value::Null,
                    is_error: false,
                },
                exclude_from_context: false,
            })
        })
        .collect::<Vec<_>>();
    let tail = resume_history_tail(&items);
    assert_eq!(tail.start, 8);
    assert_eq!(tail.omitted_turns, 0);
    assert_eq!(tail.omitted_entries, 8);
}

#[test]
fn copy_uses_the_last_completed_answer() {
    let assistant = |turn, text: &str| SessionEntry::Assistant {
        turn,
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text.into())],
            provider_replay: None,
        },
        tool_activities: Vec::new(),
        execution: ion_ai::ModelExecution {
            route: ion_ai::ModelRoute::direct(
                ModelRef {
                    provider: "test".into(),
                    model: "test".into(),
                },
                ion_ai::ModelRouteReason::UserRequest,
            ),
            returned_model: None,
        },
        usage: ion_ai::Usage::unknown(),
        termination: ion_ai::ResponseTermination::Completed,
    };
    let view = SessionView {
        id: Default::default(),
        cwd: Some(PathBuf::from("/tmp")),
        name: None,
        entries: vec![
            assistant(1, "finished"),
            SessionEntry::TurnEnded {
                turn: 1,
                reason: TurnEndReason::Completed,
            },
            assistant(2, "partial"),
        ],
        context_path: vec![1, 2, 3],
        messages: vec![],
        unfinished_turn: Some(2),
        unfinished_user_shell: None,
        last_end: None,
        last_model: None,
        reasoning: ion_ai::Reasoning::ProviderDefault,
        last_effective_model: None,
        last_context: None,
        compacted_through: None,
        last_execution: None,
        last_usage: None,
    };
    assert_eq!(last_committed_answer(&view).unwrap(), "finished");
}
#[test]
fn display_replaces_terminal_controls() {
    let mut rows = Vec::new();
    push_wrapped(&mut rows, "safe\u{1b}[31m", 30);
    assert_eq!(rows, vec!["safe�[31m"]);
}
#[test]
fn alt_enter_queues_a_followup_without_discarding_the_editor() {
    let mut ui = Frontend::default();
    ui.insert("follow up");
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), None, None),
        Action::Queue(prompt) if prompt == "follow up"
    ));
    assert!(ui.draft.is_empty());
}

#[test]
fn history_refresh_preserves_browsed_draft_and_cursor() {
    let mut ui = Frontend {
        prompt_history: vec!["older".into()],
        draft: "unsent 🦀 draft".into(),
        cursor: "unsent ".len(),
        ..Frontend::default()
    };
    ui.history_previous();
    assert_eq!(ui.draft, "older");
    // Completion/model refresh updates history, not ownership of the editor.
    ui.replace_prompt_history(vec!["older".into(), "newly committed".into()], true);
    ui.history_next();
    assert_eq!(ui.draft, "unsent 🦀 draft");
    assert_eq!(ui.cursor, "unsent ".len());
    ui.history_previous();
    ui.replace_prompt_history(vec!["different session".into()], false);
    assert_eq!(ui.draft, "unsent 🦀 draft");
    assert_eq!(ui.cursor, "unsent ".len());
}

#[test]
fn clipboard_paths_are_quoted_for_shell_and_reject_controls() {
    let paths = vec![PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/code.rs")];
    assert_eq!(
        clipboard_paths(&paths, false).unwrap(),
        "/tmp/a b.png\n/tmp/code.rs"
    );
    assert_eq!(
        clipboard_paths(&paths, true).unwrap(),
        "'/tmp/a b.png' /tmp/code.rs"
    );
    assert!(clipboard_paths(&[PathBuf::from("/tmp/bad\nname")], false).is_err());
    let mut ui = Frontend::default();
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('v'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::PasteClipboard
    ));
}

#[tokio::test]
async fn clipboard_exit_joins_cancelled_work_without_applying_input() {
    let stop = CancellationToken::new();
    let reader_stop = stop.clone();
    let (settled, mut observed) = tokio::sync::oneshot::channel();
    let mut ui = Frontend {
        draft: "retained draft".into(),
        clipboard_job: Some(ClipboardJob {
            stop,
            task: tokio::spawn(async move {
                reader_stop.cancelled().await;
                settled.send(()).unwrap();
                Ok(PreparedPaste::Text("unadmitted paste".into()))
            }),
        }),
        ..Frontend::default()
    };
    cancel_clipboard_paste(&mut ui).await.unwrap();
    assert!(observed.try_recv().is_ok());
    assert!(ui.clipboard_job.is_none());
    assert_eq!(ui.draft, "retained draft");
}

#[tokio::test]
async fn completed_file_paste_clears_reader_status() {
    let mut ui = Frontend {
        status: "Reading clipboard…".into(),
        clipboard_job: Some(ClipboardJob {
            stop: CancellationToken::new(),
            task: tokio::spawn(async {
                Ok(PreparedPaste::Files(vec![PathBuf::from(
                    "/tmp/path with spaces.txt",
                )]))
            }),
        }),
        ..Frontend::default()
    };
    finish_clipboard_paste(&mut ui).await.unwrap();
    assert_eq!(ui.draft, "/tmp/path with spaces.txt");
    assert!(ui.status.is_empty());
}

#[tokio::test]
async fn pending_clipboard_keeps_literal_input_before_admission() {
    for (literal, modifiers) in [
        ("prompt", Modifiers::NONE),
        ("  /skill:audit ARG\n", Modifiers::NONE),
        ("  /check ARG\n", Modifiers::ALT),
    ] {
        let mut ui = Frontend {
            draft: literal.into(),
            cursor: 2,
            clipboard_job: Some(ClipboardJob {
                stop: CancellationToken::new(),
                task: tokio::spawn(std::future::pending()),
            }),
            ..Frontend::default()
        };
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, modifiers), None, None),
            Action::None
        ));
        assert_eq!(ui.draft, literal);
        assert_eq!(ui.cursor, 2);
        assert!(ui.recovered.is_empty());
        ui.clipboard_job.take().unwrap().task.abort();
    }
}

#[test]
fn fork_restore_refuses_lossy_shapes_without_discarding_editor() {
    let mut multipart = Message::user_input("first".into(), []);
    multipart.content.push(Content::Text("second".into()));
    for input in [
        multipart,
        Message::user_input("x".repeat(MAX_DRAFT + 1), []),
    ] {
        let mut ui = Frontend {
            draft: "keep unsent".into(),
            cursor: 4,
            images: vec![
                ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255])
                    .unwrap()
                    .into(),
            ],
            ..Frontend::default()
        };
        assert!(ui.restore_input(input).is_err());
        assert_eq!(ui.draft, "keep unsent");
        assert_eq!(ui.cursor, 4);
        assert_eq!(ui.images.len(), 1);
    }
}

#[test]
fn command_completion_edits_without_dispatch_or_argument_loss() {
    let mut ui = Frontend {
        history_published_items: 7,
        ..Frontend::default()
    };
    for ch in "/hel".chars() {
        assert!(matches!(
            ui.key(
                KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE),
                None,
                None
            ),
            Action::None
        ));
    }
    assert!(ui.completion.active());
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None),
        Action::None
    ));
    assert_eq!(ui.draft, "/help ");
    assert!(ui.recovered.is_empty());
    assert_eq!(ui.history_published_items, 7);
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None), Action::Command(CommandInput::Editor(source)) if source.literal == "/help " && source.cursor == 6)
    );

    ui.insert("  /help focus 🦀");
    ui.cursor = "  /hel".len();
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
    assert_eq!(ui.draft, "  /help focus 🦀");
    assert_eq!(ui.cursor, "  /help ".len());
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("/re");
    // Pasted ambiguous names open discovery before accepting a proposal.
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
    assert_eq!(ui.draft, "/re");
    assert!(ui.completion.active());
    ui.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE), None, None);
    assert!(!ui.completion.active());
    assert_eq!(ui.draft, "/re");
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("/fork");
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
    assert!(ui.completion.active());
    ui.completion.rows(80, 5, &ui.keys);
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
    ui.completion.rows(80, 5, &ui.keys);
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
        Action::None
    ));
    assert_eq!(ui.draft, "/fork-after ");
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("/re");
    ui.completion
        .refresh(&ui.draft, ui.cursor, None, None, None, false);
    assert!(ui.completion.rows(80, 1, &ui.keys).is_empty());
    // A menu that cannot fit is not an invisible keyboard target.
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None), Action::Command(CommandInput::Editor(source)) if source.literal == "/re" && source.cursor == 3)
    );
}

#[test]
fn prompt_history_restores_unsent_draft_and_moves_between_lines() {
    let mut ui = Frontend {
        prompt_history: vec!["first".into(), "second\nline".into()],
        ..Frontend::default()
    };
    ui.insert("unsent");
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None, None);
    assert_eq!(ui.draft, "second\nline");
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None, None);
    assert_eq!(ui.cursor, "line".len());
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None, None);
    assert_eq!(ui.draft, "first");
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
    assert_eq!(ui.draft, "second\nline");
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
    assert_eq!(ui.draft, "unsent");
}

#[test]
fn picker_selection_stays_visible_beside_wrapped_query_and_status() {
    let mut ui = Frontend {
        status: "Ready".into(),
        ..Frontend::default()
    };
    for selected in [0, 1, 19] {
        ui.pickers.open(Picker {
            title: "Choose file",
            query: "matching-long-project-file".into(),
            selected,
            items: (0..20)
                .map(|index| PickerItem {
                    label: format!("{index:02}-matching-long-project-file.rs"),
                    detail: None,
                    value: PickerValue::Login(format!("file-{index}")),
                })
                .collect(),
        });
        for width in [8, 12, 80] {
            for height in 2..12 {
                let (rows, cursor) = modal_rows(&mut ui, None, None, width, height);
                assert_eq!(rows.len(), height as usize);
                assert!(rows.iter().all(|row| row.width() <= width as usize));
                assert!(
                    rows.iter()
                        .any(|row| row.to_string().starts_with(&format!("› {selected:02}-"))),
                    "selected item hidden at {width}x{height}"
                );
                let (row, column) = cursor.expect("query cursor stays visible");
                assert!(row < height as usize && column < width);
            }
        }
    }
}

#[test]
fn session_picker_detail_never_hides_the_selected_row() {
    let mut ui = Frontend {
        status: "Ready".into(),
        ..Frontend::default()
    };
    ui.pickers.open(Picker {
        title: "Resume session",
        query: "saved".into(),
        selected: 2,
        items: navigation::session_items(
            (0..3)
                .map(|index| ion_host::SessionSummary {
                    id: format!("019abcdef0{index:02}-4567-89ab-cdef01234567"),
                    path: PathBuf::from(format!("/session-{index}")),
                    cwd: None,
                    name: Some("saved task".into()),
                    preview: Some("saved prompt".into()),
                    model: Some(ion_ai::ModelRef {
                        provider: "smoke".into(),
                        model: "model".into(),
                    }),
                    turns: index + 1,
                    updated: std::time::UNIX_EPOCH,
                })
                .collect(),
            Some("019abcdef002-4567-89ab-cdef01234567"),
            false,
            std::time::UNIX_EPOCH,
        ),
    });
    for width in [8, 12, 80] {
        for height in 2..12 {
            let (rows, _) = modal_rows(&mut ui, None, None, width, height);
            assert_eq!(rows.len(), height as usize);
            assert!(rows.iter().all(|row| row.width() <= width as usize));
            assert!(
                rows.iter().any(|row| {
                    let row = row.to_string();
                    row.starts_with('›') && (width < 24 || row.contains("019abcdef002"))
                }),
                "selected Session hidden at {width}x{height}"
            );
            if height >= 6 && width >= 80 {
                assert!(
                    rows.iter()
                        .any(|row| row.to_string().contains("id 019abcdef")),
                    "selected metadata missing at {width}x{height}"
                );
            }
        }
    }
}

#[test]
fn resource_completion_preserves_literal_commands_and_terminal_control_names() {
    let root = std::env::temp_dir().join(format!("ion-resource-menu-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(root.join(".ion/prompts")).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::create_dir_all(root.join(".agents/skills/zzmenu-audit")).unwrap();
    fs::write(root.join(".agents/skills/zzmenu-audit/SKILL.md"), "---\nname: zzmenu-audit\ndescription: Explicit audit\ndisable-model-invocation: true\n---\nAudit instructions\n").unwrap();
    fs::write(
        root.join(".ion/prompts/zzmenu-check.md"),
        "ESC\x1b[31m 🦀\nFirst $1\n",
    )
    .unwrap();
    fs::write(
        root.join(".ion/prompts/new.md"),
        "Must not shadow terminal control\n",
    )
    .unwrap();
    let resources = Resources::load(
        &root,
        &root.join("config"),
        &ion_host::SkillSelection::default(),
    )
    .unwrap();
    let mut ui = Frontend {
        history_published_items: 7,
        ..Frontend::default()
    };
    ui.insert("/zzmenu-ch");
    ui.completion
        .refresh(&ui.draft, ui.cursor, Some(&resources), None, None, false);
    for width in [1, 8, 24, 80] {
        let rows = ui.completion.rows(width, 5, &ui.keys);
        assert!(rows.len() <= 5);
        assert!(rows.iter().all(|row| !row.contains('\x1b')
            && unicode_width::UnicodeWidthStr::width(row.as_str()) <= width));
    }
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
            Some(&resources),
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "/zzmenu-check ");
    ui.insert("concurrency");
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), Some(&resources), None), Action::Command(CommandInput::Editor(source)) if source.literal == "/zzmenu-check concurrency" && source.cursor == source.literal.len())
    );
    assert_eq!(ui.history_published_items, 7);
    ui.insert("/zzmenu-au");
    ui.key(
        KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
        Some(&resources),
        None,
    );
    assert_eq!(ui.draft, "/skill:zzmenu-audit ");
    ui.replace_draft_atomic("/ne".into());
    ui.completion
        .refresh(&ui.draft, ui.cursor, Some(&resources), None, None, false);
    assert!(
        ui.completion
            .rows(100, 5, &ui.keys)
            .iter()
            .all(|row| !row.contains("[prompt]"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn file_completion_cannot_accept_clipped_or_unpublished_candidates() {
    let root = std::env::temp_dir().join(format!("ion-file-fence-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("main.rs"), "fixture").unwrap();
    for (width, budget, unpublished) in
        [(80, 1, false), (2, 5, false), (3, 5, false), (80, 5, true)]
    {
        let mut ui = Frontend {
            cwd: root.clone(),
            ..Frontend::default()
        };
        ui.insert("@ma");
        ui.refresh_completion(None, None, false);
        while ui.pickers.has_job() {
            assert!(ui.pickers.complete().await.is_none());
        }
        ui.refresh_completion(None, None, false);
        ui.completion.rows(width, budget, &ui.keys);
        if unpublished {
            ui.completion.invalidate_visibility();
        }
        assert!(!ui.completion.visible());
        // Even one unique file needs visible selection authority. A Tab may
        // reopen suggestions, but cannot blindly replace the literal token.
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None),
            Action::None
        ));
        assert_eq!(ui.draft, "@ma");
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None), Action::Submit(text) if text == "@ma")
        );
        ui.pickers.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn automatic_file_suggestions_keep_edits_in_the_draft() {
    let root = std::env::temp_dir().join(format!("ion-complete-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fixture").unwrap();
    std::fs::write(root.join("src/skip.rs"), "ignored").unwrap();
    std::fs::write(root.join(".gitignore"), "src/skip.rs\n").unwrap();
    let mut ui = Frontend {
        cwd: root.clone(),
        ..Frontend::default()
    };
    ui.insert("Read\u{2003}@ next");
    ui.cursor = "Read\u{2003}@".len();
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('m'), Modifiers::NONE),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "Read\u{2003}@m next");
    assert!(
        ui.pickers.active().is_none(),
        "file suggestions are not a modal input owner"
    );
    assert!(
        ui.pickers.has_job(),
        "typing a file prefix should start cooperative discovery"
    );
    while ui.pickers.has_job() {
        assert!(ui.pickers.complete().await.is_none());
    }
    ui.refresh_completion(None, None, false);
    assert!(
        ui.completion
            .rows(80, 5, &ui.keys)
            .iter()
            .any(|row| row.contains("src/main.rs"))
    );
    assert!(ui.completion.visible());
    // Search edits are actual draft edits; Escape discards no input.
    ui.key(
        KeyEvent::new(KeyCode::Char('a'), Modifiers::NONE),
        None,
        None,
    );
    assert_eq!(ui.draft, "Read\u{2003}@ma next");
    let original = (ui.draft.clone(), ui.cursor);
    ui.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE), None, None);
    assert_eq!((ui.draft.clone(), ui.cursor), original);
    ui.refresh_completion(None, None, false);
    assert!(
        !ui.completion.active(),
        "drawing cannot reopen a dismissed suggestion"
    );
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
    while ui.pickers.has_job() {
        assert!(ui.pickers.complete().await.is_none());
    }
    ui.refresh_completion(None, None, false);
    ui.completion.rows(80, 5, &ui.keys);
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
        Action::None
    ));
    assert_eq!(ui.draft, "Read\u{2003}@src/main.rs next");
    assert_eq!(ui.cursor, "Read\u{2003}@src/main.rs ".len());
    // Accepting a suggestion is one undoable editor edit, never a submission.
    ui.key(
        KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
        None,
        None,
    );
    assert_eq!((ui.draft.clone(), ui.cursor), original);
    ui.pickers.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

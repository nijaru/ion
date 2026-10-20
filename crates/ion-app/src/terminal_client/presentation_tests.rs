use super::*;

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
            ui.output_detail,
            ui.thinking_visibility,
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
            ui.key(KeyEvent::new(KeyCode::Char(code), Modifiers::CONTROL), None),
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
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), None),
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
        ui.key(KeyEvent::new(KeyCode::Char('v'), Modifiers::CONTROL), None),
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
            ui.key(KeyEvent::new(KeyCode::Enter, modifiers), None),
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
            ui.key(KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), None),
            Action::None
        ));
    }
    assert!(ui.completion.active());
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None),
        Action::None
    ));
    assert_eq!(ui.draft, "/help ");
    assert!(ui.recovered.is_empty());
    assert_eq!(ui.history_published_items, 7);
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None), Action::Command(command) if command == "/help")
    );

    ui.insert("  /help focus 🦀");
    ui.cursor = "  /hel".len();
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
    assert_eq!(ui.draft, "  /help focus 🦀");
    assert_eq!(ui.cursor, "  /help ".len());
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("/re");
    // Pasted ambiguous names open discovery before accepting a proposal.
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
    assert_eq!(ui.draft, "/re");
    assert!(ui.completion.active());
    ui.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE), None);
    assert!(!ui.completion.active());
    assert_eq!(ui.draft, "/re");
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("literal");
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
    assert_eq!(ui.draft, "literal\t");
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("/fork");
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
    assert!(ui.completion.active());
    ui.completion.rows(80, 5);
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None);
    ui.completion.rows(80, 5);
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None),
        Action::None
    ));
    assert_eq!(ui.draft, "/fork-after ");
    ui.draft.clear();
    ui.cursor = 0;
    ui.insert("/re");
    ui.completion.refresh(&ui.draft, ui.cursor, None, false);
    assert!(ui.completion.rows(80, 1).is_empty());
    // A menu that cannot fit is not an invisible keyboard target.
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None), Action::Command(command) if command == "/re")
    );
}

#[test]
fn prompt_history_restores_unsent_draft_and_moves_between_lines() {
    let mut ui = Frontend {
        prompt_history: vec!["first".into(), "second\nline".into()],
        ..Frontend::default()
    };
    ui.insert("unsent");
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None);
    assert_eq!(ui.draft, "second\nline");
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None);
    assert_eq!(ui.cursor, "line".len());
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None);
    assert_eq!(ui.draft, "first");
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None);
    assert_eq!(ui.draft, "second\nline");
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None);
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
                    value: PickerValue::File {
                        path: format!("file-{index}"),
                        start: 0,
                        end: 1,
                    },
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
        items: (0..3)
            .map(|index| PickerItem {
                label: format!(
                    "{}0{index}-saved-task  saved prompt",
                    if index == 2 { "[current] " } else { "" }
                ),
                detail: Some(format!(
                    "id 019abcdef0123456789abcdef0123456{index} · {} turn(s)\nmodel smoke/model · file activity 2026-10-19 12:34:56 UTC (2m ago)",
                    index + 1
                )),
                value: PickerValue::Session(PathBuf::from(format!("/session-{index}"))),
            })
            .collect(),
    });
    for width in [8, 12, 80] {
        for height in 2..12 {
            let (rows, _) = modal_rows(&mut ui, None, None, width, height);
            assert_eq!(rows.len(), height as usize);
            assert!(rows.iter().all(|row| row.width() <= width as usize));
            assert!(
                rows.iter().any(|row| {
                    let row = row.to_string();
                    row.starts_with('›') && (width < 24 || row.contains("02-"))
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
    let resources = Resources::load(&root, &root.join("config")).unwrap();
    let mut ui = Frontend {
        history_published_items: 7,
        ..Frontend::default()
    };
    ui.insert("/zzmenu-ch");
    ui.completion
        .refresh(&ui.draft, ui.cursor, Some(&resources), false);
    for width in [1, 8, 24, 80] {
        let rows = ui.completion.rows(width, 5);
        assert!(rows.len() <= 5);
        assert!(rows.iter().all(|row| !row.contains('\x1b')
            && unicode_width::UnicodeWidthStr::width(row.as_str()) <= width));
    }
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
            Some(&resources)
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "/zzmenu-check ");
    ui.insert("concurrency");
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), Some(&resources)), Action::Command(command) if command == "/zzmenu-check concurrency")
    );
    assert_eq!(ui.history_published_items, 7);
    ui.insert("/zzmenu-au");
    ui.key(
        KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
        Some(&resources),
    );
    assert_eq!(ui.draft, "/skill:zzmenu-audit ");
    ui.replace_draft("/ne".into());
    ui.completion
        .refresh(&ui.draft, ui.cursor, Some(&resources), false);
    assert!(
        ui.completion
            .rows(100, 5)
            .iter()
            .all(|row| !row.contains("[prompt]"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn file_picker_inserts_a_selected_project_path() {
    let root = std::env::temp_dir().join(format!("ion-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("src/skip.rs"), "ignored").unwrap();
    std::fs::write(root.join(".gitignore"), "src/skip.rs\n").unwrap();
    let mut ui = Frontend {
        cwd: root.clone(),
        ..Frontend::default()
    };
    ui.insert("Read ");
    ui.key(KeyEvent::new(KeyCode::Char('@'), Modifiers::NONE), None);
    ui.key(KeyEvent::new(KeyCode::Char('m'), Modifiers::NONE), None);
    assert!(ui.pickers.complete().await.is_none());
    let picker = ui.pickers.active().unwrap();
    assert_eq!(picker.query, "m");
    assert_eq!(picker.items.len(), 1);
    assert_eq!(picker.items[0].label, "src/main.rs");
    ui.pickers.rows(80, 10);
    let chosen = ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None);
    let Action::Pick(PickerValue::File { path, start, end }) = chosen else {
        panic!("file picker did not choose a path")
    };
    ui.insert_file(path, start, end);
    assert_eq!(ui.draft, "Read @src/main.rs");
    std::fs::remove_dir_all(root).unwrap();
}

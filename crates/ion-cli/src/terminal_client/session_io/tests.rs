use ion_app::attachments::AttachmentSource;
use std::sync::{Arc, mpsc};

use ion_app::{BindingChange, ControlUpdate, Delivery, OperationOutcome, ReturnReason, UserInput};
use tokio::sync::oneshot;

use super::*;
use crate::terminal_client::{
    activity, interaction,
    tests::{Fixture, settle_io},
};

#[tokio::test]
async fn prompt_recall_excludes_automated_work_readiness() {
    let mut fixture = Fixture::new();
    let mut view = fixture.control.session().view().unwrap();
    let model = ion_ai::ModelRef {
        provider: "fixture".into(),
        model: "recall".into(),
    };
    let session = view.id;
    let turn = |turn, text: &str, completion| ion_core::SessionEntry::TurnStarted {
        turn,
        input: ion_ai::Message::user_input(text.into(), []),
        model: model.clone(),
        completion,
        request: None,
        recovery: None,
    };
    view.entries.push(turn(1, "original prompt", Vec::new()));
    view.entries.push(turn(
        2,
        "automated input",
        vec![ion_core::CompletionNotice {
            source_session: session,
            run: uuid::Uuid::nil(),
            child_session: uuid::Uuid::nil(),
        }],
    ));
    assert_eq!(History::prepare(view).prompts, ["original prompt"]);
    fixture.control.shutdown().await;
}

// The real reader has materialized its cut and released Store locks. Keep its
// actual projection leaf parked while the same terminal pump drives controls.
async fn hold_read(
    control: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    project: impl FnOnce(SessionView) -> ReadOutput + Send + 'static,
) -> mpsc::Sender<()> {
    let (started, ready) = oneshot::channel();
    let (release, held) = mpsc::channel();
    let reader = control.session().reader().unwrap();
    ui.session_io
        .start(control.session(), async move {
            Ok(reader
                .map_view(move |view| {
                    started.send(()).unwrap();
                    held.recv().unwrap();
                    project(view)
                })
                .await?)
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            ready = ready => ready.unwrap(),
            _ = ui.session_io.complete() => panic!("held read completed"),
        }
    })
    .await
    .unwrap();
    release
}

async fn finish_binding(control: &mut SessionControl<EditorDraft>, ui: &mut Frontend) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while ui.session_io.binding.is_some() {
            let update = control.next().await;
            activity::apply(ui, control, update).unwrap();
        }
    })
    .await
    .unwrap();
}

async fn sql(session: &Arc<Session>, statement: &'static str) {
    session
        .io()
        .unwrap()
        .run(move |session| {
            Ok((|| -> Result<()> {
                rusqlite::Connection::open(session.file_path()?)?.execute_batch(statement)?;
                Ok(())
            })())
        })
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_session_read_leaves_stop_and_reliable_returns_responsive() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    let release = hold_read(&fixture.control, &mut ui, |_| {
        ReadOutput::Notice("inspection joined".into())
    })
    .await;
    let literal = "  exact original 🦀\n";
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: literal.into(),
                attachments: vec![ion_app::attachments::AttachmentSource::ImageFile(
                    fixture.root.join("missing.png"),
                )],
            },
            EditorDraft {
                literal: literal.into(),
                cursor: 2,
            },
        )
        .unwrap();
    fixture.control.request_stop();
    let mut returned = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let update = tokio::select! {
                update = fixture.control.next() => update,
                _ = ui.session_io.complete() => panic!("Stop must not wait for inspection"),
            };
            let idle = matches!(update, ControlUpdate::Idle);
            if let ControlUpdate::Returned(input) = &update {
                assert!(matches!(input.reason, ReturnReason::Stopped));
                assert_eq!(input.metadata.literal, literal);
                returned = true;
            }
            activity::apply(&mut ui, &fixture.control, update).unwrap();
            if idle {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(returned);
    assert_eq!((ui.editor.draft.as_str(), ui.editor.cursor), (literal, 2));
    assert_eq!(ui.editor.attachments.len(), 1);
    assert!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    release.send(()).unwrap();
    let finished = ui.session_io.complete().await;
    assert!(
        apply(&mut fixture.control, &mut ui, finished)
            .unwrap()
            .is_none()
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn startup_history_preserves_idle_edits_and_file_discovery() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    let release = hold_read(&fixture.control, &mut ui, |view| {
        ReadOutput::History(Box::new(History::prepare(view)))
    })
    .await;
    ui.session_io.read.as_mut().unwrap().projection_revision = Some(ui.conversation.revision());
    ui.session_io.blocked = true;
    assert!(
        !ui.session_io.has_pending_effect(),
        "passive history must leave draft editing available"
    );
    std::fs::write(fixture.root.join("startup.txt"), "fixture").unwrap();
    ui.insert("Read @startup.txt");
    let scope = crate::terminal_completion::FileScope::Project(fixture.root.clone());
    ui.pickers.request_files(&scope);
    for _ in 0..3 {
        activity::apply(&mut ui, &fixture.control, ControlUpdate::Idle).unwrap();
    }
    release.send(()).unwrap();
    let finished = ui.session_io.complete().await;
    apply(&mut fixture.control, &mut ui, finished).unwrap();
    assert!(
        !ui.session_io.blocked,
        "idle input must not strand startup behind another refresh"
    );
    assert!(!ui.session_io.refresh_pending);
    ui.pickers.complete().await;
    assert!(
        ui.pickers
            .files(&scope)
            .is_some_and(|paths| paths.iter().any(|path| path == "startup.txt")),
        "the first history snapshot must not revoke concurrent file discovery"
    );
    assert_eq!(ui.editor.draft, "Read @startup.txt");
    ui.key(
        ion_terminal::KeyEvent::new(
            ion_terminal::KeyCode::Char('-'),
            ion_terminal::Modifiers::CONTROL,
        ),
        None,
        None,
    );
    assert!(
        ui.editor.draft.is_empty(),
        "startup must preserve draft undo"
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn committed_context_refresh_fault_blocks_prompt_with_attachments_until_retry() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.refresh_session(fixture.control.session()).unwrap();
    let expected = fixture
        .control
        .session()
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap()
        .context_position();
    start_binding(
        &mut fixture.control,
        &mut ui,
        BindingChange::NavigateContext {
            expected,
            head_entry: 0,
        },
        None,
    )
    .unwrap();
    finish_binding(&mut fixture.control, &mut ui).await;
    let committed = fixture
        .control
        .session()
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap();
    assert!(matches!(
        committed.entries.last(),
        Some(ion_core::SessionEntry::ContextSelected { head_entry: 0 })
    ));
    sql(
        fixture.control.session(),
        "ALTER TABLE entries RENAME TO hidden_entries",
    )
    .await;
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(ui.session_io.blocked);
    assert!(ui.session_io.refresh_error.is_some());
    assert!(
        crate::terminal_client::visible_status(&ui, None)
            .unwrap()
            .contains("prompts blocked")
    );
    ui.editor
        .attachments
        .push(ion_app::attachments::AttachmentSource::ImageFile(
            fixture.root.join("attached.png"),
        ));
    ui.editor
        .attachments
        .push(AttachmentSource::Text(ion_app::attachments::TextFile {
            name: "context.txt".into(),
            text: "context".into(),
        }));
    interaction::submit_editor(
        &mut fixture.control,
        &mut ui,
        Delivery::Start,
        EditorDraft {
            literal: "unsent 🦀".into(),
            cursor: 3,
        },
    );
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("unsent 🦀", 3)
    );
    assert_eq!(ui.editor.attachments.len(), 2);
    assert!(!fixture.control.is_busy());
    sql(
        fixture.control.session(),
        "ALTER TABLE hidden_entries RENAME TO entries",
    )
    .await;
    crate::terminal_client::command::handle_builtin(
        &mut fixture.control,
        &fixture.host,
        &mut ui,
        crate::terminal_client::Builtin::Session,
        "",
    )
    .unwrap();
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(!ui.session_io.blocked);
    assert!(ui.session_io.refresh_error.is_none());
    assert_eq!(ui.editor.draft, "unsent 🦀");
    assert_eq!(ui.editor.attachments.len(), 2);
    assert_eq!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .context_position(),
        committed.context_position()
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn stale_read_cannot_retarget_a_replaced_session() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    let old = fixture.control.session().id();
    let old_path = fixture.control.session().file_path().unwrap().to_owned();
    let switch_path = old_path.clone();
    let release = hold_read(&fixture.control, &mut ui, move |_| ReadOutput::Switch {
        path: switch_path,
        id: old,
    })
    .await;
    ui.session_io
        .retain_source(Some(EditorDraft::at_end("/resume previous".into())));
    start_binding(
        &mut fixture.control,
        &mut ui,
        BindingChange::NewSession,
        None,
    )
    .unwrap();
    finish_binding(&mut fixture.control, &mut ui).await;
    let current = fixture.control.session().id();
    assert_ne!(old, current);
    release.send(()).unwrap();
    let finished = ui.session_io.complete().await;
    assert!(
        apply(&mut fixture.control, &mut ui, finished)
            .unwrap()
            .is_none()
    );
    assert!(ui.session_io.binding.is_none());
    assert_eq!(fixture.control.session().id(), current);
    assert_eq!(ui.recovery.list().lines().count(), 2);
    assert!(ui.recovery.list().contains(&old.to_string()));
    let id = ui
        .recovery
        .list()
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let inspected = ui.recovery.inspect(id).unwrap();
    assert!(inspected.contains("/resume previous"));
    assert!(inspected.contains(&old_path.display().to_string()));
    assert!(!inspected.contains(&current.to_string()));
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(!ui.session_io.blocked);
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn stale_file_failure_and_late_binding_error_keep_original_scope_and_cause() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    let old = fixture.control.session().id();
    let old_path = fixture.control.session().file_path().unwrap().to_owned();
    ui.session_io
        .start_file_write(fixture.control.session(), async {
            anyhow::bail!("original disk failure")
        })
        .unwrap();
    ui.session_io.retain_source(Some(EditorDraft {
        literal: "/export old.html".into(),
        cursor: 3,
    }));
    start_binding(
        &mut fixture.control,
        &mut ui,
        BindingChange::SwitchSession {
            path: fixture.root.join("missing.sqlite"),
            expected_id: None,
        },
        None,
    )
    .unwrap();
    // Capture the real failed completion, but delay its UI publication until a
    // subsequent actual binding has changed the controller's current Session.
    let failed = loop {
        if let ControlUpdate::BindingFinished(finished) = fixture.control.next().await {
            break finished;
        }
    };
    assert!(failed.result.is_err());
    ui.session_io.binding.as_mut().unwrap().source =
        Some(EditorDraft::at_end("/resume missing".into()));
    fixture
        .control
        .start_binding(BindingChange::NewSession)
        .unwrap();
    loop {
        if let ControlUpdate::BindingFinished(finished) = fixture.control.next().await {
            assert!(finished.result.is_ok());
            break;
        }
    }
    let current = fixture.control.session().id();
    assert_ne!(current, old);
    binding_finished(&mut ui, &fixture.control, failed).unwrap();
    let finished = ui.session_io.complete().await;
    apply(&mut fixture.control, &mut ui, finished).unwrap();
    let list = ui.recovery.list();
    assert_eq!(list.lines().count(), 3);
    for row in list.lines().skip(1) {
        let id = row.split_whitespace().next().unwrap().parse().unwrap();
        let inspected = ui.recovery.inspect(id).unwrap();
        assert!(inspected.contains(&old.to_string()));
        assert!(inspected.contains(&old_path.display().to_string()));
        assert!(!inspected.contains(&current.to_string()));
        if inspected.contains("/export") {
            assert!(inspected.contains("original disk failure"));
            assert!(inspected.contains("Caret (UTF-8 byte offset): 3"));
        } else {
            assert!(inspected.contains("/resume missing"));
            assert!(inspected.contains("Session change failed"));
        }
    }
    assert!(ui.editor.draft.is_empty());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn late_backup_refusal_keeps_its_diagnostic_with_the_original_scope() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.session_io
        .start_file_write(fixture.control.session(), async {
            Ok(ReadOutput::RecoverySaveRefused {
                id: 1,
                path: PathBuf::from("/original/backup.json"),
                error: anyhow::anyhow!("backup already exists"),
            })
        })
        .unwrap();
    ui.session_io.retain_source(Some(EditorDraft::at_end(
        "/recovery save 1 backup.json".into(),
    )));
    // A stale completion cannot put command text into another binding's editor.
    ui.session_io.generation += 1;
    let finished = ui.session_io.complete().await;
    apply(&mut fixture.control, &mut ui, finished).unwrap();
    let list = ui.recovery.list();
    let id = list
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let inspection = ui.recovery.inspect(id).unwrap();
    assert!(inspection.contains("refused before file creation"));
    assert!(inspection.contains("backup already exists"));
    assert!(inspection.contains("/original/backup.json"));
    assert!(inspection.contains(&fixture.control.session().id().to_string()));
    assert!(ui.editor.draft.is_empty());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn file_worker_failure_keeps_unknown_source_without_overwriting_refresh_fault() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.session_io.blocked = true;
    ui.session_io.refresh_error = Some("canonical refresh fault".into());
    ui.session_io
        .start_file_write(fixture.control.session(), async {
            anyhow::bail!("write/sync or outer I/O worker failed")
        })
        .unwrap();
    ui.session_io.retain_source(Some(EditorDraft {
        literal: "/recovery save 1 uncertain.json".into(),
        cursor: 4,
    }));
    let finished = ui.session_io.complete().await;
    apply(&mut fixture.control, &mut ui, finished).unwrap();
    assert!(ui.status.starts_with("File operation failed:"));
    assert_eq!(
        ui.session_io.refresh_error.as_deref(),
        Some("canonical refresh fault")
    );
    assert!(
        ui.editor.draft.is_empty(),
        "unknown writes must not be restored for retry"
    );
    let list = ui.recovery.list();
    assert_eq!(list.lines().count(), 2);
    let id = list
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let inspection = ui.recovery.inspect(id).unwrap();
    assert!(inspection.contains("/recovery save 1 uncertain.json"));
    assert!(inspection.contains("Caret (UTF-8 byte offset): 4"));
    assert!(inspection.contains("write/sync or outer I/O worker failed"));
    assert!(inspection.contains(&fixture.control.session().id().to_string()));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn binding_changes_filesystem_scope_before_history_even_when_refresh_fails() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.refresh_session(fixture.control.session()).unwrap();
    let old = fixture.control.session().id();
    let old_path = fixture.control.session().file_path().unwrap().to_owned();
    let retained = ui.recovery.command(
        EditorDraft::at_end("/export from-A.html".into()),
        super::super::recovery::Origin::capture(fixture.control.session()),
        "old write outcome unknown".into(),
    );
    // Direct cross-cwd SwitchSession is forbidden by Host policy. Exercise the
    // frontend publication boundary with a real B controller/binding instead.
    fixture.control.shutdown().await;
    let mut bound = Fixture::new();
    let cwd_b = bound.control.session().cwd().unwrap().to_owned();
    start_binding(&mut bound.control, &mut ui, BindingChange::NewSession, None).unwrap();
    finish_binding(&mut bound.control, &mut ui).await;
    assert_ne!(bound.control.session().id(), old);
    assert_eq!(
        ui.cwd, cwd_b,
        "completion and human paths must follow the actual binding, not the history read"
    );
    sql(
        bound.control.session(),
        "ALTER TABLE entries RENAME TO hidden_entries",
    )
    .await;
    settle_io(&mut bound.control, &mut ui).await;
    let refresh_error = ui.session_io.refresh_error.clone();
    assert!(refresh_error.is_some());
    assert_eq!(ui.cwd, cwd_b);
    assert_eq!(
        ui.editor.completion.file_scope(
            "/recovery save 1 ./",
            "/recovery save 1 ./".len(),
            &ui.cwd,
            None,
            true
        ),
        Some(crate::terminal_completion::FileScope::Directory(
            cwd_b.join("./")
        ))
    );
    super::super::command::handle_builtin(
        &mut bound.control,
        &bound.host,
        &mut ui,
        super::super::Builtin::File,
        "attached.txt",
    )
    .unwrap();
    assert!(
        matches!(&ui.editor.attachments[0], AttachmentSource::File(path) if path == &cwd_b.join("attached.txt"))
    );
    super::super::command::handle_builtin(
        &mut bound.control,
        &bound.host,
        &mut ui,
        super::super::Builtin::Recovery,
        &format!("save {retained} backup.json"),
    )
    .unwrap();
    let finished = ui.session_io.complete().await;
    apply(&mut bound.control, &mut ui, finished).unwrap();
    let backup: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cwd_b.join("backup.json")).unwrap()).unwrap();
    assert_eq!(backup["origin"]["session"], old.to_string());
    assert_eq!(backup["origin"]["path"], old_path.to_str().unwrap());
    assert!(!fixture.root.join("backup.json").exists());
    assert_eq!(ui.session_io.refresh_error, refresh_error);
    sql(
        bound.control.session(),
        "ALTER TABLE hidden_entries RENAME TO entries",
    )
    .await;
    // The display is still stale; only the immutable bound cwd selects the path.
    super::super::command::handle_builtin(
        &mut bound.control,
        &bound.host,
        &mut ui,
        super::super::Builtin::Export,
        "transcript.txt",
    )
    .unwrap();
    let finished = ui.session_io.complete().await;
    apply(&mut bound.control, &mut ui, finished).unwrap();
    assert!(cwd_b.join("transcript.txt").exists());
    assert!(!fixture.root.join("transcript.txt").exists());
    bound.control.shutdown().await;
}

#[tokio::test]
async fn binding_preflight_does_not_admit_a_prompt_on_the_old_model() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    super::super::configuration::select_model(
        &fixture.control,
        &mut ui,
        fixture.host.clone(),
        super::super::configuration::ModelRequest::Pattern("missing-provider/missing-model".into()),
    )
    .unwrap();
    assert!(ui.session_io.blocks_input());
    ui.editor
        .attachments
        .push(ion_app::attachments::AttachmentSource::ImageFile(
            fixture.root.join("unsent.png"),
        ));
    interaction::submit_editor(
        &mut fixture.control,
        &mut ui,
        Delivery::Start,
        EditorDraft {
            literal: "new route prompt".into(),
            cursor: 3,
        },
    );
    assert!(!fixture.control.is_busy());
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new route prompt", 3)
    );
    assert_eq!(ui.editor.attachments.len(), 1);
    let finished = ui.session_io.complete().await;
    assert!(
        apply(&mut fixture.control, &mut ui, finished)
            .unwrap()
            .is_none()
    );
    assert!(!ui.session_io.blocks_input());
    assert_eq!(fixture.control.agent().model().provider, "fixture");
    assert!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn context_footer_tracks_planning_without_admitting_input() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    let footer = |ui: &Frontend, control: &SessionControl<EditorDraft>| {
        super::super::orientation::rows(ui, Some(control.agent().model()), 120, 2)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = fixture.control.session().view().unwrap().entries;
    ui.session_io.request_refresh();
    settle_io(&mut fixture.control, &mut ui).await;
    let report = fixture
        .control
        .agent()
        .context_report(fixture.control.session(), fixture.control.instructions())
        .unwrap();
    let expected = format!("context ~{}/?", report.estimated_input_tokens);
    assert!(
        footer(&ui, &fixture.control).contains(&expected),
        "{}",
        footer(&ui, &fixture.control)
    );
    ui.insert("unsent 🦀");
    assert!(footer(&ui, &fixture.control).contains(&expected));
    ui.session_io.schedule_refresh();
    assert!(!ui.session_io.blocks_input());
    assert!(
        !footer(&ui, &fixture.control).contains("context"),
        "pending snapshots are not current"
    );
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(footer(&ui, &fixture.control).contains(&expected));
    assert_eq!(
        serde_json::to_value(fixture.control.session().view().unwrap().entries).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(!fixture.control.is_busy());
    assert_eq!(ui.editor.draft, "unsent 🦀");

    // The native refresh must not prepare a request for a disabled footer field.
    ui.footer = serde_json::from_str(r#"{"primary":["model"],"secondary":[]}"#).unwrap();
    ui.session_io.schedule_refresh();
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(ui.context_estimate.is_none());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn binding_metadata_waits_for_current_history() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    let footer = |ui: &Frontend, control: &SessionControl<EditorDraft>| {
        super::super::orientation::rows(ui, Some(control.agent().model()), 100, 2)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    ui.session_io.request_refresh();
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(footer(&ui, &fixture.control).contains("reasoning default"));
    assert!(footer(&ui, &fixture.control).contains("context ~"));
    let context = fixture
        .control
        .agent()
        .context_report(fixture.control.session(), fixture.control.instructions());
    let release = hold_read(&fixture.control, &mut ui, move |view| {
        let mut history = History::prepare(view);
        history.context = Some(context);
        ReadOutput::History(Box::new(history))
    })
    .await;
    start_binding(
        &mut fixture.control,
        &mut ui,
        BindingChange::SelectReasoning(ion_ai::Reasoning::High),
        None,
    )
    .unwrap();
    assert!(!footer(&ui, &fixture.control).contains("reasoning"));
    finish_binding(&mut fixture.control, &mut ui).await;
    assert_eq!(
        fixture.control.session().reasoning().unwrap(),
        ion_ai::Reasoning::High
    );
    assert!(!footer(&ui, &fixture.control).contains("reasoning"));
    release.send(()).unwrap();
    let finished = ui.session_io.complete().await;
    apply(&mut fixture.control, &mut ui, finished).unwrap();
    assert!(!footer(&ui, &fixture.control).contains("reasoning"));
    assert!(!footer(&ui, &fixture.control).contains("context"));
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(footer(&ui, &fixture.control).contains("reasoning high"));
    assert!(footer(&ui, &fixture.control).contains("context ~"));

    start_binding(
        &mut fixture.control,
        &mut ui,
        BindingChange::SelectReasoning(ion_ai::Reasoning::BudgetTokens(0)),
        None,
    )
    .unwrap();
    assert!(!footer(&ui, &fixture.control).contains("reasoning"));
    finish_binding(&mut fixture.control, &mut ui).await;
    assert!(!footer(&ui, &fixture.control).contains("reasoning"));
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(footer(&ui, &fixture.control).contains("reasoning high"));
    assert_eq!(
        fixture.control.session().reasoning().unwrap(),
        ion_ai::Reasoning::High
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn stale_history_cut_does_not_erase_newer_publication() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.refresh_session(fixture.control.session()).unwrap();
    let revision = ui.conversation.revision();
    let release = hold_read(&fixture.control, &mut ui, |view| {
        ReadOutput::History(Box::new(History::prepare(view)))
    })
    .await;
    assert_eq!(
        super::super::visible_status(&ui, None).as_deref(),
        Some("Reading local state…")
    );
    assert!(ui.session_io.reports_work());
    ui.session_io.read.as_mut().unwrap().projection_revision = Some(revision);
    assert!(super::super::visible_status(&ui, None).is_none());
    assert!(!ui.session_io.reports_work());
    ui.session_io.blocked = true;
    assert!(ui.session_io.reports_work());
    ui.session_io.blocked = false;
    ui.note("newer publication".into());
    let changed = ui.conversation.revision();
    release.send(()).unwrap();
    let finished = ui.session_io.complete().await;
    assert!(
        apply(&mut fixture.control, &mut ui, finished)
            .unwrap()
            .is_none()
    );
    assert_eq!(ui.conversation.revision(), changed);
    assert!(!ui.session_io.blocks_input());
    assert!(ui.session_io.refreshing());
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(!ui.session_io.blocked);
    assert!(
        ui.pending_history_rows(80)
            .iter()
            .any(|line| line.to_string().contains("newer publication"))
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn failed_inspection_keeps_new_input_and_retains_its_command_separately() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.insert("  /session 🦀\n");
    ui.editor.cursor = 3;
    let source = ui.editor.take_literal();
    let release = hold_read(&fixture.control, &mut ui, |_| {
        ReadOutput::Notice("inspection completed".into())
    })
    .await;
    ui.session_io.retain_source(Some(source));
    ui.insert("new unsent 雪");
    ui.editor.cursor = "new ".len();
    ui.attach(ion_app::attachments::AttachmentSource::ImageFile(
        fixture.root.join("new.png"),
    ));
    release.send(()).unwrap();
    let mut finished = ui.session_io.complete().await;
    // Exercise the real terminal's asynchronous error boundary, not an
    // invented Session journal failure or a model/effect executor.
    finished.result = Err(anyhow::anyhow!("inspection fixture failure"));
    assert!(
        apply(&mut fixture.control, &mut ui, finished)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new unsent 雪", "new ".len())
    );
    assert!(
        matches!(&ui.editor.attachments[..], [ion_app::attachments::AttachmentSource::ImageFile(path)] if path == &fixture.root.join("new.png"))
    );
    assert!(
        ui.pending_history_rows(100)
            .iter()
            .any(|line| line.to_string().contains("Command text retained"))
    );
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("  /session 🦀\n", 3)
    );
    assert!(ui.editor.attachments.is_empty());
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new unsent 雪", "new ".len())
    );
    assert!(
        matches!(&ui.editor.attachments[..], [ion_app::attachments::AttachmentSource::ImageFile(path)] if path == &fixture.root.join("new.png"))
    );
    assert!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn not_dispatched_retains_new_input_and_returns_only_its_command_text() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.insert("  /command:check 🦀\n");
    ui.editor.cursor = 3;
    let source = ui.editor.take_literal();
    fixture.control.compact(None, source).unwrap();
    ui.insert("new unsent 雪");
    ui.editor.cursor = "new ".len();
    ui.attach(ion_app::attachments::AttachmentSource::ImageFile(
        fixture.root.join("new.png"),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut update = fixture.control.next().await;
            if let ControlUpdate::Settled(settlement) = &mut update {
                settlement.outcome =
                    OperationOutcome::NotDispatched(anyhow::anyhow!("manifest changed"));
                let error = activity::apply(&mut ui, &fixture.control, update)
                    .unwrap()
                    .unwrap();
                assert!(format!("{error:#}").contains("no executor invoked"));
                break;
            }
            activity::apply(&mut ui, &fixture.control, update).unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new unsent 雪", "new ".len())
    );
    assert!(
        matches!(&ui.editor.attachments[..], [ion_app::attachments::AttachmentSource::ImageFile(path)] if path == &fixture.root.join("new.png"))
    );
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("  /command:check 🦀\n", 3)
    );
    assert!(ui.editor.attachments.is_empty());
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new unsent 雪", "new ".len())
    );
    assert!(
        matches!(&ui.editor.attachments[..], [ion_app::attachments::AttachmentSource::ImageFile(path)] if path == &fixture.root.join("new.png"))
    );
    assert!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn only_not_dispatched_settlement_returns_exact_direct_source() {
    let mut fixture = Fixture::new();
    let source = EditorDraft {
        literal: "  /command:check 🦀\n".into(),
        cursor: 3,
    };
    fixture.control.compact(None, source).unwrap();
    let mut ui = Frontend::default();
    ui.editor
        .attachments
        .push(ion_app::attachments::AttachmentSource::ImageFile(
            fixture.root.join("unsent.png"),
        ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut update = fixture.control.next().await;
            if let ControlUpdate::Settled(settlement) = &mut update {
                // Exercise the terminal's typed boundary without fabricating a
                // Session journal result or invoking a model/effect executor.
                settlement.outcome =
                    OperationOutcome::NotDispatched(anyhow::anyhow!("manifest changed"));
                let error = activity::apply(&mut ui, &fixture.control, update)
                    .unwrap()
                    .unwrap();
                assert!(format!("{error:#}").contains("no executor invoked"));
                break;
            }
            activity::apply(&mut ui, &fixture.control, update).unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("  /command:check 🦀\n", 3)
    );
    assert_eq!(ui.editor.attachments.len(), 1);
    assert!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    fixture.control.shutdown().await;
}

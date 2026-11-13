use std::sync::{Arc, mpsc};

use ion_app::{BindingChange, ControlUpdate, Delivery, OperationOutcome, ReturnReason, UserInput};
use tokio::sync::oneshot;

use super::*;
use crate::terminal_client::{
    activity, interaction,
    tests::{Fixture, settle_io},
};

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
                rusqlite::Connection::open(session.path())?.execute_batch(statement)?;
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
                images: vec![ion_app::image_input::ImageSource::Path(
                    fixture.root.join("missing.png"),
                )],
                files: vec![],
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
    assert_eq!(ui.editor.images.len(), 1);
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
        .images
        .push(ion_app::image_input::ImageSource::Path(
            fixture.root.join("attached.png"),
        ));
    ui.editor.files.push(ion_app::prompt_files::TextFile {
        name: "context.txt".into(),
        text: "context".into(),
    });
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
    assert_eq!(ui.editor.images.len(), 1);
    assert_eq!(ui.editor.files.len(), 1);
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
    assert_eq!(ui.editor.images.len(), 1);
    assert_eq!(ui.editor.files.len(), 1);
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
    let old_path = fixture.control.session().path().to_owned();
    let release = hold_read(&fixture.control, &mut ui, move |_| {
        ReadOutput::Switch(old_path)
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
    assert_eq!(ui.uncertain_commands.len(), 1);
    assert_eq!(ui.uncertain_commands[0].literal, "/resume previous");
    settle_io(&mut fixture.control, &mut ui).await;
    assert!(!ui.session_io.blocked);
    fixture.control.shutdown().await;
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
        .images
        .push(ion_app::image_input::ImageSource::Path(
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
    assert_eq!(ui.editor.images.len(), 1);
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
async fn stale_history_cut_does_not_erase_newer_publication() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.refresh_session(fixture.control.session()).unwrap();
    let revision = ui.conversation.revision();
    let release = hold_read(&fixture.control, &mut ui, |view| {
        ReadOutput::History(Box::new(History::prepare(view)))
    })
    .await;
    ui.session_io.read.as_mut().unwrap().projection_revision = Some(revision);
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
    assert!(ui.session_io.blocked);
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
    ui.attach_image(ion_app::image_input::ImageSource::Path(
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
        matches!(&ui.editor.images[..], [ion_app::image_input::ImageSource::Path(path)] if path == &fixture.root.join("new.png"))
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
    assert!(ui.editor.images.is_empty());
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new unsent 雪", "new ".len())
    );
    assert!(
        matches!(&ui.editor.images[..], [ion_app::image_input::ImageSource::Path(path)] if path == &fixture.root.join("new.png"))
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
    ui.attach_image(ion_app::image_input::ImageSource::Path(
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
        matches!(&ui.editor.images[..], [ion_app::image_input::ImageSource::Path(path)] if path == &fixture.root.join("new.png"))
    );
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("  /command:check 🦀\n", 3)
    );
    assert!(ui.editor.images.is_empty());
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new unsent 雪", "new ".len())
    );
    assert!(
        matches!(&ui.editor.images[..], [ion_app::image_input::ImageSource::Path(path)] if path == &fixture.root.join("new.png"))
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
        .images
        .push(ion_app::image_input::ImageSource::Path(
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
    assert_eq!(ui.editor.images.len(), 1);
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

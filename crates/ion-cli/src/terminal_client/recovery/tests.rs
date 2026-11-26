use super::*;
use crate::terminal_client::tests::Fixture;
use ion_ai::{Message, ModelRef, ScriptedModelService};
use ion_core::{Agent, InputBudget, PreparedInput, ToolSet};
use std::{os::unix::fs::PermissionsExt, sync::Arc};

const LITERAL: &str = "  exact original 🦀\n";

async fn unresolved(fixture: &mut Fixture) -> UnresolvedInput<EditorDraft> {
    // A real deferred COMMIT failure leaves the app's original wrapper intact.
    rusqlite::Connection::open(fixture.control.session().file_path().unwrap()).unwrap().execute_batch(
        "CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
         CREATE TABLE commit_child (parent INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER uncertain_commit AFTER INSERT ON entries
         BEGIN INSERT INTO commit_child(parent) VALUES (999); END;"
    ).unwrap();
    fixture
        .control
        .submit(
            ion_app::Delivery::Start,
            UserInput::Prompt {
                text: LITERAL.into(),
                attachments: vec![AttachmentSource::Text(ion_app::attachments::TextFile {
                    name: "original context.txt".into(),
                    text: "context 🦀".into(),
                })],
            },
            EditorDraft {
                literal: LITERAL.into(),
                cursor: 2,
            },
        )
        .unwrap();
    let input = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let ion_app::ControlUpdate::Unresolved(input) = fixture.control.next().await {
                break input;
            }
        }
    })
    .await
    .unwrap();
    while !matches!(fixture.control.next().await, ion_app::ControlUpdate::Idle) {}
    input
}

fn pending() -> PreparedInput<()> {
    InputBudget::default()
        .prepare(
            Message::user_input(LITERAL.into(), []),
            (),
            ion_core::AgentLimits::default(),
        )
        .unwrap()
}

#[tokio::test]
async fn source_inspection_save_and_discard_preserve_exact_custody_and_fence() {
    let mut fixture = Fixture::new();
    let input = unresolved(&mut fixture).await;
    let origin = Origin::capture(fixture.control.session());
    let mut ui = Frontend {
        cwd: fixture.root.clone(),
        ..Frontend::default()
    };
    let id = ui.recovery.input(input, origin).unwrap();
    let inspected = ui.recovery.inspect(id).unwrap();
    assert!(inspected.contains(&serde_json::to_string(LITERAL).unwrap()));
    assert!(inspected.contains("Caret (UTF-8 byte offset): 2"));
    assert!(inspected.contains("original context.txt"));
    assert!(inspected.contains("Status: acceptance unknown"));
    assert!(inspected.contains(&fixture.control.session().id().to_string()));
    assert!(
        inspected.contains(
            &fixture
                .control
                .session()
                .file_path()
                .unwrap()
                .display()
                .to_string()
        )
    );
    assert_eq!(ui.recovery.list().lines().count(), 2);
    assert!(ui.recovery.blocks(fixture.control.session().id()));
    assert!(!ui.recovery.blocks(uuid::Uuid::now_v7()));

    // Inspection never emits image payloads, but serialization retains them.
    if let Source::Input(input) = &mut ui.recovery.records[0].source {
        let UserInput::Prompt { attachments, .. } = &mut input.input else {
            panic!()
        };
        attachments.push(AttachmentSource::image_bytes(vec![173; 8192]));
        attachments.push(AttachmentSource::File(
            fixture.root.join("deleted-context.txt"),
        ));
        attachments.push(AttachmentSource::ImageFile(
            fixture.root.join("deleted-image.png"),
        ));
    }
    let inspected = ui.recovery.inspect(id).unwrap();
    assert!(inspected.contains("8192 bytes"));
    assert!(!inspected.contains("ra2tra2t"));
    assert!(inspected.contains("path reference only; contents are not preserved"));

    command(
        &fixture.control,
        &mut ui,
        &format!("save {id} \"backup 🦀 \\\"quoted\\\".json\""),
    )
    .unwrap();
    assert!(ui.session_io.has_pending_effect());
    ui.cwd = fixture.root.join("not-the-captured-cwd");
    let finished = ui.session_io.complete().await;
    super::super::session_io::apply(&mut fixture.control, &mut ui, finished).unwrap();
    assert!(ui.status.contains("inspection backup saved"));
    assert!(!ui.status.contains("Transcript"));
    let path = fixture.root.join("backup 🦀 \"quoted\".json");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["source"]["editor"]["literal"], LITERAL);
    assert_eq!(saved["source"]["editor"]["cursor"], 2);
    assert_eq!(
        saved["source"]["input"]["Prompt"]["attachments"][0]["Text"]["text"],
        "context 🦀"
    );
    assert_eq!(
        saved["origin"]["session"],
        fixture.control.session().id().to_string()
    );
    assert_eq!(saved["status"]["state"], "unresolved");
    assert_eq!(
        saved["source"]["input"]["Prompt"]["attachments"][2]["File"],
        fixture.root.join("deleted-context.txt").to_str().unwrap()
    );
    assert_eq!(
        saved["source"]["input"]["Prompt"]["attachments"][3]["ImageFile"],
        fixture.root.join("deleted-image.png").to_str().unwrap()
    );

    let refused_command = format!("  /recovery save {id} {}", path.display());
    command(
        &fixture.control,
        &mut ui,
        &format!("save {id} {}", path.display()),
    )
    .unwrap();
    ui.session_io.retain_source(Some(EditorDraft {
        literal: refused_command.clone(),
        cursor: 3,
    }));
    ui.session_io.blocked = true;
    ui.session_io.refresh_error = Some("existing canonical refresh fault".into());
    let finished = ui.session_io.complete().await;
    super::super::session_io::apply(&mut fixture.control, &mut ui, finished).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "save must not overwrite"
    );
    assert_eq!(
        ui.recovery.records.len(),
        1,
        "an open refusal must not create an uncertain command record"
    );
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        (refused_command.as_str(), 3)
    );
    assert!(ui.status.contains("Recovery backup was not saved"));
    assert_eq!(
        ui.session_io.refresh_error.as_deref(),
        Some("existing canonical refresh fault")
    );
    assert!(!fixture.control.is_busy());
    assert!(
        ui.recovery
            .inspect(id)
            .unwrap()
            .contains("original context.txt")
    );
    ui.editor.take_literal();
    ui.session_io.blocked = false;
    command(&fixture.control, &mut ui, &format!("discard {id}")).unwrap();
    assert!(
        ui.recovery
            .inspect(id)
            .unwrap()
            .contains("source discarded")
    );
    assert!(ui.recovery.blocks(fixture.control.session().id()));
    assert!(ui.editor.draft.is_empty());
    assert!(ui.editor.recovered.is_empty());
    ui.reconcile_recovery(fixture.control.session().id())
        .unwrap();
    assert!(ui.recovery.blocks(fixture.control.session().id()));
    crate::terminal_client::interaction::submit_editor(
        &mut fixture.control,
        &mut ui,
        ion_app::Delivery::Start,
        EditorDraft {
            literal: "new after discard".into(),
            cursor: 3,
        },
    );
    assert!(ui.status.contains("/recovery"));
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("new after discard", 3)
    );
    assert!(!fixture.control.is_busy());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn serialization_and_missing_parent_refuse_before_creating_a_backup() {
    use std::os::unix::ffi::OsStringExt;
    let mut fixture = Fixture::new();
    let mut ui = Frontend {
        cwd: fixture.root.clone(),
        ..Frontend::default()
    };
    let id = ui.recovery.command(
        EditorDraft::at_end("/export original.html".into()),
        Origin::capture(fixture.control.session()),
        "original write failed".into(),
    );
    for serialization_refusal in [false, true] {
        if serialization_refusal {
            ui.recovery.records[0].origin.path =
                Some(PathBuf::from(std::ffi::OsString::from_vec(vec![
                    b'/', 0xff,
                ])));
        }
        let target = if serialization_refusal {
            fixture.root.join("not-created.json")
        } else {
            fixture.root.join("missing-parent/backup.json")
        };
        let source = format!("/recovery save {id} {}", target.display());
        command(
            &fixture.control,
            &mut ui,
            &format!("save {id} {}", target.display()),
        )
        .unwrap();
        ui.session_io.retain_source(Some(EditorDraft {
            literal: source.clone(),
            cursor: 2,
        }));
        let finished = ui.session_io.complete().await;
        super::super::session_io::apply(&mut fixture.control, &mut ui, finished).unwrap();
        assert!(!target.exists());
        assert_eq!(ui.recovery.records.len(), 1);
        assert_eq!(
            (ui.editor.draft.as_str(), ui.editor.cursor),
            (source.as_str(), 2)
        );
        assert!(ui.status.contains("Recovery backup was not saved"));
        assert!(!fixture.control.is_busy());
        ui.editor.take_literal();
    }
    // The same owned result must remain observable if quit overtakes completion.
    command(&fixture.control, &mut ui, &format!("save {id} joined.json")).unwrap();
    let source = format!("  /recovery save {id} joined.json");
    ui.session_io.retain_source(Some(EditorDraft {
        literal: source.clone(),
        cursor: 4,
    }));
    let finished = ui.session_io.complete().await;
    let mut outcome = Ok(());
    super::super::session_io::cleanup(&mut ui, finished, &mut outcome);
    assert!(outcome.unwrap_err().to_string().contains("was not saved"));
    assert_eq!(ui.recovery.records.len(), 1);
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        (source.as_str(), 4)
    );
    assert!(!fixture.root.join("joined.json").exists());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn authoritative_nonacceptance_returns_only_to_original_session_and_never_submits() {
    let mut fixture = Fixture::new();
    let mut input = unresolved(&mut fixture).await;
    // The public receipt is driven by a real Core owner, not a fabricated status
    // channel or production-only test hook. Dropping pending custody proves NA.
    let prepared = pending();
    input.receipt = prepared.receipt();
    let original = input.session;
    let mut ui = Frontend::default();
    let id = ui
        .recovery
        .input(input, Origin::capture(fixture.control.session()))
        .unwrap();
    ui.reconcile_recovery(original).unwrap();
    assert!(ui.recovery.blocks(original));
    drop(prepared);
    ui.reconcile_recovery(uuid::Uuid::now_v7()).unwrap();
    assert!(!ui.recovery.blocks(original));
    assert!(
        ui.editor.draft.is_empty(),
        "must not restore into another Session"
    );
    assert!(
        ui.recovery
            .inspect(id)
            .unwrap()
            .contains("Status: not accepted")
    );
    assert!(
        ui.recovery
            .list()
            .contains("not accepted; select original Session")
    );
    ui.reconcile_recovery(original).unwrap();
    assert_eq!((ui.editor.draft.as_str(), ui.editor.cursor), (LITERAL, 2));
    assert_eq!(ui.editor.attachments.len(), 1);
    assert!(ui.recovery.records.is_empty());
    assert!(!fixture.control.is_busy());
    let committed: i64 = rusqlite::Connection::open(fixture.control.session().file_path().unwrap())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(committed, 0);
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn nonacceptance_does_not_steal_a_picker_or_promote_input_to_its_empty_composer() {
    let mut fixture = Fixture::new();
    let mut input = unresolved(&mut fixture).await;
    let prepared = pending();
    input.receipt = prepared.receipt();
    let original = input.session;
    let mut ui = Frontend::default();
    ui.recovery
        .input(input, Origin::capture(fixture.control.session()))
        .unwrap();
    ui.pickers.open(
        crate::terminal_client::Picker::reasoning_for(
            fixture.control.agent(),
            ion_ai::Reasoning::default(),
        )
        .unwrap(),
    );
    assert_eq!(
        ui.input_target(),
        crate::terminal_client::interaction::InputTarget::Picker
    );
    ui.reconcile_recovery(original).unwrap();
    assert!(ui.recovery.blocks(original));
    drop(prepared);
    ui.reconcile_recovery(original).unwrap();
    assert_eq!(
        ui.input_target(),
        crate::terminal_client::interaction::InputTarget::Picker
    );
    assert!(ui.editor.draft.is_empty());
    assert!(ui.editor.attachments.is_empty());
    assert_eq!(ui.editor.recovered.len(), 1);
    let returned = &ui.editor.recovered[0];
    assert_eq!(
        (returned.source.literal.as_str(), returned.source.cursor),
        (LITERAL, 2)
    );
    assert_eq!(returned.attachments.len(), 1);
    assert!(ui.recovery.records.is_empty());
    assert!(
        !fixture.control.is_busy(),
        "reconciliation must not submit input"
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn discarded_nonaccepted_source_is_not_resurrected() {
    let mut fixture = Fixture::new();
    let mut input = unresolved(&mut fixture).await;
    let prepared = pending();
    input.receipt = prepared.receipt();
    let original = input.session;
    let mut ui = Frontend::default();
    let id = ui
        .recovery
        .input(input, Origin::capture(fixture.control.session()))
        .unwrap();
    ui.recovery.discard(id).unwrap();
    assert!(ui.recovery.blocks(original));
    drop(prepared);
    ui.reconcile_recovery(original).unwrap();
    assert!(!ui.recovery.blocks(original));
    assert!(ui.recovery.records.is_empty());
    assert!(ui.editor.draft.is_empty());
    assert!(ui.editor.recovered.is_empty());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn nonaccepted_return_respects_pending_clipboard_and_attachment_association() {
    let mut fixture = Fixture::new();
    let mut input = unresolved(&mut fixture).await;
    let prepared = pending();
    input.receipt = prepared.receipt();
    let original = input.session;
    let mut ui = Frontend::default();
    let stop = tokio_util::sync::CancellationToken::new();
    let task_stop = stop.clone();
    ui.clipboard_job = Some(crate::terminal_client::ClipboardJob {
        stop,
        task: tokio::spawn(async move {
            task_stop.cancelled().await;
            Ok(None)
        }),
    });
    ui.recovery
        .input(input, Origin::capture(fixture.control.session()))
        .unwrap();
    drop(prepared);
    ui.reconcile_recovery(original).unwrap();
    assert!(ui.editor.draft.is_empty());
    assert!(ui.clipboard_pending());
    assert!(ui.editor.attachments.is_empty());
    let returned = &ui.editor.recovered[0];
    assert_eq!(
        (returned.source.literal.as_str(), returned.source.cursor),
        (LITERAL, 2)
    );
    assert_eq!(returned.attachments.len(), 1);
    crate::terminal_client::cancel_clipboard_paste(&mut ui)
        .await
        .unwrap();
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn late_commit_uses_real_session_authority_and_never_restores_or_replays() {
    let mut fixture = Fixture::new();
    let mut input = unresolved(&mut fixture).await;
    // A faulted Store cannot be reused. Drive the positive receipt through a
    // separate healthy real Session, staging that producer's coordinates in the
    // public wrapper instead of exposing a production status mutation hook.
    let session = Arc::new(
        ion_core::Session::create(fixture.root.join("late.sqlite"), Some(&fixture.root)).unwrap(),
    );
    let prepared = pending();
    input.receipt = prepared.receipt();
    input.session = session.id();
    let original = session.id();
    let mut ui = Frontend::default();
    ui.recovery.input(input, Origin::capture(&session)).unwrap();
    ui.reconcile_recovery(original).unwrap();
    assert!(ui.recovery.blocks(original));
    let service = Arc::new(ScriptedModelService::new([ion_ai::Script::OpenError(
        ion_ai::ProviderError {
            kind: ion_ai::ProviderErrorKind::InvalidRequest,
            message: "synthetic no-response fixture".into(),
            retry_after_ms: None,
        },
    )]));
    let agent = Agent::with_tool_set(
        service.clone(),
        Arc::new(ToolSet::new([])),
        ModelRef {
            provider: "fixture".into(),
            model: "fixture".into(),
        },
    );
    agent
        .submit_prepared(
            session.clone(),
            prepared,
            String::new(),
            tokio_util::sync::CancellationToken::new(),
            None::<Arc<ion_core::SteeringInbox<()>>>,
            |_| {},
        )
        .await
        .unwrap_err();
    assert_eq!(
        service.requests().len(),
        1,
        "only the synthetic Core producer requests a response"
    );
    ui.recovery.records[0].origin.session = uuid::Uuid::now_v7();
    assert!(
        ui.reconcile_recovery(original).is_err(),
        "mismatched receipt must retain source and fail"
    );
    assert_eq!(ui.recovery.records.len(), 1);
    ui.recovery.records[0].origin.session = original;
    ui.reconcile_recovery(original).unwrap();
    assert!(!ui.recovery.blocks(original));
    assert!(ui.recovery.records.is_empty());
    assert!(ui.editor.draft.is_empty());
    assert!(ui.editor.recovered.is_empty());
    assert!(ui.session_io.blocked, "canonical refresh requested");
    let entries = session.view().unwrap().entries;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { .. }))
            .count(),
        1
    );
    ui.reconcile_recovery(original).unwrap();
    assert_eq!(session.view().unwrap().entries.len(), entries.len());
    session.shutdown().await.unwrap();
    fixture.control.shutdown().await;
}

#[test]
fn command_ids_are_never_reused_and_discard_claims_no_rollback() {
    let mut recovery = Recovery::default();
    let origin = Origin {
        session: uuid::Uuid::now_v7(),
        path: Some(PathBuf::from("/original.sqlite")),
    };
    let id = recovery.command(
        EditorDraft::at_end("/export original.html".into()),
        origin.clone(),
        "write failed: disk full".into(),
    );
    assert!(!recovery.blocks(origin.session));
    assert!(recovery.inspect(id).unwrap().contains("disk full"));
    recovery.discard(id).unwrap();
    assert!(recovery.inspect(id).is_err());
    let next = recovery.command(
        EditorDraft::at_end("/name second".into()),
        origin,
        "binding failed".into(),
    );
    assert!(next > id);
    let another = Recovery::default().command(
        EditorDraft::at_end("/new".into()),
        Origin {
            session: uuid::Uuid::now_v7(),
            path: Some(PathBuf::from("/other.sqlite")),
        },
        "unknown".into(),
    );
    assert!(another > next);
}

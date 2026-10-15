use super::*;
use crate::{ControlUpdate, Host, OperationOutcome, SavedSelection, Selection, Wire};
use std::fs;
use tokio_util::sync::CancellationToken;

fn route(model: &str) -> SavedSelection {
    SavedSelection {
        provider: "desktop".into(),
        model: model.into(),
        endpoint: Some("http://127.0.0.1:43129/v1/chat/completions".into()),
        wire: Some(Wire::ChatCompletions),
        api_key_env: None,
        image_input: false,
    }
}
fn control(host: Arc<Host>, session: Arc<CodingSession>, selected: Selection) -> SessionControl {
    crate::CodingApplication::new(host, None)
        .setup(session, selected, None)
        .unwrap()
        .into_control()
}

#[tokio::test]
async fn direct_shell_preflights_before_recovery_or_admission() {
    let root = std::env::temp_dir().join(format!("ion-control-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(Host::new(root.join("config"), root.join("state")));
    let selected = host.models().save_default(&route("one")).unwrap();
    let path = host.sessions(cwd.clone()).new_path().unwrap();
    let session = Arc::new(CodingSession::create(&path, Some(&cwd)).unwrap());
    let interrupted = session
        .run_user_shell(
            "old command".into(),
            false,
            CancellationToken::new(),
            |_, _| async { panic!("fixture interruption before native execution") },
        )
        .await;
    assert!(matches!(
        interrupted,
        Err(ion_core::CodingSessionError::Operation(
            ion_core::OperationError::TaskFailed(_)
        ))
    ));
    drop(session);
    let session = Arc::new(CodingSession::open(&path).unwrap());
    let mut control = control(host, session.clone(), selected);
    let before = session.view().unwrap();
    fs::rename(&cwd, root.join("moved-work")).unwrap();
    assert!(
        control
            .shell("touch must-not-dispatch".into(), false, ())
            .is_err()
    );
    assert_eq!(session.view().unwrap().entries, before.entries);
    assert!(!root.join("moved-work/must-not-dispatch").exists());
    fs::rename(root.join("moved-work"), &cwd).unwrap();
    control.shell("printf captured".into(), false, ()).unwrap();
    assert!(control.new_session().is_err());
    match control.next().await {
        ControlUpdate::Settled(settlement) => assert!(
            matches!(settlement.outcome, OperationOutcome::Shell(Ok(output)) if !output.is_error)
        ),
        _ => panic!("shell settlement required"),
    }
    control.new_session().unwrap();
    let original = session.view().unwrap();
    assert!(
        matches!(original.entries.last(), Some(ion_core::SessionEntry::UserShellSettled { outcome: ion_core::UserShellOutcome::Observed { output, .. }, .. }) if output["stdout"] == "captured")
    );
    assert!(
        !control
            .session()
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry, ion_core::SessionEntry::UserShellAdmitted { .. }))
    );
    drop(control);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clone_preserves_resolved_route_without_a_store_entry() {
    let root = std::env::temp_dir().join(format!("ion-clone-route-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(Host::new(root.join("config"), root.join("state")));
    let selected = host.models().save_default(&route("resolved-only")).unwrap();
    fs::remove_file(root.join("config/models.json")).unwrap();
    let session = Arc::new(
        CodingSession::create(host.sessions(cwd.clone()).new_path().unwrap(), Some(&cwd)).unwrap(),
    );
    session.select_model(selected.identity()).unwrap();
    let identity = selected.identity();
    let mut control = control(host, session, selected);
    control
        .clone_session()
        .expect("a conversation copy retains the resolved route");
    assert_eq!(control.agent().model(), &identity);
    assert_eq!(
        control.session().model_preference().unwrap(),
        Some(identity)
    );
    drop(control);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn idle_override_and_effort_restore_as_one_selection() {
    let root = std::env::temp_dir().join(format!("ion-control-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(Host::new(root.join("config"), root.join("state")));
    let mut limited = route("limited");
    limited.wire = Some(Wire::LlamaCppNoThinking);
    let limited = host.models().save_default(&limited).unwrap();
    let selected = host.models().save_default(&route("override")).unwrap();
    let path = host.sessions(cwd.clone()).new_path().unwrap();
    let session = Arc::new(CodingSession::create(&path, Some(&cwd)).unwrap());
    session.select_model(limited.identity()).unwrap();
    let binding = control(host.clone(), session.clone(), selected);
    binding.select_reasoning(ion_ai::Reasoning::High).unwrap();
    drop(binding);
    drop(session);
    let session = Arc::new(CodingSession::open(path).unwrap());
    let selected = host
        .models()
        .choose(
            None,
            None,
            session.view().unwrap().last_model,
            host.credentials(),
        )
        .unwrap();
    let binding = control(host.clone(), session, selected);
    assert_eq!(binding.agent().model().model, "override");
    assert_eq!(
        binding.session().reasoning().unwrap(),
        ion_ai::Reasoning::High
    );
    drop(binding);
    drop(host);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn replacement_restores_model_and_keeps_old_binding_on_preflight_failure() {
    let root = std::env::temp_dir().join(format!("ion-control-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(Host::new(root.join("config"), root.join("state")));
    let first_model = host.models().save_default(&route("one")).unwrap();
    let first_path = host.sessions(cwd.clone()).new_path().unwrap();
    let first = Arc::new(CodingSession::create(&first_path, Some(&cwd)).unwrap());
    first.select_model(first_model.identity()).unwrap();
    let mut binding = control(host.clone(), first, first_model);
    binding.select_reasoning(ion_ai::Reasoning::High).unwrap();
    let entries = binding.session().view().unwrap().entries;
    assert!(
        binding
            .select_reasoning(ion_ai::Reasoning::BudgetTokens(128))
            .is_err()
    );
    assert_eq!(binding.session().view().unwrap().entries, entries);
    let mut limited = route("limited");
    limited.wire = Some(Wire::LlamaCppNoThinking);
    host.models().save_default(&limited).unwrap();
    assert!(
        binding
            .select_model(ion_ai::ModelRef {
                provider: "desktop".into(),
                model: "limited".into()
            })
            .is_err()
    );
    assert_eq!(binding.agent().model().model, "one");
    assert_eq!(binding.session().view().unwrap().entries, entries);
    host.models().save_default(&route("two")).unwrap();
    binding.new_session().unwrap();
    let second_id = binding.session_id();
    assert_eq!(binding.agent().model().model, "two");
    assert_eq!(
        binding.session().reasoning().unwrap(),
        ion_ai::Reasoning::ProviderDefault
    );
    let moved = root.join("moved-work");
    fs::rename(&cwd, &moved).unwrap();
    assert!(binding.switch_session(first_path.clone()).is_err());
    assert_eq!(binding.session_id(), second_id);
    assert_eq!(binding.agent().model().model, "two");
    fs::rename(&moved, &cwd).unwrap();
    binding.switch_session(first_path).unwrap();
    assert_eq!(binding.agent().model().model, "one");
    assert_eq!(
        binding.session().reasoning().unwrap(),
        ion_ai::Reasoning::High
    );
    assert_ne!(binding.session_id(), second_id);
    fs::create_dir_all(cwd.join(".ion/prompts")).unwrap();
    fs::write(cwd.join(".ion/prompts/hello.md"), "Hello $1").unwrap();
    binding.reload_resources().unwrap();
    assert!(
        binding
            .resources()
            .templates()
            .any(|item| item.name == "hello")
    );
    drop(binding);
    drop(host);
    fs::remove_dir_all(root).unwrap();
}

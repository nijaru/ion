//! Public shared-control boundary: restricted assembly, replacement and refusal.
use std::{
    fs,
    path::Path,
    sync::{Arc, RwLock},
};

use anyhow::{Context, Result, ensure};
use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, Usage,
};
use ion_core::{Agent, AgentLimits, ForkPoint, Session, ToolSet};
use serde_json::json;

use crate::{
    Application, ApplicationRuntime, CommandCatalog, ControlUpdate, Delivery, OperationOutcome,
    Resources, SessionCatalog, SessionSetup, UserInput, attachments::AttachmentSource,
};

struct Restricted {
    service: Arc<ScriptedModelService>,
    instructions: RwLock<Option<String>>,
    commands: Option<CommandCatalog>,
}
fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "restricted".into(),
        model: name.into(),
    }
}
fn response(content: Vec<Content>) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}
impl Application for Restricted {
    fn commands(&self, cwd: Option<&Path>) -> Result<CommandCatalog> {
        ensure!(cwd.is_none(), "this application has no workspace");
        Ok(self.commands.clone().unwrap_or_default())
    }
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        ensure!(cwd.is_none(), "this application has no workspace");
        Ok(Resources::new(
            self.instructions
                .read()
                .unwrap()
                .clone()
                .context("instructions unavailable")?,
        ))
    }
    fn runtime(
        &self,
        cwd: Option<&Path>,
        selected: Option<&ModelRef>,
        _questions: Option<Arc<crate::QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        ensure!(cwd.is_none(), "this application has no workspace");
        let selected = selected.cloned().unwrap_or_else(|| model("first"));
        ensure!(selected != model("missing"), "model unavailable");
        // A faulty factory must not silently change requested identity.
        let selected = if selected == model("misbound") {
            model("first")
        } else {
            selected
        };
        Ok(ApplicationRuntime::new(Arc::new(
            Agent::with_tool_set(self.service.clone(), Arc::new(ToolSet::new([])), selected)
                .with_limits(AgentLimits {
                    image_input: true,
                    ..AgentLimits::default()
                }),
        )))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn owned_close_releases_writer_without_stopping_a_sibling_or_losing_history() {
    use crate::{SessionOptions, SessionTarget};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let root = std::env::temp_dir().join(format!("ion-owned-close-{}", uuid::Uuid::now_v7()));
    let catalog = SessionCatalog::new(root.clone(), None);
    let application = Arc::new(Restricted {
        service: Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first".into())]),
            response(vec![Content::Text("sibling".into())]),
        ])),
        instructions: RwLock::new(Some("Explicit instructions".into())),
        commands: None,
    });
    let open = |target, model| {
        SessionSetup::open(
            target,
            catalog.clone(),
            application.clone(),
            model,
            SessionOptions::default(),
            CancellationToken::new(),
        )
    };
    let path = catalog.new_path().unwrap();
    assert!(
        open(SessionTarget::Create(path.clone()), Some(model("missing")))
            .join()
            .await
            .is_err()
    );
    let id = Session::inspect_id(&path).unwrap();
    let target = |id| SessionTarget::Open {
        path: path.clone(),
        expected_id: Some(id),
    };
    assert!(
        target(uuid::Uuid::new_v4())
            .prepare(None, CancellationToken::new())
            .join()
            .await
            .is_err()
    );
    let acquired = target(id)
        .prepare(None, CancellationToken::new())
        .join()
        .await
        .unwrap();
    assert!(matches!(
        Session::open(&path),
        Err(ion_core::SessionError::AlreadyOpen)
    ));
    let mut first = open(SessionTarget::Acquired(acquired.clone()), None)
        .join()
        .await
        .unwrap()
        .into_control::<()>();
    assert!(Arc::ptr_eq(&acquired, first.session()));
    drop(acquired);
    let mut sibling = open(SessionTarget::Create(catalog.new_path().unwrap()), None)
        .join()
        .await
        .unwrap()
        .into_control::<()>();
    let key = ion_core::RequestKey::new("scope".into(), "first".into()).unwrap();
    let input = Message::user_input("first".into(), []);
    first.submit_request(&key, input, ()).await.unwrap();
    loop {
        match first.next().await {
            ControlUpdate::Settled(settlement) => {
                assert!(
                    matches!(settlement.outcome, OperationOutcome::Turn(Ok(answer)) if answer == "first")
                );
                break;
            }
            ControlUpdate::Idle => panic!("first request never settled"),
            _ => {}
        }
    }
    let history = first.session().history().unwrap();
    let live = first.session().clone();
    let mut close = first.begin_close();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), close.join())
            .await
            .is_err()
    );
    drop(live);
    let report = tokio::time::timeout(Duration::from_secs(5), close.join())
        .await
        .unwrap()
        .unwrap();
    assert!(report.cleanup_errors.is_empty());
    let reopened = Session::open(path).unwrap();
    assert_eq!(
        history.request_receipt(&key).await.unwrap().unwrap().end,
        Some(ion_core::TurnEndReason::Completed)
    );
    let sibling_key = ion_core::RequestKey::new("scope".into(), "sibling".into()).unwrap();
    sibling
        .submit_request(&sibling_key, Message::user_input("sibling".into(), []), ())
        .await
        .unwrap();
    loop {
        match sibling.next().await {
            ControlUpdate::Settled(settlement) => {
                assert!(
                    matches!(settlement.outcome, OperationOutcome::Turn(Ok(answer)) if answer == "sibling")
                );
                break;
            }
            ControlUpdate::Idle => panic!("sibling never settled"),
            _ => {}
        }
    }
    assert!(
        sibling
            .begin_close()
            .join()
            .await
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn identified_admission_requires_execution_custody_before_commit() {
    let root = std::env::temp_dir().join(format!("ion-request-runtime-{}", uuid::Uuid::now_v7()));
    let catalog = SessionCatalog::new(root.clone(), None);
    let session = Arc::new(Session::create(catalog.new_path().unwrap(), None).unwrap());
    let application = Arc::new(Restricted {
        service: Arc::new(ScriptedModelService::new([])),
        instructions: RwLock::new(Some("Explicit instructions".into())),
        commands: None,
    });
    let mut control = SessionSetup::new(session.clone(), catalog, application, None, None, None)
        .unwrap()
        .into_control::<String>();
    let key = ion_core::RequestKey::new("inbox".into(), "not-accepted".into()).unwrap();
    let input = Message::user_input("original".into(), []);
    let rejected = futures_util::FutureExt::now_or_never(control.submit_request(
        &key,
        input.clone(),
        "metadata".into(),
    ))
    .expect("runtime preflight completes without an executor")
    .err()
    .unwrap();
    assert_eq!(rejected.metadata, "metadata");
    assert!(matches!(rejected.input, UserInput::Message(found) if found == input));
    assert!(rejected.reason.to_string().contains("Tokio runtime"));
    assert!(session.request_receipt(&key).unwrap().is_none());
    assert!(session.view().unwrap().entries.is_empty());
    assert!(!control.is_busy());
    drop(control);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn workspace_free_application_keeps_its_authority_across_shared_lifecycle() {
    let root = std::env::temp_dir().join(format!("ion-restricted-{}", uuid::Uuid::now_v7()));
    let catalog = SessionCatalog::new(root.join("state"), None);
    let path = catalog.new_path().unwrap();
    let must_not_exist = root.join("must-not-execute");
    let service = Arc::new(ScriptedModelService::new([
        response(vec![Content::ToolCall(ToolCall {
            id: "attempt".into(),
            name: "exec".into(),
            arguments: json!({"command":format!("touch {}", must_not_exist.display())}),
            raw_arguments: None,
        })]),
        response(vec![Content::Text("restricted answer".into())]),
    ]));
    let application = Arc::new(Restricted {
        service: service.clone(),
        instructions: RwLock::new(Some("Only this application's instructions.".into())),
        commands: None,
    });
    let session = Arc::new(Session::create(&path, None).unwrap());
    let mismatched = SessionSetup::new(
        session.clone(),
        SessionCatalog::new(root.join("wrong-scope"), Some(root.clone())),
        application.clone(),
        None,
        Some(ion_ai::Reasoning::ProviderDefault),
        None,
    );
    assert!(mismatched.is_err());
    assert!(session.view().unwrap().entries.is_empty());
    let mut control = SessionSetup::new(session, catalog, application.clone(), None, None, None)
        .unwrap()
        .into_control::<String>();
    let refused = control
        .shell("touch not-authorized".into(), false, "literal shell".into())
        .unwrap_err();
    assert_eq!(refused.0, "literal shell");
    assert!(control.session().view().unwrap().entries.is_empty());
    for source in [
        AttachmentSource::ImageFile(root.join("must-not-read.png")),
        AttachmentSource::File(root.join("must-not-read.txt")),
    ] {
        let input = UserInput::Prompt {
            text: "retain original".into(),
            attachments: vec![source.clone()],
        };
        let refused = control
            .submit(Delivery::Start, input, "literal attachment".into())
            .unwrap_err();
        assert_eq!(refused.metadata, "literal attachment");
        assert!(
            refused
                .reason
                .to_string()
                .contains("file attachments are not enabled")
        );
        assert!(
            matches!(refused.input, UserInput::Prompt { text, attachments }
            if text == "retain original" && serde_json::to_value(&attachments).unwrap() == serde_json::to_value([source]).unwrap())
        );
        assert!(!control.is_busy());
    }
    let image = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
    control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: "/unknown is literal".into(),
                attachments: vec![
                    AttachmentSource::image_bytes({
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD
                            .decode(image.content.data())
                            .unwrap()
                    }),
                    image.into(),
                    AttachmentSource::Text(crate::attachments::TextFile {
                        name: "memory.md".into(),
                        text: "/literal file data".into(),
                    }),
                ],
            },
            "accepted literal".into(),
        )
        .unwrap();
    loop {
        match control.next().await {
            ControlUpdate::Settled(settlement) => {
                assert!(matches!(settlement.outcome, OperationOutcome::Turn(Ok(_))));
                assert_eq!(settlement.accepted[0].metadata, "accepted literal");
            }
            ControlUpdate::Returned(input) => panic!("unexpected return: {:?}", input.reason),
            ControlUpdate::Idle => break,
            _ => {}
        }
    }
    assert!(!must_not_exist.exists());
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request.tools.is_empty());
        assert_eq!(
            request.instructions.as_deref(),
            Some("Only this application's instructions.")
        );
    }
    assert!(
        control
            .session()
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(
                entry,
                ion_core::SessionEntry::ToolResult {
                    result: ion_core::ToolSettlement {
                        outcome: ion_core::ToolOutcome::NotDispatched { .. },
                        ..
                    },
                    ..
                }
            ))
    );
    control.select_model(model("second")).await.unwrap();
    let original_entries = control.session().view().unwrap().entries;
    for name in ["missing", "misbound"] {
        assert!(control.select_model(model(name)).await.is_err());
        assert_eq!(control.agent().model(), &model("second"));
        assert_eq!(control.session().view().unwrap().entries, original_entries);
    }
    *application.instructions.write().unwrap() = Some("Reloaded application instructions.".into());
    control.reload_resources().await.unwrap();
    let cloned = control.clone_session().await.unwrap();
    let clone_path = control.session().file_path().unwrap().to_owned();
    assert_eq!(control.session_id(), cloned);
    assert_eq!(control.agent().model(), &model("second"));
    control.new_session().await.unwrap();
    assert_eq!(control.agent().model(), &model("first"));
    control.switch_session(clone_path).await.unwrap();
    assert_eq!(control.agent().model(), &model("second"));
    control
        .fork_session(ForkPoint::BeforeTurn(1))
        .await
        .unwrap();
    assert_eq!(control.agent().model(), &model("first"));
    assert_eq!(control.session().cwd(), None);
    assert!(control.agent().tool_catalog().declared_specs().is_empty());
    assert_eq!(control.instructions(), "Reloaded application instructions.");
    let before = control.session_id();
    *application.instructions.write().unwrap() = None;
    assert!(control.reload_resources().await.is_err());
    assert!(control.new_session().await.is_err());
    assert_eq!(control.session_id(), before);
    assert_eq!(control.instructions(), "Reloaded application instructions.");
    control.select_model(model("second")).await.unwrap();
    let report = control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    let path = control.session().file_path().unwrap().to_owned();
    let retirements = control.retirements();
    drop(control);
    retirements.join().await.unwrap();
    let reopened = Arc::new(Session::open(&path).unwrap());
    assert_eq!(reopened.cwd(), None);
    assert_eq!(Session::inspect_metadata(&path).unwrap().cwd, None);
    *application.instructions.write().unwrap() = Some("Reloaded application instructions.".into());
    let reopened = SessionSetup::new(
        reopened,
        SessionCatalog::new(root.join("state"), None),
        application,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(reopened.agent().model(), &model("second"));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn configured_command_is_rejected_when_the_application_has_no_user_shell_authority() {
    let root = std::env::temp_dir().join(format!("ion-command-shell-{}", uuid::Uuid::now_v7()));
    let config = root.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("commands.json"),
        r#"{"commands":{"check":{"description":"Run tests","program":"true","args":[],"share_with_model":false}}}"#,
    )
    .unwrap();
    let commands = CommandCatalog::load(&root, &config).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), None).unwrap());
    let application = Arc::new(Restricted {
        service: Arc::new(ScriptedModelService::new([])),
        instructions: RwLock::new(Some("Explicit instructions".into())),
        commands: Some(commands),
    });
    let mut control = SessionSetup::new(
        session,
        SessionCatalog::new(root.join("state"), None),
        application,
        None,
        None,
        None,
    )
    .unwrap()
    .into_control::<String>();
    let (metadata, error) = control
        .command("check", &[], "literal draft".into())
        .unwrap_err();
    assert_eq!(metadata, "literal draft");
    assert!(
        error.to_string().contains("direct shell is not enabled"),
        "{error:#}"
    );
    assert!(!control.is_busy());
    drop(control);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn coding_application_selection_and_read_only_narrow_the_model_loadout() {
    let root = std::env::temp_dir().join(format!("ion-selection-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(crate::Host::new(root.join("config"), root.join("state")));
    let selected = host
        .models()
        .save_default(&crate::SavedSelection {
            provider: "offline".into(),
            model: "selection-model".into(),
            endpoint: Some("http://127.0.0.1:1/v1/chat/completions".into()),
            wire: Some(crate::Wire::ChatCompletions),
            api_key_env: None,
            context_window_tokens: None,
            max_output_tokens: None,
            image_input: false,
        })
        .unwrap();

    for names in [vec!["read", "write"], Vec::new()] {
        let session = Arc::new(
            Session::create(
                host.sessions(Some(cwd.clone())).new_path().unwrap(),
                Some(&cwd),
            )
            .unwrap(),
        );
        let application = crate::CodingApplication::new(host.clone(), None).with_tool_selection(
            ion_core::ToolSelection::include(names.iter().map(|name| (*name).into())),
        );
        let control = application
            .setup(
                session.clone(),
                selected.clone(),
                None,
                None,
                host.resources(&cwd).unwrap(),
            )
            .unwrap()
            .into_control::<()>();
        let catalog = control.agent().tool_catalog();
        assert_eq!(catalog.specs().len(), names.len());
        for name in [
            "read", "write", "edit", "exec", "process", "subagent", "ls", "find", "grep",
        ] {
            assert_eq!(catalog.is_declared(name), names.contains(&name), "{name}");
        }
        drop(control);
        drop(session);
    }

    fs::create_dir_all(host.config_root()).unwrap();
    fs::write(
        host.config_root().join("commands.json"),
        r#"{"commands":{"check":{"description":"explicit user command","program":"true","args":[],"share_with_model":false}}}"#,
    )
    .unwrap();
    let session = Arc::new(
        Session::create(
            host.sessions(Some(cwd.clone())).new_path().unwrap(),
            Some(&cwd),
        )
        .unwrap(),
    );
    let application = crate::CodingApplication::new(host.clone(), None).with_read_only_tools();
    let control = application
        .setup(
            session.clone(),
            selected.clone(),
            None,
            None,
            host.resources(&cwd).unwrap(),
        )
        .unwrap()
        .into_control::<()>();
    assert_eq!(control.commands().iter().count(), 1);
    let catalog = control.agent().tool_catalog();
    assert!(catalog.is_declared("read"));
    assert!(catalog.is_declared("ls"));
    assert!(catalog.is_declared("find"));
    assert!(catalog.is_declared("grep"));
    assert!(catalog.is_declared("history"));
    for excluded in ["edit", "write", "exec", "process", "subagent"] {
        assert!(!catalog.is_declared(excluded), "{excluded}");
    }
    drop(control);
    drop(session);

    let session = Arc::new(
        Session::create(
            host.sessions(Some(cwd.clone())).new_path().unwrap(),
            Some(&cwd),
        )
        .unwrap(),
    );
    let application = crate::CodingApplication::new(host.clone(), None)
        .with_code_mode(true)
        .with_read_only_tools();
    let control = application
        .setup(
            session.clone(),
            selected.clone(),
            None,
            None,
            host.resources(&cwd).unwrap(),
        )
        .unwrap()
        .into_control::<()>();
    let catalog = control.agent().tool_catalog();
    for included in ["code_mode", "read", "ls", "find", "grep", "history"] {
        assert!(catalog.is_declared(included), "{included}");
    }
    for excluded in ["edit", "write", "exec", "process", "subagent"] {
        assert!(!catalog.is_declared(excluded), "{excluded}");
    }
    drop(control);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn last_selection_builder_wins_for_restricted_roles() {
    let root = std::env::temp_dir().join(format!("ion-selection-order-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(crate::Host::new(root.join("config"), root.join("state")));
    let selected = host
        .models()
        .save_default(&crate::SavedSelection {
            provider: "offline".into(),
            model: "selection-order".into(),
            endpoint: Some("http://127.0.0.1:1/v1/chat/completions".into()),
            wire: Some(crate::Wire::ChatCompletions),
            api_key_env: None,
            context_window_tokens: None,
            max_output_tokens: None,
            image_input: false,
        })
        .unwrap();
    let catalog = host.sessions(Some(cwd.clone()));
    let session = Arc::new(Session::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let explicit = ion_core::ToolSelection::include(["write".into()]);
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_read_only_tools()
        .with_tool_selection(explicit.clone())
        .setup(
            session,
            selected.clone(),
            None,
            None,
            host.resources(&cwd).unwrap(),
        )
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert_eq!(tools.specs().len(), 1);
    assert!(tools.is_declared("write"));
    assert!(!tools.is_declared("read"));
    drop(control);

    let session = Arc::new(Session::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_tool_selection(explicit)
        .with_read_only_tools()
        .setup(session, selected, None, None, host.resources(&cwd).unwrap())
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert!(tools.is_declared("read"));
    assert!(!tools.is_declared("write"));
    assert!(!tools.is_declared("ask_user_question"));
    drop(control);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn question_capability_obeys_explicit_selection_and_read_only_policy() {
    let root = std::env::temp_dir().join(format!("ion-question-tools-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let host = Arc::new(crate::Host::new(root.join("config"), root.join("state")));
    let selected = host
        .models()
        .save_default(&crate::SavedSelection {
            provider: "offline".into(),
            model: "question-tools".into(),
            endpoint: Some("http://127.0.0.1:1/v1/chat/completions".into()),
            wire: Some(crate::Wire::ChatCompletions),
            api_key_env: None,
            context_window_tokens: None,
            max_output_tokens: None,
            image_input: false,
        })
        .unwrap();
    let catalog = host.sessions(Some(cwd.clone()));
    let session = Arc::new(Session::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_code_mode(true)
        .with_tool_selection(ion_core::ToolSelection::include([
            "read".into(),
            "code_mode".into(),
        ]))
        .setup(
            session,
            selected.clone(),
            None,
            Some(crate::QuestionCustody::new()),
            host.resources(&cwd).unwrap(),
        )
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert!(tools.is_declared("read"));
    assert!(tools.is_declared("code_mode"));
    assert!(!tools.is_declared("ask_user_question"));
    drop(control);

    let session = Arc::new(Session::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_read_only_tools()
        .setup(
            session,
            selected.clone(),
            None,
            Some(crate::QuestionCustody::new()),
            host.resources(&cwd).unwrap(),
        )
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert!(tools.is_declared("read"));
    assert!(tools.is_declared("ask_user_question"));
    drop(control);

    let session = Arc::new(Session::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_read_only_tools()
        .setup(session, selected, None, None, host.resources(&cwd).unwrap())
        .unwrap()
        .into_control::<()>();
    assert!(
        !control
            .agent()
            .tool_catalog()
            .is_declared("ask_user_question")
    );
    drop(control);
    fs::remove_dir_all(root).unwrap();
}

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
use ion_core::{AgentLimits, CodingAgent, CodingSession, ForkPoint, ToolSet};
use serde_json::json;

use crate::{
    Application, ApplicationRuntime, CommandCatalog, ControlUpdate, Delivery, OperationOutcome,
    Resources, SessionCatalog, SessionSetup, UserInput, image_input::ImageSource,
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
            CodingAgent::with_tool_set(self.service.clone(), Arc::new(ToolSet::new([])), selected)
                .with_limits(AgentLimits {
                    image_input: true,
                    ..AgentLimits::default()
                }),
        )))
    }
}

#[test]
fn identified_admission_requires_execution_custody_before_commit() {
    let root = std::env::temp_dir().join(format!("ion-request-runtime-{}", uuid::Uuid::now_v7()));
    let catalog = SessionCatalog::new(root.clone(), None);
    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), None).unwrap());
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
    let rejected = control
        .submit_request(&key, input.clone(), "metadata".into())
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
    let session = Arc::new(CodingSession::create(&path, None).unwrap());
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
    let input = UserInput::Prompt {
        text: "retain original".into(),
        images: vec![ImageSource::Path(root.join("must-not-read.png"))],
    };
    let refused = control
        .submit(Delivery::Start, input, "literal image".into())
        .unwrap_err();
    assert_eq!(refused.metadata, "literal image");
    assert!(
        matches!(refused.input, UserInput::Prompt { text, images } if text == "retain original" && matches!(&images[..], [ImageSource::Path(path)] if path == &root.join("must-not-read.png")))
    );
    assert!(!control.is_busy());
    let image = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
    control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: "/unknown is literal".into(),
                images: vec![image.into()],
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
    control.select_model(model("second")).unwrap();
    let original_entries = control.session().view().unwrap().entries;
    for name in ["missing", "misbound"] {
        assert!(control.select_model(model(name)).is_err());
        assert_eq!(control.agent().model(), &model("second"));
        assert_eq!(control.session().view().unwrap().entries, original_entries);
    }
    *application.instructions.write().unwrap() = Some("Reloaded application instructions.".into());
    control.reload_resources().unwrap();
    let cloned = control.clone_session().unwrap();
    let clone_path = control.session().path().to_owned();
    assert_eq!(control.session_id(), cloned);
    assert_eq!(control.agent().model(), &model("second"));
    control.new_session().unwrap();
    assert_eq!(control.agent().model(), &model("first"));
    control.switch_session(clone_path).unwrap();
    assert_eq!(control.agent().model(), &model("second"));
    control.fork_session(ForkPoint::BeforeTurn(1)).unwrap();
    assert_eq!(control.agent().model(), &model("first"));
    assert_eq!(control.session().cwd(), None);
    assert!(control.agent().tool_catalog().declared_specs().is_empty());
    assert_eq!(control.instructions(), "Reloaded application instructions.");
    let before = control.session_id();
    *application.instructions.write().unwrap() = None;
    assert!(control.reload_resources().is_err());
    assert!(control.new_session().is_err());
    assert_eq!(control.session_id(), before);
    assert_eq!(control.instructions(), "Reloaded application instructions.");
    control.select_model(model("second")).unwrap();
    let report = control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    let path = control.session().path().to_owned();
    drop(control);
    let reopened = Arc::new(CodingSession::open(&path).unwrap());
    assert_eq!(reopened.cwd(), None);
    assert_eq!(CodingSession::inspect_metadata(&path).unwrap().cwd, None);
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
    let session = Arc::new(CodingSession::create(root.join("session.sqlite"), None).unwrap());
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
            image_input: false,
        })
        .unwrap();

    for names in [vec!["read", "write"], Vec::new()] {
        let session = Arc::new(
            CodingSession::create(
                host.sessions(Some(cwd.clone())).new_path().unwrap(),
                Some(&cwd),
            )
            .unwrap(),
        );
        let application = crate::CodingApplication::new(host.clone(), None).with_tool_selection(
            ion_core::ToolSelection::include(names.iter().map(|name| (*name).into())),
        );
        let control = application
            .setup(session.clone(), selected.clone(), None, None)
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
        CodingSession::create(
            host.sessions(Some(cwd.clone())).new_path().unwrap(),
            Some(&cwd),
        )
        .unwrap(),
    );
    let application = crate::CodingApplication::new(host.clone(), None).with_read_only_tools();
    let control = application
        .setup(session.clone(), selected.clone(), None, None)
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
        CodingSession::create(
            host.sessions(Some(cwd.clone())).new_path().unwrap(),
            Some(&cwd),
        )
        .unwrap(),
    );
    let application = crate::CodingApplication::new(host.clone(), None)
        .with_code_mode(true)
        .with_read_only_tools();
    let control = application
        .setup(session.clone(), selected.clone(), None, None)
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
            image_input: false,
        })
        .unwrap();
    let catalog = host.sessions(Some(cwd.clone()));
    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let explicit = ion_core::ToolSelection::include(["write".into()]);
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_read_only_tools()
        .with_tool_selection(explicit.clone())
        .setup(session, selected.clone(), None, None)
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert_eq!(tools.specs().len(), 1);
    assert!(tools.is_declared("write"));
    assert!(!tools.is_declared("read"));
    drop(control);

    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host, None)
        .with_tool_selection(explicit)
        .with_read_only_tools()
        .setup(session, selected, None, None)
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
            image_input: false,
        })
        .unwrap();
    let catalog = host.sessions(Some(cwd.clone()));
    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
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
        )
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert!(tools.is_declared("read"));
    assert!(tools.is_declared("code_mode"));
    assert!(!tools.is_declared("ask_user_question"));
    drop(control);

    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host.clone(), None)
        .with_read_only_tools()
        .setup(
            session,
            selected.clone(),
            None,
            Some(crate::QuestionCustody::new()),
        )
        .unwrap()
        .into_control::<()>();
    let tools = control.agent().tool_catalog();
    assert!(tools.is_declared("read"));
    assert!(tools.is_declared("ask_user_question"));
    drop(control);

    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let control = crate::CodingApplication::new(host, None)
        .with_read_only_tools()
        .setup(session, selected, None, None)
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

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
    Application, ApplicationRuntime, ControlUpdate, Delivery, OperationOutcome, Resources,
    SessionCatalog, SessionSetup, UserInput, image_input::ImageSource,
};

struct Restricted {
    service: Arc<ScriptedModelService>,
    instructions: RwLock<Option<String>>,
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
    });
    let session = Arc::new(CodingSession::create(&path, None).unwrap());
    let mismatched = SessionSetup::new(
        session.clone(),
        SessionCatalog::new(root.join("wrong-scope"), Some(root.clone())),
        application.clone(),
        None,
        Some(ion_ai::Reasoning::ProviderDefault),
    );
    assert!(mismatched.is_err());
    assert!(session.view().unwrap().entries.is_empty());
    let mut control = SessionSetup::new(session, catalog, application.clone(), None, None)
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
    )
    .unwrap();
    assert_eq!(reopened.agent().model(), &model("second"));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

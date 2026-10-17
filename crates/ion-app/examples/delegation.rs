//! Credential-free foreground/background delegation through the shared engine.
//! Run with a fresh absolute state directory. The smoke also kills this process
//! after a synced child effect, then exercises both explicit recovery policies.
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, Reasoning,
    ResponseTermination, Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    AuthorityScope, CodingAgent, CodingSession, CodingToolOutput, CodingToolSource,
    DelegationOutcome, ToolDefinition, ToolExecutor, ToolInvocation, ToolRegistration, ToolSet,
    TurnEndReason,
};
use ion_host::{
    Application, ApplicationRuntime, BackgroundOwner, ControlUpdate, OperationOutcome,
    RequestAdmission, Resources, SessionCatalog, SessionDelegation, SessionSetup,
};
use serde_json::json;

struct App {
    agent: Arc<CodingAgent>,
    instructions: &'static str,
}
impl Application for App {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        ensure!(cwd.is_none(), "workspace-free application");
        Ok(Resources::new(self.instructions.into()))
    }
    fn runtime(&self, _: Option<&Path>, model: Option<&ModelRef>) -> Result<ApplicationRuntime> {
        ensure!(
            model.is_none_or(|model| model == self.agent.model()),
            "unexpected model"
        );
        Ok(ApplicationRuntime::new(self.agent.clone()))
    }
}
struct Effect {
    parent: Arc<CodingSession>,
    file: PathBuf,
    pause: bool,
    release: Option<Arc<tokio::sync::Notify>>,
    synced: Arc<tokio::sync::Notify>,
}
impl CodingToolSource for Effect {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(definition("effect"), self)]
    }
}
impl ToolExecutor for Effect {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let links = self.parent.view().expect("parent facts").delegations();
            let link = links.first().expect("intent before child effect");
            assert_eq!(
                invocation.origin().expect("admitted child").session(),
                link.record.child
            );
            assert!(link.outcome.is_none());
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file)
                .expect("owned effect file");
            file.write_all(b"child effect\n").expect("effect write");
            file.sync_all().expect("effect sync");
            self.synced.notify_one();
            if self.pause {
                println!("CHILD_EFFECT_SYNCED");
                std::io::stdout().flush().expect("marker flush");
                invocation.stop().cancelled().await;
            } else if let Some(release) = &self.release {
                tokio::select! {
                    () = release.notified() => {},
                    () = invocation.stop().cancelled() => {},
                }
            }
            output(json!({"written": true}), false)
        })
    }
}
struct Delegate {
    child: SessionDelegation,
    owner: Option<Arc<BackgroundOwner>>,
}
impl CodingToolSource for Delegate {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(definition("delegate"), self)]
    }
}
impl ToolExecutor for Delegate {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let task = match self
                .child
                .prepare(Message::user_input("perform the child effect".into(), []))
            {
                Ok(task) => task,
                Err(error) => return output(json!(error.to_string()), true),
            };
            let task = if let Some(owner) = &self.owner {
                task.in_background(owner.destination())
            } else {
                task
            };
            match invocation.delegate(task).await {
                Ok(receipt) => {
                    let failed = !receipt.transferred
                        && !matches!(&receipt.outcome, Some(DelegationOutcome::Joined { receipt: Some(receipt), error: None, answer: Some(_) }) if receipt.end == Some(TurnEndReason::Completed));
                    output(
                        serde_json::to_value(receipt).expect("receipt serialization"),
                        failed,
                    )
                }
                Err(error) => output(json!(error), true),
            }
        })
    }
}
fn definition(name: &str) -> ToolDefinition {
    ToolDefinition::external(ToolSpec {
        name: name.into(),
        description: name.into(),
        input_schema: json!({"type":"object","additionalProperties":false}),
    })
}
fn output(value: serde_json::Value, is_error: bool) -> CodingToolOutput {
    CodingToolOutput {
        value,
        images: Vec::new(),
        is_error,
    }
}
fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "synthetic".into(),
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
fn call(name: &str) -> Content {
    Content::ToolCall(ToolCall {
        id: "call".into(),
        name: name.into(),
        arguments: json!({}),
        raw_arguments: None,
    })
}

async fn drive(control: &mut ion_host::SessionControl) -> Result<()> {
    while control.is_busy() {
        control.maintain().await;
    }
    let mut settled = false;
    while let Some(update) = control.take_update() {
        match update {
            ControlUpdate::Settled(settlement) => {
                match settlement.outcome {
                    OperationOutcome::Turn(result) => {
                        result?;
                    }
                    _ => anyhow::bail!("unexpected operation"),
                }
                settled = true;
            }
            ControlUpdate::Returned(input) => anyhow::bail!("input returned: {:?}", input.reason),
            _ => {}
        }
    }
    ensure!(settled, "missing reliable settlement");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = PathBuf::from(std::env::args_os().nth(1).context(
        "usage: delegation ABSOLUTE_STATE [--fork] [--background] [--pause-after-effect|--resume|--new]",
    )?);
    ensure!(root.is_absolute(), "state directory must be absolute");
    let flags = std::env::args().skip(2).collect::<Vec<_>>();
    let fork = flags.iter().any(|flag| flag == "--fork");
    let background = flags.iter().any(|flag| flag == "--background");
    let modes = flags
        .iter()
        .filter(|flag| flag.as_str() != "--fork" && flag.as_str() != "--background")
        .collect::<Vec<_>>();
    let mode = modes.first().map(|flag| flag.as_str());
    let pause = mode == Some("--pause-after-effect");
    let resume = mode == Some("--resume");
    let new = mode == Some("--new");
    ensure!(
        modes.len() <= 1
            && flags.len() == modes.len() + usize::from(fork) + usize::from(background)
            && (mode.is_none() || pause || resume || new)
            && !(background && resume),
        "unknown or repeated mode"
    );
    let parent_turn = if fork { 2 } else { 1 };
    let recovering = resume || new;
    let session = Arc::new(if recovering {
        CodingSession::open(root.join("parent.sqlite"))?
    } else {
        CodingSession::create(root.join("parent.sqlite"), None)?
    });
    let original_id = session.id();
    let child_service = Arc::new(ScriptedModelService::new(if recovering {
        vec![]
    } else {
        vec![
            response(vec![call("effect")]),
            response(vec![Content::Text("child done".into())]),
        ]
    }));
    let owner = Arc::new(BackgroundOwner::new(1)?);
    let synced = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let effects = Arc::new(Effect {
        parent: session.clone(),
        file: root.join("effects"),
        pause,
        release: background.then(|| release.clone()),
        synced: synced.clone(),
    });
    let child_agent = Arc::new(CodingAgent::with_tool_set(
        child_service.clone(),
        Arc::new(ToolSet::new([effects as Arc<dyn CodingToolSource>])),
        model("child"),
    ));
    let tools = Arc::new(Delegate {
        owner: background.then(|| owner.clone()),
        child: SessionDelegation {
            context: if fork {
                ion_core::DelegationContext::Fork {
                    point: ion_core::ForkPoint::AfterTurn(1),
                }
            } else {
                ion_core::DelegationContext::Fresh
            },
            directory: root.join("children"),
            cwd: None,
            application: Arc::new(App {
                agent: child_agent,
                instructions: "child-only instructions",
            }),
            model: Some(model("child")),
            reasoning: Reasoning::ProviderDefault,
            authority: AuthorityScope::new("example-child/environment/v1")?,
        },
    });
    let mut scripts = Vec::new();
    if recovering {
        scripts.push(response(vec![Content::Text(
            "continued without replay".into(),
        )]));
    } else {
        if fork {
            scripts.push(response(vec![Content::Text("source answer".into())]));
        }
        scripts.extend([
            response(vec![call("delegate")]),
            response(vec![Content::Text("parent done".into())]),
        ]);
    }
    let service = Arc::new(ScriptedModelService::new(scripts));
    let agent = Arc::new(
        CodingAgent::with_tool_set(
            service.clone(),
            Arc::new(ToolSet::new([tools as Arc<dyn CodingToolSource>])),
            model("parent"),
        )
        .with_recovery_scope(AuthorityScope::new(if fork {
            "example-parent/environment/fork/v1"
        } else {
            "example-parent/environment/fresh/v1"
        })?),
    );
    let app = Arc::new(App {
        agent,
        instructions: "parent-only instructions",
    });
    let mut control = SessionSetup::new(
        session.clone(),
        SessionCatalog::new(root.clone(), None),
        app,
        None,
        None,
    )?
    .into_control::<()>();
    let key = ion_core::RequestKey::new("example".into(), "parent-delivery".into())?;
    let input = Message::user_input("delegate the effect".into(), []);
    let run: Result<()> = async {
        if fork && !recovering {
            control.submit_request(&ion_core::RequestKey::new("example".into(), "history".into())?, Message::user_input("fork source marker".into(), []), ()).map_err(|rejected| rejected.reason)?;
            drive(&mut control).await?;
        }
        let old = if recovering { Some(session.view()?.delegations().first().context("missing original relationship")?.record.clone()) } else { None };
        let before = session.view()?.entries;
        let admission = control.submit_request(&key, input, ()).map_err(|rejected| rejected.reason)?;
        if recovering {
            ensure!(matches!(admission, RequestAdmission::Existing { receipt, .. } if receipt.turn == parent_turn && (if background { receipt.end == Some(TurnEndReason::Completed) } else { receipt.end.is_none() })), "redelivery must only reconcile original work");
            ensure!(session.view()?.entries == before && !control.is_busy() && service.requests().is_empty(), "redelivery executed or changed work");
            if resume {
                let point = session.recovery_state()?.context("continuation unavailable")?.point;
                control.resume_turn(point, ()).map_err(|(_, error)| error)?;
            } else {
                control.submit_request(&ion_core::RequestKey::new("example".into(), "fresh-delivery".into())?, Message::user_input("new explicit work".into(), []), ()).map_err(|rejected| rejected.reason)?;
            }
        } else {
            ensure!(matches!(admission, RequestAdmission::Started { .. }), "first delivery did not start");
        }
        drive(&mut control).await?;
        if background && !recovering {
            synced.notified().await;
            let link = session.view()?.delegations().into_iter().next().context("missing transfer")?;
            ensure!(link.transferred && link.outcome.is_none(), "release must follow transfer, not child settlement");
            let report = control.shutdown().await;
            ensure!(report.cleanup_errors.is_empty(), "parent shutdown failed");
            if pause {
                println!("BACKGROUND_PARENT_JOINED");
                std::io::stdout().flush()?;
                std::future::pending::<()>().await;
            }
            release.notify_one();
            while !owner.snapshot().children[0].joined { owner.maintain().await; }
            owner.take(link.record.child).context("missing retained background result")??;
        }
        let view = session.view()?;
        let links = view.delegations();
        ensure!(links.len() == 1 && view.id == original_id, "relationship or parent identity changed");
        let child = CodingSession::open(&links[0].record.path)?;
        ensure!(child.id() == links[0].record.child && child.view()?.parent_delegation() == Some(&links[0].record), "child materialization identity changed");
        if recovering {
            ensure!(links[0].record == old.unwrap() && links[0].outcome == Some(DelegationOutcome::Unknown), "lost relationship was replayed or certainty invented");
            ensure!(child.view()?.unfinished_turn == Some(parent_turn), "parent recovery rewrote the lost child's execution truth");
            ensure!(child_service.requests().is_empty() && service.requests().len() == 1, "recovery dispatched child work");
            ensure!(view.last_end == Some((parent_turn + u64::from(!resume), TurnEndReason::Completed)), "wrong recovery Turn");
        } else {
            ensure!(matches!(&links[0].outcome, Some(DelegationOutcome::Joined { receipt: Some(receipt), error: None, answer: Some(_) }) if receipt.end == Some(TurnEndReason::Completed)), "child did not settle");
            ensure!(child_service.requests().len() == 2 && service.requests().len() == 2 + usize::from(fork), "unexpected requests");
            let requests = child_service.requests();
            ensure!(requests[0].instructions.as_deref() == Some("child-only instructions") && requests[0].tools.len() == 1 && requests[0].tools[0].name == "effect", "parent configuration inherited");
            let messages = serde_json::to_string(&requests[0].messages)?;
            ensure!(messages.contains("fork source marker") == fork, "chosen context was not honored");
            ensure!(!messages.contains("delegate the effect"), "active parent Turn leaked past the selected boundary");
        }
        ensure!(std::fs::read(root.join("effects"))? == b"child effect\n", "unexpected host effects");
        Ok(())
    }.await;
    let report = control.shutdown().await;
    let cleanup = owner.join_shutdown().await;
    let run = match (run, cleanup) {
        (run, Ok(())) => run,
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("background cleanup also failed: {cleanup}")))
        }
    };
    ensure!(
        report.cleanup_errors.is_empty(),
        "cleanup failed: {:?}; run: {run:?}",
        report.cleanup_errors
    );
    run?;
    println!(
        "delegation: explicit environment, retained ownership/join, stable identity; no lost child replay"
    );
    Ok(())
}

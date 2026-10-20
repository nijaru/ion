//! Coding policy over ordinary managed Session delegation; no separate loop.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, ensure};
use ion_ai::{BoxFuture, Message, ModelRef, ToolSpec};
use ion_core::{
    AuthorityScope, CodingToolOutput, CodingToolSource, DelegationContext, DelegationOutcome,
    DelegationReceipt, ForkPoint, RequestReceipt, ToolActivityKind, ToolDefinition, ToolExecutor,
    ToolInvocation, ToolPresentation, ToolRegistration, TurnEndReason,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Application, ApplicationRuntime, CodingApplication, CodingRole};
use crate::{BackgroundChildSnapshot, BackgroundOwner, Resources, Selection, SessionDelegation};

pub(super) struct Subagent {
    application: Arc<ChildApplication>,
    directory: PathBuf,
    cwd: PathBuf,
    owner: Arc<BackgroundOwner>,
    authority: AuthorityScope,
}
/// Actual selected route, not a lookup key redirected by later model-store edits.
/// A child captures no application owner and enables no recursive built-in.
struct ChildApplication {
    coding: CodingApplication,
    selected: Selection,
}
impl Application for ChildApplication {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        self.coding.resources(cwd)
    }
    fn runtime(
        &self,
        cwd: Option<&Path>,
        model: Option<&ModelRef>,
        _questions: Option<Arc<crate::QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        ensure!(
            model.is_none_or(|model| *model == self.selected.identity()),
            "child model differs from the prepared coding route"
        );
        self.coding.runtime_with_selection(
            cwd.ok_or_else(|| anyhow::anyhow!("coding requires a working directory"))?,
            &self.selected,
            None,
        )
    }
}
impl Subagent {
    pub(super) fn new(
        parent: &CodingApplication,
        cwd: &Path,
        selected: &Selection,
        owner: Arc<BackgroundOwner>,
        environment: Arc<crate::CommandEnvironment>,
    ) -> Self {
        Self {
            application: Arc::new(ChildApplication {
                coding: CodingApplication {
                    host: parent.host.clone(),
                    external_tools: parent.external_tools.clone(),
                    code_mode: parent.code_mode,
                    selection: parent.selection.clone(),
                    role: CodingRole::Subagent,
                    processes: parent.processes.clone(),
                    command_environment: Some(environment),
                },
                selected: selected.clone(),
            }),
            directory: parent
                .host
                .sessions(Some(cwd.to_path_buf()))
                .directory()
                .join("children"),
            cwd: cwd.to_path_buf(),
            owner,
            // This coding binding makes no durable same-environment claim.
            // Durable hosts supply and re-attest their own versioned scope.
            authority: AuthorityScope::new(format!("ion.coding.live/{}", uuid::Uuid::now_v7()))
                .expect("bounded live authority reference"),
        }
    }
}
impl CodingToolSource for Subagent {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        let point = json!({"oneOf":[
            {"type":"object","additionalProperties":false,"required":["before_turn"],"properties":{"before_turn":{"type":"integer","minimum":1}}},
            {"type":"object","additionalProperties":false,"required":["after_turn"],"properties":{"after_turn":{"type":"integer","minimum":1}}}
        ]});
        let mut choices = vec![
            json!({"type":"object","additionalProperties":false,"required":["action","task"],"properties":{
                "action":{"const":"start"}, "task":{"type":"string","minLength":1,"maxLength":65536},
                "mode":{"enum":["foreground","background"],"default":"foreground"},
                "context":{"oneOf":[
                    {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"fresh"}}},
                    {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"fork"},"point":point}}
                ]}
            }}),
        ];
        choices.push(json!({"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"const":"list"}}}));
        for action in ["status", "wait", "cancel", "acknowledge"] {
            choices.push(json!({"type":"object","additionalProperties":false,"required":["action","child"],"properties":{"action":{"const":action},"child":{"type":"string","format":"uuid"}}}));
        }
        let mut definition = ToolDefinition::external(ToolSpec {
            name: "subagent".into(),
            description: "Managed coding Sessions. start(task, mode=foreground, context={type:fresh}) inherits this request's selected route/reasoning, workspace, resources and tools; no automatic recursive built-in or sandbox/conflict isolation. context={type:fork} copies the settled committed prefix BEFORE the initiating Turn (not its unfinished conversation); optional point selects an earlier before_turn/after_turn boundary, validated by Core. Background start acknowledges committed ownership transfer, NOT completion: it survives parent stop and Session changes, not application shutdown. list/status inspect retained application jobs; wait joins one without consuming it; cancel explicitly stops that child; acknowledge releases a joined result's capacity. Parent stop cancels a wait, not transferred work. Use wait/status before depending on results; answers are child claims, not independent verification. Results and original answers remain in committed relationship facts. Unknown/acknowledged IDs grant no authority or replay. No automatic retries. Normal headless runs drain owned work before exit; quit/EOF/interruption stops and joins it.".into(),
            input_schema: json!({"type":"object","oneOf":choices}),
        });
        definition.presentation =
            ToolPresentation::static_target(ToolActivityKind::Subagent, "Coding task");
        vec![ToolRegistration::new(definition, self)]
    }
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Foreground,
    Background,
}
#[derive(Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Context {
    #[default]
    Fresh,
    Fork {
        #[serde(default)]
        point: Option<ForkPoint>,
    },
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Start {
        task: String,
        #[serde(default)]
        mode: Mode,
        #[serde(default)]
        context: Context,
    },
    List,
    Status {
        child: uuid::Uuid,
    },
    Wait {
        child: uuid::Uuid,
    },
    Cancel {
        child: uuid::Uuid,
    },
    Acknowledge {
        child: uuid::Uuid,
    },
}
fn output(value: Value, is_error: bool) -> CodingToolOutput {
    CodingToolOutput {
        value,
        images: Vec::new(),
        is_error,
    }
}
fn failure(error: impl std::fmt::Display) -> CodingToolOutput {
    output(json!({"error":error.to_string()}), true)
}
fn receipt_output(
    receipt: DelegationReceipt,
    joined: bool,
    error: Option<String>,
) -> CodingToolOutput {
    let completed = matches!(
        &receipt.outcome,
        Some(DelegationOutcome::Joined {
            receipt: Some(RequestReceipt {
                end: Some(TurnEndReason::Completed),
                ..
            }),
            error: None,
            answer: Some(_),
        })
    );
    let is_error = error.is_some() || (joined && !completed);
    output(
        json!({"child_session":receipt.record.child,"path":receipt.record.path,"context":receipt.record.spec.context,
        "transferred":receipt.transferred,"joined":joined,"outcome":receipt.outcome,"error":error}),
        is_error,
    )
}
fn child_output(child: BackgroundChildSnapshot) -> CodingToolOutput {
    receipt_output(child.receipt, child.joined, child.error)
}
impl ToolExecutor for Subagent {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let (Some(reasoning), Some(origin)) = (invocation.reasoning(), invocation.origin())
            else {
                return failure("subagent requires an admitted invocation");
            };
            if invocation.stop().is_cancelled() {
                return failure("stopped before subagent operation");
            }
            let input: Input = match serde_json::from_value(invocation.call().arguments.clone()) {
                Ok(input) => input,
                Err(error) => return failure(error),
            };
            match input {
                Input::Start {
                    task,
                    mode,
                    context,
                } => {
                    if task.trim().is_empty() || task.len() > 65536 {
                        return failure(
                            "subagent task must be nonblank and at most 65536 UTF-8 bytes",
                        );
                    }
                    let context = match context {
                        Context::Fresh => DelegationContext::Fresh,
                        Context::Fork { point } => DelegationContext::Fork {
                            point: point.unwrap_or(ForkPoint::BeforeTurn(origin.turn())),
                        },
                    };
                    let recipe = SessionDelegation {
                        context,
                        directory: self.directory.clone(),
                        cwd: Some(self.cwd.clone()),
                        application: self.application.clone(),
                        model: Some(self.application.selected.identity()),
                        reasoning,
                        authority: self.authority.clone(),
                    };
                    let task = match recipe.prepare(Message::user_input(task, [])) {
                        Ok(task) => task,
                        Err(error) => return failure(error),
                    };
                    let task = match mode {
                        Mode::Foreground => task,
                        Mode::Background => task.in_background(self.owner.destination()),
                    };
                    match invocation.delegate(task).await {
                        Ok(receipt) => {
                            let joined = receipt.outcome.is_some();
                            receipt_output(receipt, joined, None)
                        }
                        Err(error) => failure(error),
                    }
                }
                Input::List => output(
                    json!({"revision":self.owner.revision(),"summary":self.owner.summary(),"jobs":self.owner.jobs()}),
                    false,
                ),
                Input::Status { child } => match self.owner.child(child) {
                    Some(child) => child_output(child),
                    None => failure("child is not owned here (unknown or acknowledged)"),
                },
                Input::Wait { child } => tokio::select! {
                    biased;
                    () = invocation.stop().cancelled() => failure("wait stopped; background child remains owned"),
                    result = self.owner.wait(child) => match result { Ok(child) => child_output(child), Err(error) => failure(error) }
                },
                Input::Cancel { child } => match self.owner.request_stop(child) {
                    Ok(()) => output(json!({"child_session":child,"stop_requested":true}), false),
                    Err(error) => failure(error),
                },
                Input::Acknowledge { child } => match self.owner.take(child) {
                    Some(Ok(receipt)) => receipt_output(receipt, true, None),
                    Some(Err(error)) => failure(error),
                    None => failure(
                        "no joined result to acknowledge (running, unknown or already acknowledged)",
                    ),
                },
            }
        })
    }
}

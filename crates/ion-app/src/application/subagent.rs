//! Coding policy over ordinary managed Session delegation; no separate loop.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, ensure};
use ion_ai::{BoxFuture, Message, ModelRef, ToolSpec};

mod start;
use ion_core::{
    AuthorityScope, DelegationOutcome, DelegationReceipt, RequestReceipt, ToolActivityKind,
    ToolDefinition, ToolExecutor, ToolInvocation, ToolOutput, ToolPresentation, ToolRegistration,
    ToolSource, TurnEndReason,
};
use serde::Deserialize;
use serde_json::{Value, json};
use start::{DEFAULT_STARTUP_TIMEOUT_MS, MAX_STARTUP_TIMEOUT_MS};

use super::{Application, ApplicationRuntime, CodingApplication, CodingRole};
use crate::{BackgroundOwner, BackgroundRunSnapshot, Resources, Selection};

const DEFAULT_JOIN_MS: u64 = 60_000;
const DEFAULT_WAIT_MS: u64 = 300_000;
const MAX_WAIT_MS: u64 = 600_000;
const DEFAULT_EXECUTION_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const MAX_EXECUTION_TIMEOUT_MS: u64 = 2 * 60 * 60 * 1000;

pub(super) struct Subagent {
    application: Arc<ChildApplication>,
    directory: PathBuf,
    cwd: PathBuf,
    owner: Arc<BackgroundOwner>,
    completions: Arc<crate::completion::CompletionRegistry>,
    authority: AuthorityScope,
}
/// Actual selected route, not a lookup key redirected by later model-store edits.
/// A child captures no application owner and enables no recursive built-in.
struct ChildApplication {
    coding: CodingApplication,
    selected: Selection,
}
impl Application for ChildApplication {
    fn session_retirements(&self) -> Option<crate::SessionRetirements> {
        self.coding.session_retirements()
    }
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
        owner: &crate::ApplicationOwner,
        environment: Arc<crate::CommandEnvironment>,
        search_settings: Arc<crate::SearchSettings>,
        tool_veto: Option<Arc<crate::ToolVetoSettings>>,
    ) -> Self {
        Self {
            application: Arc::new(ChildApplication {
                coding: CodingApplication {
                    host: parent.host.clone(),
                    external_tools: parent.external_tools.clone(),
                    mcp_tools: parent.mcp_tools.clone(),
                    code_mode: parent.code_mode,
                    selection: parent.selection.clone(),
                    role: CodingRole::Subagent,
                    session_retirements: parent.session_retirements.clone(),
                    processes: parent.processes.clone(),
                    command_environment: Some(environment),
                    search_settings: Some(search_settings),
                    tool_veto,
                },
                selected: selected.clone(),
            }),
            directory: parent
                .host
                .sessions(Some(cwd.to_path_buf()))
                .directory()
                .join("children"),
            cwd: cwd.to_path_buf(),
            owner: owner.background().clone(),
            completions: owner.completions().clone(),
            // This coding binding makes no durable same-environment claim.
            // Durable hosts supply and re-attest their own versioned scope.
            authority: AuthorityScope::new(format!("ion.coding.live/{}", uuid::Uuid::now_v7()))
                .expect("bounded live authority reference"),
        }
    }
}
impl ToolSource for Subagent {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        let point = json!({"oneOf":[
            {"type":"object","additionalProperties":false,"required":["before_turn"],"properties":{"before_turn":{"type":"integer","minimum":1}}},
            {"type":"object","additionalProperties":false,"required":["after_turn"],"properties":{"after_turn":{"type":"integer","minimum":1}}}
        ]});
        let mut choices = vec![
            json!({"type":"object","additionalProperties":false,"required":["action","task"],"properties":{
                "action":{"const":"start"}, "task":{"type":"string","minLength":1,"maxLength":65536},
                "mode":{"enum":["foreground","background"],"default":"foreground"},
                "join_timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_WAIT_MS,"default":DEFAULT_JOIN_MS,"description":"Foreground only. Join this same run for at most this budget after live admission (including child setup). If still joining, commit transfer to the application owner and return its running handle. No restart, stop or execution deadline change. Capacity is reserved before work; parent stop before transfer still stops and fully joins."},
                "resume":{"type":"string","minLength":1,"maxLength":4096,"description":"Existing settled Session file to continue under a new run ID. Relative to the parent workspace; mutually exclusive with context. Requires its original cwd and exclusive writer. Current call configuration supplies authority, model and reasoning; saved metadata is not a grant."},
                "cwd":{"type":"string","minLength":1,"maxLength":4096,"description":"Child workspace relative to the parent or absolute; omitted inherits the parent. Resume must use its original workspace."},
                "startup_timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_STARTUP_TIMEOUT_MS,"default":DEFAULT_STARTUP_TIMEOUT_MS,"description":"One budget for model/workspace/resume/resource/runtime preflight before child admission. Expiry cancels preparation and joins all started work, which can outlive the budget. No child is admitted on expiry; startup_timed_out is distinct from execution timeout."},
                "execution_timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_EXECUTION_TIMEOUT_MS,"default":DEFAULT_EXECUTION_TIMEOUT_MS,"description":"Child runtime budget from task submission, independent of parent wait budgets. Expiry requests stop and joins cleanup; the retained outcome records execution_timed_out. Not rollback or a hard cleanup deadline."},
                "model":{"type":"string","description":"Exact provider/model identifier from the known catalog or configured routes; omitted inherits the captured parent route. No endpoints or credentials."},
                "reasoning":{"type":"string","description":"Generation effort for the child: default, off, low, medium, high or budget:TOKENS. Omitted inherits the parent's effort, even with a different model. Unsupported controls fail before child admission."},
                "context":{"oneOf":[
                    {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"fresh"}}},
                    {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"fork"},"point":point}}
                ]}
            }}),
        ];
        choices.push(json!({"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"const":"list"}}}));
        choices.push(json!({"type":"object","additionalProperties":false,"required":["action","ids"],"properties":{
            "action":{"const":"wait"},
            "ids":{"type":"array","items":{"type":"string","format":"uuid"},"minItems":1,"maxItems":64,"uniqueItems":true},
            "timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_WAIT_MS,"default":DEFAULT_WAIT_MS,"description":"One shared wait budget, not a child execution deadline. Returns all selected reports/statuses when any child joins, or expires with children still running. No cancellation or acknowledgement."}
        }}));
        choices.push(json!({"type":"object","additionalProperties":false,"required":["action","id","message"],"properties":{
            "action":{"const":"steer"}, "id":{"type":"string","format":"uuid"},
            "message":{"type":"string","minLength":1,"maxLength":65536,"description":"Literal guidance for the owned running child, queued at the existing safe model-step boundary. No new Turn, route, instructions or authority. Queued is not committed; inspect steering status for commitment or returned input."}
        }}));
        for action in ["status", "cancel", "acknowledge"] {
            choices.push(json!({"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":action},"id":{"type":"string","format":"uuid"}}}));
        }
        let mut definition = ToolDefinition::external(ToolSpec {
            name: "subagent".into(),
            description: concat!(
                "Run a managed coding Session. Tools are inherited; route, effort and workspace can be overridden. No automatic recursion or workspace isolation. Fresh context is the default; fork copies the settled prefix before this Turn (or an earlier specified boundary). Resume continues a saved conversation under a new run ID. ",
                "Use the returned id for controls, not child_session or path. Foreground joins within join_timeout_ms; join_expired transfers that same run to the application. Background transfers immediately and rejects join_timeout_ms. Transferred work survives parent stop, not application shutdown. Wait or check status on its ID instead of starting a duplicate. ",
                "wait returns when any selected child joins or the wait expires; wait again only on running IDs. steer queues guidance without interrupting tools; check delivery in status. cancel stops the child. acknowledge releases a joined slot without deleting the conversation. ",
                "Startup, execution and waiting have separate budgets. Timeout does not roll back effects. Joined means cleanup finished, not success; check the outcome and verify the answer. Provisional or partial output is not a completed result. Recover settled outcomes after acknowledgement or reopen via unfiltered history search for the run UUID."
            ).into(),
            input_schema: json!({"type":"object","oneOf":choices}),
        });
        definition.presentation =
            ToolPresentation::static_target(ToolActivityKind::Subagent, "Coding task");
        vec![ToolRegistration::new(definition, self)]
    }
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Start(start::Start),
    List,
    Status {
        id: uuid::Uuid,
    },
    Wait {
        ids: Vec<uuid::Uuid>,
        #[serde(default = "default_wait_ms")]
        timeout_ms: u64,
    },
    Steer {
        id: uuid::Uuid,
        message: String,
    },
    Cancel {
        id: uuid::Uuid,
    },
    Acknowledge {
        id: uuid::Uuid,
    },
}
fn default_wait_ms() -> u64 {
    DEFAULT_WAIT_MS
}
fn output(value: Value, is_error: bool) -> ToolOutput {
    ToolOutput {
        artifacts: Vec::new(),
        value,
        images: Vec::new(),
        is_error,
    }
}
fn failure(error: impl std::fmt::Display) -> ToolOutput {
    output(json!({"error":error.to_string()}), true)
}
fn receipt_output(receipt: DelegationReceipt, joined: bool, error: Option<String>) -> ToolOutput {
    let completed = matches!(
        &receipt.outcome,
        Some(DelegationOutcome::Joined {
            execution_timed_out: false,
            receipt: Some(RequestReceipt {
                end: Some(TurnEndReason::Completed),
                ..
            }),
            error: None,
            answer: Some(_),
            ..
        })
    );
    let is_error = error.is_some() || (joined && !completed);
    output(
        json!({"id":receipt.record.run,"child_session":receipt.record.child,"path":receipt.record.path,"target":receipt.record.spec.target,
        "model":receipt.record.spec.model,"reasoning":receipt.record.spec.reasoning.to_string(),
        "execution_timeout_ms":receipt.record.spec.execution_timeout_ms,
        "transferred":receipt.transferred,"joined":joined,"outcome":receipt.outcome,"error":error}),
        is_error,
    )
}
fn child_output(child: BackgroundRunSnapshot) -> ToolOutput {
    let mut output = receipt_output(child.receipt, child.joined, child.error);
    output.value["steering"] = json!(child.steering);
    output.value["progress"] = json!(child.progress);
    output
}
impl ToolExecutor for Subagent {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
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
                Input::Start(start) => {
                    let startup_timeout_ms = start.startup_timeout_ms;
                    let prepared = match start
                        .prepare(self, reasoning, origin.turn(), invocation.stop())
                        .await
                    {
                        Ok(prepared) => prepared,
                        Err(error) => return start::failure(error, startup_timeout_ms),
                    };
                    let target = match self.completions.parent_target(origin) {
                        Ok(target) => target,
                        Err(error) => return failure(error),
                    };
                    let task = if let Some(target) = target {
                        prepared.task.with_completion_target(target)
                    } else {
                        prepared.task
                    };
                    let join_timeout_ms = prepared.join_timeout_ms;
                    let task = match join_timeout_ms {
                        Some(ms) => {
                            task.in_background_after(&self.owner, Duration::from_millis(ms))
                        }
                        None => task.in_background(&self.owner),
                    };
                    match invocation.delegate(task).await {
                        Ok(receipt) => {
                            let joined = receipt.outcome.is_some();
                            let join_expired = join_timeout_ms.is_some() && receipt.transferred;
                            let mut output = receipt_output(receipt, joined, None);
                            output.value["startup_timeout_ms"] = json!(startup_timeout_ms);
                            output.value["join_timeout_ms"] = json!(join_timeout_ms);
                            output.value["join_expired"] = json!(join_expired);
                            output
                        }
                        Err(error) => failure(error),
                    }
                }
                Input::Steer { id, message } => {
                    if message.trim().is_empty() || message.len() > 65536 {
                        return failure(
                            "steering message must be nonblank and at most 65536 UTF-8 bytes",
                        );
                    }
                    let request_id = format!(
                        "{}:{}:{}:{}:{:?}",
                        origin.session(),
                        origin.turn(),
                        origin.occurrence().assistant_entry,
                        origin.occurrence().ordinal,
                        origin.composed_child()
                    );
                    let request =
                        ion_core::RequestKey::new("ion.delegation.steering".into(), request_id)
                            .expect(
                                "fixed namespace and engine coordinates fit request-key bounds",
                            );
                    match self
                        .owner
                        .steer(
                            id,
                            &request,
                            &Message::user_input(message, []),
                            invocation.stop(),
                        )
                        .await
                    {
                        Ok(report) => output(json!({"id":id,"steering":report}), false),
                        Err(error) => failure(error),
                    }
                }
                Input::List => output(
                    json!({"revision":self.owner.revision(),"summary":self.owner.summary(),"jobs":self.owner.jobs()}),
                    false,
                ),
                Input::Status { id } => match self.owner.run(id) {
                    Some(child) => child_output(child),
                    None => failure("child is not owned here (unknown or acknowledged)"),
                },
                Input::Wait { ids, timeout_ms } => {
                    if !(1..=MAX_WAIT_MS).contains(&timeout_ms) {
                        return failure("wait timeout_ms must be 1..=600000");
                    }
                    tokio::select! {
                        biased;
                        () = invocation.stop().cancelled() => failure("wait stopped; background children remain owned"),
                        result = self.owner.wait(&ids, Duration::from_millis(timeout_ms)) => match result {
                            Ok(report) => {
                                let mut is_error = false;
                                let runs: Vec<_> = report.runs.into_iter().map(|child| {
                                    let child = child_output(child);
                                    is_error |= child.is_error;
                                    child.value
                                }).collect();
                                output(json!({"wait_expired":report.wait_expired,"runs":runs}), is_error)
                            },
                            Err(error) => failure(error),
                        }
                    }
                }
                Input::Cancel { id } => match self.owner.request_stop(id) {
                    Ok(()) => output(json!({"id":id,"stop_requested":true}), false),
                    Err(error) => failure(error),
                },
                Input::Acknowledge { id } => match self.owner.take(id) {
                    Some(Ok(report)) => child_output(report),
                    Some(Err(error)) => child_output(error.run),
                    None => failure(
                        "no joined result to acknowledge (running, unknown or already acknowledged)",
                    ),
                },
            }
        })
    }
}

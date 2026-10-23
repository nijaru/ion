//! Coding policy over ordinary managed Session delegation; no separate loop.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, ensure};
use ion_ai::{BoxFuture, Message, ModelRef, ToolSpec};
use ion_core::{
    AuthorityScope, CodingToolOutput, CodingToolSource, DelegationOutcome, DelegationReceipt,
    DelegationTarget, ForkPoint, RequestReceipt, ToolActivityKind, ToolDefinition, ToolExecutor,
    ToolInvocation, ToolPresentation, ToolRegistration, TurnEndReason,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Application, ApplicationRuntime, CodingApplication, CodingRole};
use crate::{BackgroundOwner, BackgroundRunSnapshot, Resources, Selection, SessionDelegation};

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
        search_settings: Arc<crate::SearchSettings>,
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
                    search_settings: Some(search_settings),
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
                "join_timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_WAIT_MS,"default":DEFAULT_JOIN_MS,"description":"Foreground only. Join this same run for at most this budget after live admission (including child setup). If still joining, commit transfer to the application owner and return its running handle. No restart, stop or execution deadline change. Capacity is reserved before work; parent stop before transfer still stops and fully joins."},
                "resume":{"type":"string","minLength":1,"maxLength":4096,"description":"Existing settled Session file to continue under a new run ID. Relative to the parent workspace; mutually exclusive with context. Requires its original cwd and exclusive writer. Current call configuration supplies authority, model and reasoning; saved metadata is not a grant."},
                "cwd":{"type":"string","minLength":1,"maxLength":4096,"description":"Child workspace relative to the parent or absolute; omitted inherits the parent. Resume must use its original workspace."},
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
            description: "Managed coding Sessions. start(task, mode=foreground, join_timeout_ms=60000, context={type:fresh}, resume?, cwd?, model?, reasoning?, execution_timeout_ms=1800000) inherits this request's captured route/reasoning unless explicitly overridden. model is an exact provider/model from the known catalog or configured routes, using that route's credential policy. No silent route/effort fallback; overrides affect only the new child, not this parent or user configuration. Workspace inherits unless cwd is explicit; resume must use the saved original workspace. Resources are loaded for that workspace and selected tools remain inherited; no automatic recursive built-in or sandbox/conflict isolation. context={type:fork} copies the settled committed prefix BEFORE the initiating Turn (not its unfinished conversation); optional point selects an earlier before_turn/after_turn boundary, validated by Core. resume=path is mutually exclusive with context and explicitly starts a new run in the same settled conversation, never a fork or replay of unfinished work. It acquires the exclusive writer before admission and uses this call's captured authority/model/reasoning, not saved grants or preferences. Each result has id (run handle), child_session (conversation UUID) and path. Old runs retain independent results, waiters and controls; acknowledge does not delete the conversation. Foreground reserves application capacity and joins for at most join_timeout_ms (1..600000 ms; default 60 seconds), including child materialization/setup after live admission but not synchronous recipe preflight. Completion wins a ready expiry race and returns its joined result without transfer or retained capacity. Otherwise commit transfer of the SAME execution and return join_expired=true, joined=false and its id; continue with wait/status, never launch a replacement. Join expiry does not stop work, reset its execution deadline or imply success. Parent stop before transfer stops and fully joins that child, even beyond the join budget; after transfer only the application owner controls its lifetime. Explicit background mode rejects join_timeout_ms and transfers immediately. Background start or join expiry acknowledges committed ownership transfer, NOT completion: it survives parent stop and Session changes, not application shutdown. list/status inspect retained application jobs; wait(ids, timeout_ms=300000) returns all selected reports/statuses when any joins, or wait_expired=true after the shared budget (1..600000 ms), without consuming results or stopping children. Wait again only on still-running IDs. steer(id, message) queues literal guidance in that child's existing shared control, after current tool calls and before its next model step. It does not interrupt work, start another Turn, reset the execution deadline or change authority/model/instructions. Queued is not committed delivery: status/wait retain per-input committed, returned or unknown states. Late uncommitted guidance returns its original input; stopped/settled/acknowledged run IDs reject new guidance, never resume implicitly. Parent cancellation before steering admission leaves the child running; after admission it cannot recall queued input. Live request identity reconciles exact repeated invocations, not owner loss. cancel explicitly stops that child; acknowledge releases a joined result's capacity. Parent stop cancels a wait, not transferred work. Use wait/status before depending on results; answers are child claims, not independent verification. Results and original answers remain in committed relationship facts. Unknown/acknowledged IDs grant no authority or replay. Each child has a separate execution budget (default 30 minutes, 1..7200000 ms). Expiry requests stop and fully joins before reporting execution_timed_out; effects are not rolled back or safely replayable, and cleanup may outlive the budget. Joined outcomes retain elapsed_ms through observed stop/settlement, excluding subsequent cleanup. Failures may retain partial_output (latest visible text, at most 50 KiB UTF-8, with truncated/observation_gap flags), not a successful answer or evidence of completed effects. Thinking and tool arguments/results are excluded. Unknown timing is null. Owned status/wait/list expose ephemeral progress: actual child/session request coordinates, elapsed_ms, observed UTC last_activity_at_ms, content-free activity, bounded current tool NAME only, stopping and provisional_output (same 50 KiB UTF-8 truncation/gap rules). Reads and silence do not advance last activity or refresh deadlines. No progress is a heartbeat, committed fact, successful answer or stall verdict; joined alone means full cleanup finished. No automatic retries. Normal headless runs drain owned work before exit; quit/EOF/interruption stops and joins it.".into(),
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
        #[serde(default, deserialize_with = "explicit_join_timeout")]
        join_timeout_ms: Option<u64>,
        context: Option<Context>,
        resume: Option<String>,
        cwd: Option<PathBuf>,
        model: Option<String>,
        reasoning: Option<String>,
        #[serde(default = "default_execution_timeout_ms")]
        execution_timeout_ms: u64,
    },
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
// Missing means use policy; an explicitly supplied null is not an integer budget.
fn explicit_join_timeout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}
fn default_execution_timeout_ms() -> u64 {
    DEFAULT_EXECUTION_TIMEOUT_MS
}
fn default_wait_ms() -> u64 {
    DEFAULT_WAIT_MS
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
fn child_output(child: BackgroundRunSnapshot) -> CodingToolOutput {
    let mut output = receipt_output(child.receipt, child.joined, child.error);
    output.value["steering"] = json!(child.steering);
    output.value["progress"] = json!(child.progress);
    output
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
                    join_timeout_ms,
                    context,
                    resume,
                    cwd,
                    model,
                    reasoning: requested_reasoning,
                    execution_timeout_ms,
                } => {
                    if join_timeout_ms.is_some_and(|ms| !(1..=MAX_WAIT_MS).contains(&ms)) {
                        return failure("join_timeout_ms must be 1..=600000");
                    }
                    if matches!(mode, Mode::Background) && join_timeout_ms.is_some() {
                        return failure("join_timeout_ms applies only to foreground start");
                    }
                    if !(1..=MAX_EXECUTION_TIMEOUT_MS).contains(&execution_timeout_ms) {
                        return failure("execution_timeout_ms must be 1..=7200000");
                    }
                    if task.trim().is_empty() || task.len() > 65536 {
                        return failure(
                            "subagent task must be nonblank and at most 65536 UTF-8 bytes",
                        );
                    }
                    let reasoning = match requested_reasoning {
                        Some(value) => match value.parse() {
                            Ok(reasoning) => reasoning,
                            Err(error) => return failure(error),
                        },
                        None => reasoning,
                    };
                    let application = if let Some(model) = model {
                        let Some((provider, model)) = model
                            .split_once('/')
                            .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
                        else {
                            return failure(
                                "subagent model must be an exact provider/model identifier",
                            );
                        };
                        let identity = ModelRef {
                            provider: provider.into(),
                            model: model.into(),
                        };
                        // Same-identity selection must retain per-invocation overrides
                        // and cannot be redirected by later model-store changes.
                        let selected = if identity == self.application.selected.identity() {
                            self.application.selected.clone()
                        } else {
                            match self
                                .application
                                .coding
                                .host
                                .models()
                                .resolve_identity(&identity)
                            {
                                Ok(selected) => selected,
                                Err(error) => return failure(error),
                            }
                        };
                        Arc::new(ChildApplication {
                            coding: self.application.coding.clone(),
                            selected,
                        })
                    } else {
                        self.application.clone()
                    };
                    if resume.is_some() && context.is_some() {
                        return failure("resume and context are mutually exclusive");
                    }
                    let target = if let Some(path) = resume {
                        if path.is_empty() || path.len() > 4096 {
                            return failure("resume must be 1..=4096 UTF-8 bytes");
                        }
                        match DelegationTarget::resume(self.cwd.join(path)) {
                            Ok(target) => target,
                            Err(error) => return failure(error),
                        }
                    } else {
                        match context.unwrap_or_default() {
                            Context::Fresh => DelegationTarget::Fresh {
                                directory: self.directory.clone(),
                            },
                            Context::Fork { point } => DelegationTarget::Fork {
                                directory: self.directory.clone(),
                                point: point.unwrap_or(ForkPoint::BeforeTurn(origin.turn())),
                            },
                        }
                    };
                    let cwd = match cwd {
                        Some(path)
                            if path.as_os_str().is_empty()
                                || path.as_os_str().as_encoded_bytes().len() > 4096 =>
                        {
                            return failure("cwd must be 1..=4096 UTF-8 bytes");
                        }
                        Some(path) => self.cwd.join(path),
                        None => self.cwd.clone(),
                    };
                    let recipe = SessionDelegation {
                        target,
                        cwd: Some(cwd),
                        model: Some(application.selected.identity()),
                        application,
                        reasoning,
                        execution_timeout_ms: Some(execution_timeout_ms),
                        authority: self.authority.clone(),
                    };
                    let task = match recipe.prepare(Message::user_input(task, [])) {
                        Ok(task) => task,
                        Err(error) => return failure(error),
                    };
                    let join_timeout_ms = matches!(mode, Mode::Foreground)
                        .then(|| join_timeout_ms.unwrap_or(DEFAULT_JOIN_MS));
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

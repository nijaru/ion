//! JSONL translation over the sole shared Host input and execution owner.
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ion_core::ForkPoint;
use ion_host::{
    Delivery, InputId, InputPhase, OperationId, SessionControl, SessionSetup, UserInput,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::BufReader;

use crate::{
    inspection, preview_input,
    publication::{Output, ReplyCredit},
    stdio,
};

mod connection;
mod input;
mod lifecycle;
mod questions;
mod requests;
#[cfg(test)]
mod tests;
use input::CommandReader;

/// Host retains this until return/settlement. The credit also outlives any
/// queued acknowledgement; neither protocol IDs nor reply leases own input.
#[derive(Serialize)]
struct RpcMetadata {
    id: Option<Value>,
    command: &'static str,
    #[serde(skip)]
    credit: ReplyCredit,
}
struct Correlation {
    id: Option<Value>,
    command: &'static str,
    credit: ReplyCredit,
    acknowledged: bool,
    position: usize,
    /// Direct keyed admission commits before the asynchronous Host callback.
    turn: Option<u64>,
}
struct OperationReply {
    operation: OperationId,
    id: Option<Value>,
    /// Automated turns have no client request or reply credit.
    credit: Option<ReplyCredit>,
    turn: Option<u64>,
}
struct Control {
    services: Arc<ion_host::Host>,
    host: SessionControl<RpcMetadata>,
    question_responder: Option<ion_host::QuestionResponder>,
    /// One coalesced invalidation, not another owner of the live request.
    question_changed: bool,
    correlations: BTreeMap<InputId, Correlation>,
    turn_reply: Option<OperationReply>,
    /// Publication loss invalidates the whole remaining provisional stream,
    /// not just one delta. Only settlement permits a new stream.
    suppress_activity: bool,
    gap: Option<u64>,
    refused: u64,
    unacknowledged_stops: u64,
}
impl Control {
    fn new(setup: SessionSetup, services: Arc<ion_host::Host>) -> Self {
        Self {
            services,
            host: setup.into_control::<RpcMetadata>(),
            question_responder: None,
            question_changed: false,
            correlations: BTreeMap::new(),
            turn_reply: None,
            suppress_activity: false,
            gap: None,
            refused: 0,
            unacknowledged_stops: 0,
        }
    }
    fn session_id(&self) -> String {
        self.host.session_id()
    }
    fn cancel_background(&self, value: &Value) -> Result<Value> {
        let run = required_string(value, "run")?.parse::<uuid::Uuid>()?;
        self.host
            .background()
            .context("application has no background owner")?
            .request_stop(run)?;
        Ok(json!({"run":run,"stop_requested":true}))
    }

    fn idle(&self) -> Result<()> {
        ensure!(
            !self.host.is_busy(),
            "an operation or input preparation is active; abort or wait for settlement"
        );
        Ok(())
    }

    /// A transport reply credit has already been reserved. This method never
    /// waits for publication and never prepares, queues or executes input itself.
    fn command(&mut self, value: Value, credit: ReplyCredit) -> Option<(Value, ReplyCredit)> {
        let id = value.get("id").cloned();
        let command = value.get("type").and_then(Value::as_str).unwrap_or("parse");
        if !value.is_object() || id.as_ref().is_some_and(|id| !id.is_string()) {
            return Some((
                failure(
                    None,
                    "parse",
                    "command must be an object with an optional string id",
                ),
                credit,
            ));
        }
        if matches!(command, "prompt" | "steer" | "follow_up") {
            return match self.submit(&value, id.clone(), command, credit.clone()) {
                Ok(()) => None,
                Err(error) => Some((failure(id, command, &format!("{error:#}")), credit)),
            };
        }
        let outcome: Result<Value> = (|| {
            match command {
                "submit_request" => self.submit_request(&value, id.clone(), credit.clone()),
                "reconnect_mcp" => {
                    let server = required_string(&value, "server")?;
                    let metadata = RpcMetadata { id: id.clone(), command: "reconnect_mcp", credit: credit.clone() };
                    let operation = self.host.reconnect_mcp(server, metadata).map_err(|(_, error)| error)?;
                    Ok(json!({"disposition":"admitted","operation":operation,"server":server}))
                }
                "get_request" => self.get_request(&value),
                "get_question" => self.get_question(&value),
                "answer_question" => self.answer_question(&value),
                "cancel_question" => self.cancel_question(&value),
                "clear_queue" => {
                    self.host.clear_queue();
                    // Recovery arrives as reliable Returned updates, not a
                    // second client-side drain or a duplicate full-input reply.
                    Ok(json!({"disposition":"requested","recovery":"input_return_events"}))
                }
                "abort" => {
                    ensure!(self.host.is_busy(), "no active operation or input preparation");
                    self.host.request_stop();
                    Ok(json!({"disposition":"requested"}))
                }
                "get_state" => {
                    let view = self.host.session().view()?;
                    let state = self.host.state();
                    let operation = state.operation.map(|operation| match operation.kind { ion_host::OperationKind::Turn => "turn", ion_host::OperationKind::Compact => "compact", ion_host::OperationKind::Shell => "shell" });
                    Ok(json!({"session":self.session_id(),"cwd":view.cwd,"name":view.name,"model":self.host.agent().model(),"reasoning":view.reasoning.to_string(),"busy":state.busy,"operation":operation,"preparing_input":state.preparing_input,"mcp_reconnect":state.mcp_reconnect,"completion_delivery":state.completion_delivery,"entries":view.entries.len(),"follow_ups":state.pending_follow_ups}))
                }
                "set_completion_delivery" => {
                    let delivery = required_string(&value, "delivery")?.parse().map_err(anyhow::Error::msg)?;
                    self.host.set_completion_delivery(delivery)?;
                    Ok(json!({"completion_delivery":self.host.completion_delivery(),"previous_grants":"revoked"}))
                }
                "get_background" => Ok(json!({"background":self.host.background().map(|owner| owner.snapshot())})),
                "cancel_background" => self.cancel_background(&value),
                "acknowledge_background" => {
                    let run = required_string(&value, "run")?.parse::<uuid::Uuid>()?;
                    let owner = self.host.background().context("application has no background owner")?;
                    let result = owner.take(run).context("no joined result to acknowledge (running, unknown or already acknowledged)")??;
                    Ok(json!({"receipt":result.receipt,"steering":result.steering,"progress":result.progress}))
                }
                "inspect" => inspection::to_value(&self.host.session().view()?),
                "list_sessions" => Ok(json!(self.host.catalog().list()?.iter().map(|item| json!({"id":item.id,"name":item.name,"preview":item.preview,"turns":item.turns,"model":item.model})).collect::<Vec<_>>())),
                "get_context" => {
                    let view = self.host.session().view()?;
                    Ok(json!({"position":view.context_position(),"points":view.context_points().iter().map(|point| json!({"entry":point.entry,"head_entry":point.head_entry,"restore_input":point.restore_input,"selected_path":point.selected_path,"preview":preview_input(&point.message)})).collect::<Vec<_>>()}))
                }
                "navigate_context" => {
                    self.idle()?;
                    let expected: ion_core::ContextPosition = serde_json::from_value(value.get("expected").context("expected context position is required")?.clone())?;
                    let head = value.get("head_entry").and_then(Value::as_u64).context("head_entry must be an unsigned integer")?;
                    let position = self.host.navigate_context(expected, head)?;
                    Ok(json!({"position":position,"files_unchanged":true}))
                }
                "list_turns" => Ok(json!(self.host.session().view()?.turns().iter().map(|item| json!({"turn":item.turn,"preview":preview_input(&item.input),"ended":item.end.is_some()})).collect::<Vec<_>>())),
                "list_models" => Ok(json!(self.services.models().choices(self.services.credentials())?.iter().map(|item| json!({"provider":item.selected.provider,"model":item.selected.model,"label":item.label,"image_input":item.selected.image_input})).collect::<Vec<_>>())),
                "list_resources" => Ok(json!({"skills":self.host.resources().skills().map(|item| json!({"name":item.name,"description":item.description})).collect::<Vec<_>>(),"prompts":self.host.resources().templates().map(|item| json!({"name":item.name,"description":item.description})).collect::<Vec<_>>(),"diagnostics":self.host.resources().diagnostics().iter().map(|item| json!({"path":item.path,"message":item.message})).collect::<Vec<_>>()})),
                "reload_resources" => {
                    self.idle()?;
                    self.host.reload_resources()?;
                    Ok(json!({"skills":self.host.resources().skills().count(),"prompts":self.host.resources().templates().count()}))
                }
                "compact" => {
                    fields(&value, &["type", "id", "instructions"])?;
                    let instructions = value.get("instructions").map(|text| {
                        text.as_str().context("instructions must be a string").map(str::to_owned)
                    }).transpose()?;
                    let metadata = RpcMetadata { id: id.clone(), command: "compact", credit: credit.clone() };
                    self.host.compact(instructions, metadata).map_err(|(_, error)| error)?;
                    Ok(json!({"disposition":"started"}))
                }
                "shell" => {
                    let command = required_string(&value, "command")?;
                    ensure!(!command.trim().is_empty(), "command is empty");
                    let excluded = value.get("exclude_from_context").map_or(Ok(false), |value| value.as_bool().context("exclude_from_context must be a boolean"))?;
                    let metadata = RpcMetadata { id: id.clone(), command: "shell", credit: credit.clone() };
                    self.host.shell(command.to_owned(), excluded, metadata).map_err(|(_, error)| error)?;
                    Ok(json!({"disposition":"started"}))
                }
                "set_reasoning" => {
                    self.idle()?;
                    let reasoning = required_string(&value, "effort")?.parse::<ion_ai::Reasoning>().map_err(anyhow::Error::msg)?;
                    self.host.select_reasoning(reasoning)?;
                    Ok(json!({"reasoning":reasoning.to_string()}))
                }
                "set_model" => {
                    self.idle()?;
                    let provider = required_string(&value, "provider")?;
                    let model = required_string(&value, "model")?;
                    self.host.select_model(ion_ai::ModelRef { provider: provider.to_owned(), model: model.to_owned() })?;
                    Ok(json!({"model":self.host.agent().model()}))
                }
                "new_session" => {
                    self.idle()?;
                    self.host.new_session()?;
                    Ok(json!({"session":self.session_id(),"model":self.host.agent().model()}))
                }
                "clone_session" => {
                    self.idle()?;
                    let session = self.host.clone_session()?;
                    Ok(json!({"session":session,"model":self.host.agent().model()}))
                }
                "fork" => {
                    self.idle()?;
                    let turn = value.get("turn").and_then(Value::as_u64).context("turn must be an unsigned integer")?;
                    let after = value.get("after").map(|value| value.as_bool().context("after must be a boolean")).transpose()?.unwrap_or(false);
                    self.host.fork_session(if after { ForkPoint::AfterTurn(turn) } else { ForkPoint::BeforeTurn(turn) })?;
                    Ok(json!({"session":self.session_id(),"model":self.host.agent().model()}))
                }
                "switch_session" => {
                    self.idle()?;
                    self.host.switch_session(PathBuf::from(required_string(&value, "session")?))?;
                    Ok(json!({"session":self.session_id(),"model":self.host.agent().model()}))
                }
                "set_name" => {
                    self.idle()?;
                    let name = match value.get("name") {
                        None | Some(Value::Null) => None,
                        Some(value) => Some(value.as_str().context("name must be a string or null")?),
                    };
                    self.host.session().set_name(name)?;
                    Ok(json!({"name":name}))
                }
                _ => bail!("unknown command: {command}"),
            }
        })();
        Some((
            match outcome {
                Ok(data) => success(id, command, data),
                Err(error) => failure(id, command, &format!("{error:#}")),
            },
            credit,
        ))
    }

    fn submit(
        &mut self,
        value: &Value,
        id: Option<Value>,
        command: &str,
        credit: ReplyCredit,
    ) -> Result<()> {
        let (command, delivery) = match command {
            "prompt" => ("prompt", Delivery::Start),
            "steer" => (
                "steer",
                Delivery::Steer(
                    self.host
                        .current_operation()
                        .context("no active Turn; use prompt instead")?,
                ),
            ),
            "follow_up" => ("follow_up", Delivery::FollowUp),
            _ => unreachable!("input command"),
        };
        let text = required_string(value, "message")?.to_owned();
        let images = value.get("images").map_or(Ok(Vec::new()), |images| {
            use ion_host::image_input::ImageSource;
            images
                .as_array()
                .context("images must be an array of local paths or inline images")?
                .iter()
                .map(|image| {
                    if let Some(path) = image.as_str() {
                        Ok(ImageSource::Path(PathBuf::from(path)))
                    } else {
                        Ok(ImageSource::Encoded {
                            mime_type: required_string(image, "mime_type")?.to_owned(),
                            data: required_string(image, "data")?.to_owned(),
                        })
                    }
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let position = self.host.state().pending_follow_ups + 1;
        let metadata = RpcMetadata {
            id: id.clone(),
            command,
            credit: credit.clone(),
        };
        let input = self
            .host
            .submit(
                delivery,
                UserInput::Prompt {
                    text,
                    images,
                    files: Vec::new(),
                },
                metadata,
            )
            .map_err(|rejected| rejected.reason)?;
        self.correlations.insert(
            input,
            Correlation {
                id,
                command,
                credit,
                acknowledged: false,
                position,
                turn: None,
            },
        );
        Ok(())
    }
}

pub async fn run(
    setup: SessionSetup,
    services: Arc<ion_host::Host>,
    mut interrupt: tokio::signal::unix::Signal,
) -> Result<()> {
    let mut control = Control::new(setup, services);
    let opened = stdio::Stdio::open(
        (std::io::stdin(), tokio::io::Interest::READABLE),
        (std::io::stdout(), tokio::io::Interest::WRITABLE),
    );
    let (input, writer) = match opened {
        Ok(io) => io,
        Err(error) => return connection::startup_failure(&mut control, error.into()).await,
    };
    let mut output = Output::new(writer);
    // No effects have started, but even initial publication errors go through
    // the connection's shared shutdown path.
    if let Err(error) = output.enqueue(json!({"type":"ready","session":control.session_id(),"cwd":control.host.session().cwd(),"publication":{"reply_slots":connection::REPLY_SLOTS,"inspection_slots":1,"overflow":"commands_not_admitted","abort":"always_stops_reply_may_gap","progress":"gap_resync_inspect"}})) {
        return connection::startup_failure(&mut control, error).await;
    }
    connection::connection(
        control,
        CommandReader::new(BufReader::new(input)),
        output,
        async move {
            interrupt.recv().await;
        },
    )
    .await
}

fn fields(value: &Value, allowed: &[&str]) -> Result<()> {
    for field in value.as_object().expect("validated RPC object").keys() {
        ensure!(
            allowed.contains(&field.as_str()),
            "unexpected field: {field}"
        );
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{field} must be a string"))
}
fn success(id: Option<Value>, command: &str, data: Value) -> Value {
    let mut record = json!({"type":"response","command":command,"success":true,"data":data});
    if let Some(id) = id {
        record["id"] = id;
    }
    record
}
fn failure(id: Option<Value>, command: &str, error: &str) -> Value {
    let mut record = json!({"type":"response","command":command,"success":false,"error":error});
    if let Some(id) = id {
        record["id"] = id;
    }
    record
}

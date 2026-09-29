//! Long-lived JSONL client of the shared host and coding loop.
use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ion_ai::{Content, Message, Role};
use ion_core::{CodingAgentEvent, CodingSession, CodingToolHost, ForkPoint, SteeringInbox};
use ion_host::{Host, Resources, Selection, SessionCatalog};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::{expand_input, preview_input, redact_image_payloads, write_json_record};

const MAX_COMMAND_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 4 * MAX_COMMAND_BYTES;

enum Input {
    Line(Vec<u8>),
    TooLarge,
    Eof,
}

enum Output {
    Record(Value),
    Done,
}

struct Active {
    stop: CancellationToken,
    steering: Arc<SteeringInbox>,
}

struct QueuedFollowUp {
    id: Option<Value>,
    input: Message,
    encoded_bytes: usize,
}

/// The only mutable control state. The Session and selected route are fixed
/// inside each spawned Turn, so idle commands cannot change a running Turn.
struct Control {
    host: Arc<Host>,
    catalog: SessionCatalog,
    session: Arc<CodingSession>,
    selected: Selection,
    resources: Resources,
    active: Option<Active>,
    follow_ups: VecDeque<QueuedFollowUp>,
    queued_bytes: usize,
    output: mpsc::Sender<Output>,
    external_tools: Option<Arc<dyn CodingToolHost>>,
}

pub async fn run(
    session: Arc<CodingSession>,
    selected: Selection,
    resources: Resources,
    catalog: SessionCatalog,
    host: Arc<Host>,
    external_tools: Option<Arc<dyn CodingToolHost>>,
) -> Result<()> {
    let (output, mut events) = mpsc::channel::<Output>(128);
    let mut control = Control {
        host,
        catalog,
        session,
        selected,
        resources,
        active: None,
        follow_ups: VecDeque::new(),
        queued_bytes: 0,
        output,
        external_tools,
    };
    write_json_record(
        &json!({"type":"ready","session":control.session_id(),"cwd":control.session.cwd()}),
    )?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut closing = false;
    loop {
        tokio::select! {
            line = read_command(&mut input), if !closing => {
                match line? {
                    Input::Line(line) => control.command(&line)?,
                    Input::TooLarge => write_json_record(&failure(None, "parse", "command exceeds 8 MiB"))?,
                    Input::Eof => {
                        closing = true;
                        if let Some(active) = &control.active { active.stop.cancel(); }
                    }
                }
            }
            Some(output) = events.recv(), if control.active.is_some() => {
                match output {
                    Output::Record(record) => write_json_record(&record)?,
                    Output::Done => {
                        control.active = None;
                        if !closing { control.start_next_follow_up()?; }
                    }
                }
            }
        }
        if closing && control.active.is_none() {
            control.return_uncommitted_follow_ups()?;
            break;
        }
    }
    Ok(())
}

async fn read_command<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Input> {
    let mut line = Vec::new();
    let mut too_large = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if line.is_empty() && !too_large {
                Ok(Input::Eof)
            } else {
                Ok(Input::TooLarge) // Incomplete trailing records are never executed.
            };
        }
        let end = chunk.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(chunk.len(), |index| index + 1);
        if !too_large {
            if line.len() + count > MAX_COMMAND_BYTES {
                too_large = true;
                line.clear();
            } else {
                line.extend_from_slice(&chunk[..count]);
            }
        }
        reader.consume(count);
        if end.is_some() {
            if too_large {
                return Ok(Input::TooLarge);
            }
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Input::Line(line));
        }
    }
}

impl Control {
    fn session_id(&self) -> String {
        self.session
            .path()
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    fn idle(&self) -> Result<()> {
        ensure!(
            self.active.is_none(),
            "a Turn is active; abort or wait for turn_end"
        );
        Ok(())
    }

    fn command(&mut self, line: &[u8]) -> Result<()> {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => {
                write_json_record(&failure(None, "parse", &error.to_string()))?;
                return Ok(());
            }
        };
        let id = value.get("id").cloned();
        let command = value.get("type").and_then(Value::as_str).unwrap_or("parse");
        if !value.is_object() || id.as_ref().is_some_and(|id| !id.is_string()) {
            write_json_record(&failure(
                None,
                "parse",
                "command must be an object with an optional string id",
            ))?;
            return Ok(());
        }
        if command == "prompt" {
            if let Err(error) = self.prompt(&value, id.clone()) {
                write_json_record(&failure(id, command, &format!("{error:#}")))?;
            }
            return Ok(());
        }
        let outcome: Result<Value> = (|| {
            match command {
                "steer" => {
                    let active = self.active.as_ref().context("no active Turn")?;
                    let message = required_string(&value, "message")?;
                    let prompt = expand_input(&self.resources, message.to_owned())?;
                    let images = self.load_images(&value)?;
                    ensure!(!prompt.trim().is_empty() || !images.is_empty(), "message is empty");
                    active.steering.push_message(input_with_images(prompt, images))?;
                    Ok(json!({"disposition":"queued"}))
                }
                "follow_up" => {
                    ensure!(self.active.is_some(), "no active Turn; use prompt instead");
                    let message = required_string(&value, "message")?;
                    let prompt = expand_input(&self.resources, message.to_owned())?;
                    let images = self.load_images(&value)?;
                    ensure!(!prompt.trim().is_empty() || !images.is_empty(), "message is empty");
                    let input = input_with_images(prompt, images);
                    let encoded_bytes = serde_json::to_vec(&(&id, &input))?.len();
                    ensure!(
                        self.queued_bytes.checked_add(encoded_bytes).is_some_and(|total| total <= MAX_QUEUED_BYTES),
                        "queued follow-ups exceed the 32 MiB process bound"
                    );
                    self.follow_ups.push_back(QueuedFollowUp { id: id.clone(), input, encoded_bytes });
                    self.queued_bytes += encoded_bytes;
                    Ok(json!({"disposition":"queued","position":self.follow_ups.len()}))
                }
                "clear_queue" => {
                    let steering = self.active.as_ref().map_or_else(Vec::new, |active| active.steering.take_uncommitted());
                    let follow_up = self.follow_ups.drain(..).map(|pending| json!({"id":pending.id,"input":pending.input})).collect::<Vec<_>>();
                    self.queued_bytes = 0;
                    Ok(json!({"steering":steering,"follow_up":follow_up}))
                }
                "abort" => {
                    let active = self.active.as_ref().context("no active Turn")?;
                    active.stop.cancel();
                    Ok(json!({"disposition":"requested"}))
                }
                "get_state" => {
                    let view = self.session.view()?;
                    Ok(json!({"session":self.session_id(),"cwd":view.cwd,"name":view.name,"model":self.selected.identity(),"busy":self.active.is_some(),"entries":view.entries.len(),"follow_ups":self.follow_ups.len()}))
                }
                "inspect" => {
                    let mut view = serde_json::to_value(self.session.view()?)?;
                    redact_image_payloads(&mut view);
                    Ok(view)
                }
                "list_sessions" => Ok(json!(self.catalog.list()?.iter().map(|item| json!({"id":item.id,"name":item.name,"preview":item.preview,"turns":item.turns,"model":item.model})).collect::<Vec<_>>())),
                "list_turns" => Ok(json!(self.session.view()?.turns().iter().map(|item| json!({"turn":item.turn,"preview":preview_input(&item.input),"ended":item.end.is_some()})).collect::<Vec<_>>())),
                "list_models" => Ok(json!(self.host.models().choices(self.host.credentials())?.iter().map(|item| json!({"provider":item.selected.provider,"model":item.selected.model,"label":item.label,"image_input":item.selected.image_input})).collect::<Vec<_>>())),
                "list_resources" => Ok(json!({"skills":self.resources.skills().map(|item| json!({"name":item.name,"description":item.description})).collect::<Vec<_>>(),"prompts":self.resources.templates().map(|item| json!({"name":item.name,"description":item.description})).collect::<Vec<_>>(),"diagnostics":self.resources.diagnostics().iter().map(|item| json!({"path":item.path,"message":item.message})).collect::<Vec<_>>()})),
                "reload_resources" => {
                    self.idle()?;
                    self.resources = self.host.resources(self.session.cwd())?;
                    Ok(json!({"skills":self.resources.skills().count(),"prompts":self.resources.templates().count()}))
                }
                "set_model" => {
                    self.idle()?;
                    let provider = required_string(&value, "provider")?;
                    let model = required_string(&value, "model")?;
                    let selected = self.host.models().choose(Some(provider.to_owned()), Some(model.to_owned()), None, self.host.credentials())?;
                    selected.require_access(self.host.credentials())?;
                    self.selected = selected;
                    Ok(json!({"model":self.selected.identity()}))
                }
                "new_session" => {
                    self.idle()?;
                    let path = self.catalog.new_path()?;
                    self.session = Arc::new(CodingSession::create(&path, self.session.cwd())?);
                    Ok(json!({"session":self.session_id()}))
                }
                "fork" => {
                    self.idle()?;
                    let turn = value.get("turn").and_then(Value::as_u64).context("turn must be an unsigned integer")?;
                    let after = value.get("after").and_then(Value::as_bool).unwrap_or(false);
                    let model = self.session.view()?.turns().into_iter().find(|item| item.turn == turn).map(|item| item.model).context("selected Turn does not exist")?;
                    let selected = self.host.models().choose(None, None, Some(model), self.host.credentials())?;
                    selected.require_access(self.host.credentials())?;
                    let path = self.catalog.new_path()?;
                    let fork = self.session.fork_to(&path, if after { ForkPoint::AfterTurn(turn) } else { ForkPoint::BeforeTurn(turn) })?;
                    self.session = Arc::new(fork);
                    self.selected = selected;
                    Ok(json!({"session":self.session_id(),"model":self.selected.identity()}))
                }
                "switch_session" => {
                    self.idle()?;
                    let path = self.catalog.resolve_explicit(PathBuf::from(required_string(&value, "session")?))?;
                    let view = CodingSession::inspect(&path)?;
                    ensure!(view.cwd == self.session.cwd(), "session belongs to another working directory");
                    let selected = self.host.models().choose(None, None, view.last_model, self.host.credentials())?;
                    selected.require_access(self.host.credentials())?;
                    self.session = Arc::new(CodingSession::open(path)?);
                    self.selected = selected;
                    self.resources = self.host.resources(self.session.cwd())?;
                    Ok(json!({"session":self.session_id(),"model":self.selected.identity()}))
                }
                "set_name" => {
                    self.idle()?;
                    let name = value.get("name").and_then(Value::as_str);
                    self.session.set_name(name)?;
                    Ok(json!({"name":name}))
                }
                _ => bail!("unknown command: {command}"),
            }
        })();
        write_json_record(&match outcome {
            Ok(data) => success(id, command, data),
            Err(error) => failure(id, command, &format!("{error:#}")),
        })?;
        Ok(())
    }

    fn load_images(&self, value: &Value) -> Result<Vec<ion_host::image_input::LoadedImage>> {
        value.get("images").map_or(Ok(Vec::new()), |images| {
            images
                .as_array()
                .context("images must be an array of local paths or inline images")?
                .iter()
                .map(|image| {
                    if let Some(path) = image.as_str() {
                        let path = PathBuf::from(path);
                        let path = if path.is_absolute() {
                            path
                        } else {
                            self.session.cwd().join(path)
                        };
                        ion_host::image_input::load_image(&self.selected, &path)
                    } else {
                        let mime_type = required_string(image, "mime_type")?;
                        let data = required_string(image, "data")?;
                        ion_host::image_input::load_encoded_image(&self.selected, mime_type, data)
                    }
                })
                .collect::<Result<Vec<_>>>()
        })
    }

    fn prompt(&mut self, value: &Value, id: Option<Value>) -> Result<()> {
        self.idle()?;
        let prompt = required_string(value, "message")?;
        let prompt = expand_input(&self.resources, prompt.to_owned())?;
        let images = self.load_images(value)?;
        ensure!(
            !prompt.trim().is_empty() || !images.is_empty(),
            "message is empty"
        );
        self.start_message(input_with_images(prompt, images), id, false)
    }

    fn start_next_follow_up(&mut self) -> Result<()> {
        while let Some(pending) = self.follow_ups.pop_front() {
            self.queued_bytes -= pending.encoded_bytes;
            let id = pending.id;
            let input = pending.input;
            match self.start_message(input.clone(), id.clone(), true) {
                Ok(()) => return Ok(()),
                Err(error) => write_json_record(&json!({
                    "type":"follow_up_failed","id":id,"input":input,"error":format!("{error:#}")
                }))?,
            }
        }
        Ok(())
    }

    fn return_uncommitted_follow_ups(&mut self) -> Result<()> {
        for pending in self.follow_ups.drain(..) {
            write_json_record(&json!({
                "type":"uncommitted_follow_up","id":pending.id,"input":pending.input
            }))?;
        }
        self.queued_bytes = 0;
        Ok(())
    }

    fn start_message(&mut self, input: Message, id: Option<Value>, queued: bool) -> Result<()> {
        self.idle()?;
        let agent = self.host.agent_with_optional_tools(
            &self.session,
            &self.selected,
            self.external_tools.clone(),
        )?;
        let instructions = self.resources.instructions().to_owned();
        let model = self.selected.identity();
        let session = self.session.clone();
        let stop = CancellationToken::new();
        let steering = Arc::new(SteeringInbox::default());
        let output = self.output.clone();
        let task_stop = stop.clone();
        let task_steering = steering.clone();
        let recover_input = queued.then(|| input.clone());
        self.active = Some(Active { stop, steering });
        tokio::spawn(async move {
            let mut accepted = None;
            let mut output_fault = None;
            let result = agent
                .submit_message_with_steering(
                    &session,
                    model,
                    input,
                    instructions,
                    task_stop.clone(),
                    &task_steering,
                    |event| match event {
                        CodingAgentEvent::TurnAccepted { turn } => {
                            accepted = Some(turn);
                            let record = if queued {
                                json!({"type":"follow_up_started","id":id,"turn":turn})
                            } else {
                                success(
                                    id.clone(),
                                    "prompt",
                                    json!({"disposition":"started","turn":turn}),
                                )
                            };
                            if output.try_send(Output::Record(record)).is_err() {
                                output_fault = Some("RPC output queue is full".to_owned());
                                task_stop.cancel();
                            }
                        }
                        event => {
                            if let Some(turn) = accepted {
                                let mut record = event_record(event);
                                record["turn"] = json!(turn);
                                if output.try_send(Output::Record(record)).is_err() {
                                    output_fault = Some("RPC output queue is full".to_owned());
                                    task_stop.cancel();
                                }
                            }
                        }
                    },
                )
                .await;
            if let Some(turn) = accepted {
                let status = match &result {
                    Ok(_) => "completed",
                    Err(ion_core::CodingAgentError::Cancelled) => "cancelled",
                    Err(_) => "failed",
                };
                let mut record = json!({"type":"turn_end","turn":turn,"status":status});
                if let Err(error) = &result {
                    record["error"] = json!(error.to_string());
                }
                if let Some(fault) = output_fault {
                    record["output_error"] = json!(fault);
                }
                let _ = output.send(Output::Record(record)).await;
            } else {
                let error = result.err().map_or_else(
                    || "Turn was not accepted".to_owned(),
                    |error| error.to_string(),
                );
                let record = if queued {
                    json!({"type":"follow_up_failed","id":id,"input":recover_input,"error":error})
                } else {
                    failure(id, "prompt", &error)
                };
                let _ = output.send(Output::Record(record)).await;
            }
            for pending in task_steering.take_uncommitted() {
                let _ = output
                    .send(Output::Record(
                        json!({"type":"uncommitted_steering","input":pending}),
                    ))
                    .await;
            }
            let _ = output.send(Output::Done).await;
        });
        Ok(())
    }
}

fn input_with_images(prompt: String, images: Vec<ion_host::image_input::LoadedImage>) -> Message {
    Message {
        role: Role::User,
        content: std::iter::once(Content::Text(prompt))
            .chain(
                images
                    .into_iter()
                    .flat_map(ion_host::image_input::LoadedImage::into_parts),
            )
            .collect(),
        provider_replay: None,
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let text = value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{field} must be a string"))?;
    Ok(text)
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

pub(super) fn event_record(event: CodingAgentEvent) -> Value {
    match event {
        CodingAgentEvent::TurnAccepted { turn } => json!({"type":"turn_accepted","turn":turn}),
        CodingAgentEvent::TextDelta(text) => json!({"type":"text_delta","text":text}),
        CodingAgentEvent::ProviderRetry {
            attempt,
            max_retries,
            delay_ms,
        } => {
            json!({"type":"provider_retry","attempt":attempt,"max_retries":max_retries,"delay_ms":delay_ms})
        }
        CodingAgentEvent::ToolStarted {
            call_id,
            name,
            arguments,
        } => json!({"type":"tool_started","call_id":call_id,"name":name,"arguments":arguments}),
        CodingAgentEvent::ToolFinished {
            call_id,
            name,
            output,
        } => {
            json!({"type":"tool_finished","call_id":call_id,"name":name,"output":output.value,"image_mime_types":output.images.iter().map(|image| image.mime_type().as_str()).collect::<Vec<_>>(),"is_error":output.is_error})
        }
        CodingAgentEvent::ToolRejected {
            call_id,
            name,
            output,
        } => {
            json!({"type":"tool_rejected","call_id":call_id,"name":name,"output":output.value,"is_error":output.is_error})
        }
        CodingAgentEvent::InterruptedCalls(count) => {
            json!({"type":"interrupted_calls","count":count})
        }
        CodingAgentEvent::ContextCompacted { through_entry } => {
            json!({"type":"context_compacted","through_entry":through_entry})
        }
        CodingAgentEvent::ResponseRestarted => json!({"type":"response_restarted"}),
        CodingAgentEvent::Final(text) => json!({"type":"final","text":text}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framing_is_lf_only_and_recovers_after_oversize() {
        let mut input = Vec::new();
        input.extend_from_slice(b"{\"message\":\"a\xE2\x80\xA8b\"}\r\n");
        input.extend(std::iter::repeat_n(b'x', MAX_COMMAND_BYTES + 1));
        input.extend_from_slice(b"\n{}\n");
        let mut reader = BufReader::new(input.as_slice());
        assert!(matches!(
            read_command(&mut reader).await.unwrap(),
            Input::Line(_)
        ));
        assert!(matches!(
            read_command(&mut reader).await.unwrap(),
            Input::TooLarge
        ));
        assert!(
            matches!(read_command(&mut reader).await.unwrap(), Input::Line(line) if line == b"{}")
        );
    }
}

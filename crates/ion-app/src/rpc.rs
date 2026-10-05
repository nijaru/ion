//! Long-lived JSONL client of the shared host and coding loop.
use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ion_ai::Message;
use ion_core::{CodingAgentEvent, ForkPoint, SteeringInbox};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, BufReader},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    agent_events::event_record, expand_input, preview_input, redact_image_payloads,
    write_json_record,
};

mod input;
use input::{CommandReader, Input, MAX_COMMAND_BYTES};

const MAX_QUEUED_BYTES: usize = 4 * MAX_COMMAND_BYTES;

struct Active {
    stop: CancellationToken,
    /// Present only for an active coding Turn. Other cancellable operations
    /// such as manual compaction do not accept steering or follow-ups.
    steering: Option<Arc<SteeringInbox>>,
    task: JoinHandle<Vec<Value>>,
}

struct QueuedFollowUp {
    id: Option<Value>,
    input: Message,
    encoded_bytes: usize,
}

/// The only mutable control state. The Session and selected route are fixed
/// inside each spawned Turn, so idle commands cannot change a running Turn.
struct Control {
    binding: ion_host::SessionBinding,
    active: Option<Active>,
    follow_ups: VecDeque<QueuedFollowUp>,
    queued_bytes: usize,
    output: mpsc::Sender<Value>,
}

pub async fn run(binding: ion_host::SessionBinding) -> Result<()> {
    let (output, events) = mpsc::channel(128);
    let control = Control {
        binding,
        active: None,
        follow_ups: VecDeque::new(),
        queued_bytes: 0,
        output,
    };
    write_json_record(
        &json!({"type":"ready","session":control.session_id(),"cwd":control.binding.session().cwd()}),
    )?;
    connection(
        control,
        CommandReader::new(BufReader::new(tokio::io::stdin())),
        events,
    )
    .await
}

/// Connection exit owns cancellation and joining, including input/output faults.
/// Closing the progress receiver makes further publication fail immediately;
/// the operation still settles its owned work before the connection returns.
async fn connection<R: AsyncBufRead + Unpin>(
    mut control: Control,
    mut input: CommandReader<R>,
    mut events: mpsc::Receiver<Value>,
) -> Result<()> {
    let result: Result<()> = async {
        let mut closing = false;
        loop {
            let busy = control.active.is_some();
            tokio::select! {
                Some(record) = events.recv(), if busy => {
                    write_json_record(&record)?;
                }
                joined = async { (&mut control.active.as_mut().expect("active operation").task).await }, if busy => {
                    // A polled-complete JoinHandle must not be awaited again during
                    // error cleanup. Its result is now owned by this branch.
                    control.active = None;
                    let terminal = joined.context("RPC operation task failed; unfinished effects remain unknown")?;
                    // The task has stopped producing. Publish pending progress,
                    // then completion, before admitting follow-ups or idle state.
                    while let Ok(record) = events.try_recv() {
                        write_json_record(&record)?;
                    }
                    for record in terminal { write_json_record(&record)?; }
                    if !closing { control.start_next_follow_up()?; }
                }
                line = input.next(), if !closing => {
                    match line? {
                        Input::Line(line) => control.command(&line)?,
                        Input::TooLarge => write_json_record(&failure(None, "parse", "command exceeds 8 MiB"))?,
                        Input::Incomplete => write_json_record(&failure(None, "parse", "command is missing its final newline"))?,
                        Input::Eof => {
                            closing = true;
                            if let Some(active) = &control.active { active.stop.cancel(); }
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
    }.await;
    events.close();
    let settlement = if let Some(active) = control.active.take() {
        active.stop.cancel();
        active
            .task
            .await
            .context("RPC operation failed during connection settlement")
            .map(|_| ())
    } else {
        Ok(())
    };
    match (result, settlement) {
        (Err(error), Err(settlement)) => {
            Err(error.context(format!("settlement also failed: {settlement:#}")))
        }
        (Err(error), _) => Err(error),
        (Ok(()), settlement) => settlement,
    }
}

impl Control {
    fn session_id(&self) -> String {
        self.binding.session_id()
    }

    fn idle(&self) -> Result<()> {
        ensure!(
            self.active.is_none(),
            "an operation is active; abort or wait for its terminal record"
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
                    let active = self.active.as_ref().context("no active operation")?;
                    let steering = active.steering.as_ref().context("no active Turn")?;
                    let message = required_string(&value, "message")?;
                    let prompt = expand_input(self.binding.resources(), message.to_owned())?;
                    let images = self.load_images(&value)?;
                    ensure!(!prompt.trim().is_empty() || !images.is_empty(), "message is empty");
                    steering.push_message(Message::user_input(prompt, images))?;
                    Ok(json!({"disposition":"queued"}))
                }
                "follow_up" => {
                    ensure!(
                        self.active
                            .as_ref()
                            .is_some_and(|active| active.steering.is_some()),
                        "no active Turn; use prompt instead"
                    );
                    let message = required_string(&value, "message")?;
                    let prompt = expand_input(self.binding.resources(), message.to_owned())?;
                    let images = self.load_images(&value)?;
                    ensure!(!prompt.trim().is_empty() || !images.is_empty(), "message is empty");
                    let input = Message::user_input(prompt, images);
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
                    let steering = self
                        .active
                        .as_ref()
                        .and_then(|active| active.steering.as_ref())
                        .map_or_else(Vec::new, |steering| steering.take_uncommitted());
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
                    let view = self.binding.session().view()?;
                    let operation = self.active.as_ref().map(|active| {
                        if active.steering.is_some() { "turn" } else { "compact" }
                    });
                    Ok(json!({"session":self.session_id(),"cwd":view.cwd,"name":view.name,"model":self.binding.selected().identity(),"busy":self.active.is_some(),"operation":operation,"entries":view.entries.len(),"follow_ups":self.follow_ups.len()}))
                }
                "inspect" => {
                    let mut view = serde_json::to_value(self.binding.session().view()?)?;
                    redact_image_payloads(&mut view);
                    Ok(view)
                }
                "list_sessions" => Ok(json!(self.binding.catalog().list()?.iter().map(|item| json!({"id":item.id,"name":item.name,"preview":item.preview,"turns":item.turns,"model":item.model})).collect::<Vec<_>>())),
                "list_turns" => Ok(json!(self.binding.session().view()?.turns().iter().map(|item| json!({"turn":item.turn,"preview":preview_input(&item.input),"ended":item.end.is_some()})).collect::<Vec<_>>())),
                "list_models" => Ok(json!(self.binding.host().models().choices(self.binding.host().credentials())?.iter().map(|item| json!({"provider":item.selected.provider,"model":item.selected.model,"label":item.label,"image_input":item.selected.image_input})).collect::<Vec<_>>())),
                "list_resources" => Ok(json!({"skills":self.binding.resources().skills().map(|item| json!({"name":item.name,"description":item.description})).collect::<Vec<_>>(),"prompts":self.binding.resources().templates().map(|item| json!({"name":item.name,"description":item.description})).collect::<Vec<_>>(),"diagnostics":self.binding.resources().diagnostics().iter().map(|item| json!({"path":item.path,"message":item.message})).collect::<Vec<_>>()})),
                "reload_resources" => {
                    self.idle()?;
                    self.binding.reload_resources()?;
                    Ok(json!({"skills":self.binding.resources().skills().count(),"prompts":self.binding.resources().templates().count()}))
                }
                "compact" => self.start_compaction(id.clone()),
                "set_model" => {
                    self.idle()?;
                    let provider = required_string(&value, "provider")?;
                    let model = required_string(&value, "model")?;
                    self.binding.select_model(ion_ai::ModelRef { provider: provider.to_owned(), model: model.to_owned() })?;
                    Ok(json!({"model":self.binding.selected().identity()}))
                }
                "new_session" => {
                    self.idle()?;
                    self.binding.new_session()?;
                    Ok(json!({"session":self.session_id(),"model":self.binding.selected().identity()}))
                }
                "clone_session" => {
                    self.idle()?;
                    let session = self.binding.clone_session()?;
                    Ok(json!({"session":session,"model":self.binding.selected().identity()}))
                }
                "fork" => {
                    self.idle()?;
                    let turn = value.get("turn").and_then(Value::as_u64).context("turn must be an unsigned integer")?;
                    let after = value.get("after").and_then(Value::as_bool).unwrap_or(false);
                    self.binding.fork_session(if after { ForkPoint::AfterTurn(turn) } else { ForkPoint::BeforeTurn(turn) })?;
                    Ok(json!({"session":self.session_id(),"model":self.binding.selected().identity()}))
                }
                "switch_session" => {
                    self.idle()?;
                    self.binding.switch_session(PathBuf::from(required_string(&value, "session")?))?;
                    Ok(json!({"session":self.session_id(),"model":self.binding.selected().identity()}))
                }
                "set_name" => {
                    self.idle()?;
                    let name = value.get("name").and_then(Value::as_str);
                    self.binding.session().set_name(name)?;
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
                            self.binding.session().cwd().join(path)
                        };
                        ion_host::image_input::load_image(self.binding.selected(), &path)
                    } else {
                        let mime_type = required_string(image, "mime_type")?;
                        let data = required_string(image, "data")?;
                        ion_host::image_input::load_encoded_image(
                            self.binding.selected(),
                            mime_type,
                            data,
                        )
                    }
                })
                .collect::<Result<Vec<_>>>()
        })
    }

    fn prompt(&mut self, value: &Value, id: Option<Value>) -> Result<()> {
        self.idle()?;
        let prompt = required_string(value, "message")?;
        let prompt = expand_input(self.binding.resources(), prompt.to_owned())?;
        let images = self.load_images(value)?;
        ensure!(
            !prompt.trim().is_empty() || !images.is_empty(),
            "message is empty"
        );
        self.start_message(Message::user_input(prompt, images), id, false)
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

    fn start_compaction(&mut self, id: Option<Value>) -> Result<Value> {
        self.idle()?;
        let agent = self.binding.agent().clone();
        let session = self.binding.session().clone();
        let model = self.binding.selected().identity();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let output = self.output.clone();
        let task = tokio::spawn(async move {
            let mut output_fault = None;
            let result = agent
                .compact(&session, model, task_stop.clone(), |event| {
                    if output.try_send(event_record(event)).is_err() {
                        output_fault = Some("RPC progress queue is unavailable".to_owned());
                        task_stop.cancel();
                    }
                })
                .await;
            let status = match &result {
                Ok(_) => "completed",
                Err(ion_core::CodingAgentError::Cancelled) => "cancelled",
                Err(_) => "failed",
            };
            let mut record = json!({"type":"compact_end","id":id,"status":status});
            if let Ok(changed) = result {
                record["changed"] = json!(changed);
            } else if let Err(error) = result {
                record["error"] = json!(error.to_string());
            }
            if let Some(fault) = output_fault {
                record["output_error"] = json!(fault);
            }
            vec![record]
        });
        self.active = Some(Active {
            stop,
            steering: None,
            task,
        });
        Ok(json!({"disposition":"started"}))
    }

    fn start_message(&mut self, input: Message, id: Option<Value>, queued: bool) -> Result<()> {
        self.idle()?;
        let agent = self.binding.agent().clone();
        let instructions = self.binding.resources().instructions().to_owned();
        let model = self.binding.selected().identity();
        let session = self.binding.session().clone();
        let stop = CancellationToken::new();
        let steering = Arc::new(SteeringInbox::default());
        let output = self.output.clone();
        let task_stop = stop.clone();
        let task_steering = steering.clone();
        let recover_input = queued.then(|| input.clone());
        let task = tokio::spawn(async move {
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
                            if output.try_send(record).is_err() {
                                output_fault = Some("RPC progress queue is unavailable".to_owned());
                                task_stop.cancel();
                            }
                        }
                        event => {
                            if let Some(turn) = accepted {
                                let mut record = event_record(event);
                                record["turn"] = json!(turn);
                                if output.try_send(record).is_err() {
                                    output_fault =
                                        Some("RPC progress queue is unavailable".to_owned());
                                    task_stop.cancel();
                                }
                            }
                        }
                    },
                )
                .await;
            let terminal = if let Some(turn) = accepted {
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
                record
            } else {
                let error = result.err().map_or_else(
                    || "Turn was not accepted".to_owned(),
                    |error| error.to_string(),
                );
                if queued {
                    json!({"type":"follow_up_failed","id":id,"input":recover_input,"error":error})
                } else {
                    failure(id, "prompt", &error)
                }
            };
            let mut terminal = vec![terminal];
            terminal.extend(
                task_steering
                    .take_uncommitted()
                    .into_iter()
                    .map(|pending| json!({"type":"uncommitted_steering","input":pending})),
            );
            terminal
        });
        self.active = Some(Active {
            stop,
            steering: Some(steering),
            task,
        });
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Control, mpsc::Receiver<Value>, PathBuf) {
        use std::fs;

        use ion_core::CodingSession;
        use ion_host::{Host, SavedSelection, SessionBinding, Wire};

        let root = std::env::temp_dir().join(format!("ion-rpc-{}", uuid::Uuid::now_v7()));
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let selected = host
            .models()
            .save_default(&SavedSelection {
                provider: "desktop".into(),
                model: "test".into(),
                endpoint: Some("http://127.0.0.1:9/v1/chat/completions".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                image_input: false,
            })
            .unwrap();
        let session_path = host.sessions(cwd.clone()).new_path().unwrap();
        let session = Arc::new(CodingSession::create(&session_path, &cwd).unwrap());
        session.select_model(selected.identity()).unwrap();
        let binding = SessionBinding::new(host, session, selected, None).unwrap();
        let (output, events) = mpsc::channel(16);
        let control = Control {
            binding,
            active: None,
            follow_ups: VecDeque::new(),
            queued_bytes: 0,
            output,
        };

        (control, events, root)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manual_compaction_uses_the_shared_active_operation_slot() {
        let (mut control, _events, root) = fixture();

        assert_eq!(
            control.start_compaction(Some(json!("cancelled"))).unwrap(),
            json!({"disposition":"started"})
        );
        assert!(control.idle().is_err());
        let active = control.active.as_ref().unwrap();
        assert!(active.steering.is_none());
        active.stop.cancel();

        let terminal = control.active.take().unwrap().task.await.unwrap();
        let cancelled = &terminal[0];
        assert_eq!(cancelled["type"], "compact_end");
        assert_eq!(cancelled["id"], "cancelled");
        assert_eq!(cancelled["status"], "cancelled");

        assert_eq!(
            control.start_compaction(Some(json!("noop"))).unwrap(),
            json!({"disposition":"started"})
        );
        let terminal = control.active.take().unwrap().task.await.unwrap();
        let completed = &terminal[0];
        assert_eq!(completed["type"], "compact_end");
        assert_eq!(completed["id"], "noop");
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["changed"], false);

        drop(control);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn input_failure_waits_for_owned_operation_and_releases_sends() {
        use std::{
            pin::Pin,
            sync::atomic::{AtomicBool, Ordering},
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, ReadBuf};

        struct FailedInput;
        impl AsyncRead for FailedInput {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Err(std::io::Error::other("injected RPC input failure")))
            }
        }
        let (mut control, events, root) = fixture();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let output = control.output.clone();
        let settled = Arc::new(AtomicBool::new(false));
        let task_settled = settled.clone();
        let task = tokio::spawn(async move {
            task_stop.cancelled().await;
            assert!(output.send(json!({"type":"terminal"})).await.is_err());
            task_settled.store(true, Ordering::SeqCst);
            Vec::new()
        });
        control.active = Some(Active {
            stop,
            steering: None,
            task,
        });
        let error = connection(
            control,
            CommandReader::new(BufReader::new(FailedInput)),
            events,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected RPC input failure"));
        assert!(
            settled.load(Ordering::SeqCst),
            "connection returned before operation settlement"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn operation_panic_ends_connection_instead_of_stranding_busy_state() {
        let (mut control, events, root) = fixture();
        control.active = Some(Active {
            stop: CancellationToken::new(),
            steering: None,
            task: tokio::spawn(async { panic!("injected RPC operation panic") }),
        });
        let (input, _open_client) = tokio::io::duplex(64);
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connection(control, CommandReader::new(BufReader::new(input)), events),
        )
        .await
        .expect("RPC stranded its active slot after panic")
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected RPC operation panic"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

//! Long-lived JSONL client of the shared host and coding loop.
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ion_ai::Message;
use ion_core::{
    AcceptedInput, CodingAgentEvent, ForkPoint, InputBudget, InputReservation, SteeringInbox,
};
use serde_json::{Value, json};
use tokio::{io::BufReader, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    agent_events::event_record, expand_input, preview_input, redact_image_payloads,
    write_json_record,
};

mod connection;
mod input;
use connection::connection;
use input::CommandReader;

struct Active {
    stop: CancellationToken,
    operation: Operation,
    task: JoinHandle<Vec<Value>>,
}

enum Operation {
    Turn {
        steering: Arc<SteeringInbox>,
        submission: Arc<Submission>,
    },
    Compact {
        id: Option<Value>,
    },
    Shell {
        id: Option<Value>,
    },
}

impl Operation {
    fn name(&self) -> &'static str {
        match self {
            Self::Turn { .. } => "turn",
            Self::Compact { .. } => "compact",
            Self::Shell { .. } => "shell",
        }
    }

    fn id(&self) -> &Option<Value> {
        match self {
            Self::Turn { submission, .. } => &submission.id,
            Self::Compact { id } | Self::Shell { id } => id,
        }
    }

    fn steering(&self) -> Option<&Arc<SteeringInbox>> {
        match self {
            Self::Turn { steering, .. } => Some(steering),
            _ => None,
        }
    }
}

/// Live custody survives task failure. Admission is the explicit Core event,
/// not an inference from a later Session read.
struct Submission {
    id: Option<Value>,
    admission: Mutex<Admission>,
}

struct Admission {
    turn: Option<u64>,
    pending: Option<(Message, InputReservation)>,
}

impl Submission {
    fn accept(&self, turn: u64) {
        let pending = {
            let mut admission = self.admission.lock().expect("admission fact mutex");
            admission.turn = Some(turn);
            admission.pending.take()
        };
        drop(pending);
    }

    fn turn(&self) -> Option<u64> {
        self.admission.lock().expect("admission fact mutex").turn
    }

    fn recover(&self, error: &str) -> Option<Value> {
        let admission = self.admission.lock().expect("admission fact mutex");
        admission.pending.as_ref().map(
            |(input, _reservation)| json!({"type":"follow_up_failed","id":self.id,"input":input,"error":error}),
        )
    }
}

struct Completion {
    records: Vec<Value>,
    error: Option<anyhow::Error>,
}

/// The only mutable control state. The Session and selected route are fixed
/// inside each spawned Turn, so idle commands cannot change a running Turn.
struct Control {
    binding: ion_host::SessionBinding,
    active: Option<Active>,
    follow_ups: VecDeque<AcceptedInput<Option<Value>>>,
    input_budget: InputBudget,
    output: mpsc::Sender<Value>,
}

pub async fn run(binding: ion_host::SessionBinding) -> Result<()> {
    let (output, events) = mpsc::channel(128);
    let control = Control {
        binding,
        active: None,
        follow_ups: VecDeque::new(),
        input_budget: InputBudget::default(),
        output,
    };
    write_json_record(
        &json!({"type":"ready","session":control.session_id(),"cwd":control.binding.session().cwd()}),
    )?;
    connection(
        control,
        CommandReader::new(BufReader::new(tokio::io::stdin())),
        events,
        std::io::stdout(),
    )
    .await
}

impl Control {
    fn finish_operation(
        &mut self,
        joined: std::result::Result<Vec<Value>, tokio::task::JoinError>,
    ) -> Completion {
        // The connection joins the occupied slot, then consumes it here.
        let active = self.active.take().expect("joined active operation");
        let (mut records, error) = match joined {
            Ok(records) => (records, None),
            Err(error) => {
                let error = anyhow::Error::new(error)
                    .context("RPC operation task failed; unfinished effects remain unknown");
                let mut record = json!({"type":"operation_failed","operation":active.operation.name(),"id":active.operation.id(),"error":format!("{error:#}")});
                if let Operation::Turn { submission, .. } = &active.operation
                    && let Some(turn) = submission.turn()
                {
                    record["turn"] = json!(turn);
                }
                let mut records = vec![record];
                if let Operation::Turn { submission, .. } = &active.operation
                    && let Some(recovery) = submission.recover(&format!("{error:#}"))
                {
                    records.push(recovery);
                }
                (records, Some(error))
            }
        };
        if let Operation::Turn { steering, .. } = active.operation {
            records.extend(steering.take_uncommitted().into_iter().map(
                |pending| json!({"type":"uncommitted_steering","input":pending.into_message()}),
            ));
        }
        Completion { records, error }
    }

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

    fn command(&mut self, line: &[u8]) -> Option<Value> {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => return Some(failure(None, "parse", &error.to_string())),
        };
        let id = value.get("id").cloned();
        let command = value.get("type").and_then(Value::as_str).unwrap_or("parse");
        if !value.is_object() || id.as_ref().is_some_and(|id| !id.is_string()) {
            return Some(failure(
                None,
                "parse",
                "command must be an object with an optional string id",
            ));
        }
        if command == "prompt" {
            return self
                .prompt(&value, id.clone())
                .err()
                .map(|error| failure(id, command, &format!("{error:#}")));
        }
        let outcome: Result<Value> = (|| {
            match command {
                "steer" => {
                    let active = self.active.as_ref().context("no active operation")?;
                    let steering = active.operation.steering().context("no active Turn")?;
                    let message = required_string(&value, "message")?;
                    let prompt = expand_input(self.binding.resources(), message.to_owned())?;
                    let images = self.load_images(&value)?;
                    ensure!(!prompt.trim().is_empty() || !images.is_empty(), "message is empty");
                    steering.push_message(Message::user_input(prompt, images), ())?;
                    Ok(json!({"disposition":"queued"}))
                }
                "follow_up" => {
                    ensure!(
                        self.active
                            .as_ref()
                            .is_some_and(|active| active.operation.steering().is_some()),
                        "no active Turn; use prompt instead"
                    );
                    let message = required_string(&value, "message")?;
                    let prompt = expand_input(self.binding.resources(), message.to_owned())?;
                    let images = self.load_images(&value)?;
                    ensure!(!prompt.trim().is_empty() || !images.is_empty(), "message is empty");
                    let input = self.input_budget.admit(
                        Message::user_input(prompt, images),
                        id.clone(),
                        self.binding.agent().limits(),
                    )?;
                    self.follow_ups.push_back(input);
                    Ok(json!({"disposition":"queued","position":self.follow_ups.len()}))
                }
                "clear_queue" => {
                    let steering = self
                        .active
                        .as_ref()
                        .and_then(|active| active.operation.steering())
                        .map_or_else(Vec::new, |steering| steering.take_uncommitted())
                        .into_iter().map(AcceptedInput::into_message).collect::<Vec<_>>();
                    let follow_up = self.follow_ups.drain(..).map(|pending| {
                        let (input, id, _reservation) = pending.into_parts();
                        json!({"id":id,"input":input})
                    }).collect::<Vec<_>>();
                    Ok(json!({"steering":steering,"follow_up":follow_up}))
                }
                "abort" => {
                    let active = self.active.as_ref().context("no active operation")?;
                    active.stop.cancel();
                    Ok(json!({"disposition":"requested"}))
                }
                "get_state" => {
                    let view = self.binding.session().view()?;
                    let operation = self.active.as_ref().map(|active| active.operation.name());
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
                "shell" => {
                    let command = required_string(&value, "command")?;
                    ensure!(!command.trim().is_empty(), "command is empty");
                    let excluded = value.get("exclude_from_context").map_or(Ok(false), |value| {
                        value.as_bool().context("exclude_from_context must be a boolean")
                    })?;
                    self.start_shell(command, excluded, id.clone())
                }
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
        Some(match outcome {
            Ok(data) => success(id, command, data),
            Err(error) => failure(id, command, &format!("{error:#}")),
        })
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
        self.start_message(Message::user_input(prompt, images), id, None)
    }

    fn start_next_follow_up(&mut self) -> Result<()> {
        self.idle()?;
        if let Some(pending) = self.follow_ups.pop_front() {
            let (input, id, reservation) = pending.into_parts();
            self.start_message(input, id, Some(reservation))?;
        }
        Ok(())
    }

    fn take_uncommitted_follow_ups(&mut self) -> impl Iterator<Item = Value> + '_ {
        self.follow_ups.drain(..).map(|pending| {
            let (input, id, _reservation) = pending.into_parts();
            json!({"type":"uncommitted_follow_up","id":id,"input":input})
        })
    }

    fn start_shell(
        &mut self,
        command: &str,
        exclude_from_context: bool,
        id: Option<Value>,
    ) -> Result<Value> {
        self.idle()?;
        let stop = CancellationToken::new();
        let running = self
            .binding
            .run_user_shell(command, stop.clone(), exclude_from_context);
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            let record = match running.await {
                Ok(output) => {
                    let status = if output.value["cancelled"] == true {
                        "cancelled"
                    } else if output.is_error {
                        "failed"
                    } else {
                        "completed"
                    };
                    let outcome = ion_core::UserShellOutcome::Observed {
                        output: output.value,
                        is_error: output.is_error,
                    };
                    json!({"type":"shell_end","id":task_id,"status":status,"outcome":outcome})
                }
                // No observation claim on admission or storage failure. Passive
                // Session inspection distinguishes absent authority from unknown effects.
                Err(error) => {
                    json!({"type":"shell_end","id":task_id,"status":"failed","error":format!("{error:#}")})
                }
            };
            vec![record]
        });
        self.active = Some(Active {
            stop,
            operation: Operation::Shell { id },
            task,
        });
        Ok(json!({"disposition":"started"}))
    }

    fn start_compaction(&mut self, id: Option<Value>) -> Result<Value> {
        self.idle()?;
        let agent = self.binding.agent().clone();
        let session = self.binding.session().clone();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let output = self.output.clone();
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            let mut output_fault = None;
            let result = agent
                .compact(&session, task_stop.clone(), |event| {
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
            let mut record = json!({"type":"compact_end","id":task_id,"status":status});
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
            operation: Operation::Compact { id },
            task,
        });
        Ok(json!({"disposition":"started"}))
    }

    fn start_message(
        &mut self,
        input: Message,
        id: Option<Value>,
        reservation: Option<InputReservation>,
    ) -> Result<()> {
        let queued = reservation.is_some();
        self.idle()?;
        let agent = self.binding.agent().clone();
        let instructions = self.binding.resources().instructions().to_owned();
        let session = self.binding.session().clone();
        let stop = CancellationToken::new();
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            self.input_budget.clone(),
        ));
        let output = self.output.clone();
        let task_stop = stop.clone();
        let task_steering = steering.clone();
        let submission = Arc::new(Submission {
            id,
            admission: Mutex::new(Admission {
                turn: None,
                pending: reservation.map(|reservation| (input.clone(), reservation)),
            }),
        });
        let task_submission = submission.clone();
        let task = tokio::spawn(async move {
            let mut output_fault = None;
            let result = agent
                .submit_message_with_steering(
                    &session,
                    input,
                    instructions,
                    task_stop.clone(),
                    &task_steering,
                    |event| match event {
                        CodingAgentEvent::TurnAccepted { turn } => {
                            task_submission.accept(turn);
                            let record = if queued {
                                json!({"type":"follow_up_started","id":task_submission.id,"turn":turn})
                            } else {
                                success(
                                    task_submission.id.clone(),
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
                            if let Some(turn) = task_submission.turn() {
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
            let terminal = if let Some(turn) = task_submission.turn() {
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
                task_submission
                    .recover(&error)
                    .unwrap_or_else(|| failure(task_submission.id.clone(), "prompt", &error))
            };
            vec![terminal]
        });
        self.active = Some(Active {
            stop,
            operation: Operation::Turn {
                steering,
                submission,
            },
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
    use std::io::Write;

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
            input_budget: InputBudget::default(),
            output,
        };

        (control, events, root)
    }

    fn empty_submission() -> Arc<Submission> {
        Arc::new(Submission {
            id: None,
            admission: Mutex::new(Admission {
                turn: None,
                pending: None,
            }),
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn steering_accepted_after_task_completion_is_returned_at_join() {
        let (mut control, _events, root) = fixture();
        let steering = Arc::new(SteeringInbox::new(
            control.binding.agent().limits(),
            control.input_budget.clone(),
        ));
        control.active = Some(Active {
            stop: CancellationToken::new(),
            operation: Operation::Turn {
                steering,
                submission: empty_submission(),
            },
            task: tokio::spawn(async { vec![json!({"type":"turn_end","status":"completed"})] }),
        });
        // The task is complete but the connection has not closed admission yet.
        let joined = (&mut control.active.as_mut().unwrap().task).await;
        control
            .command(br#"{"id":"late","type":"steer","message":"retain me"}"#)
            .unwrap();
        let completion = control.finish_operation(joined);
        assert!(completion.error.is_none());
        let terminal = completion.records;
        assert!(control.active.is_none());
        assert_eq!(terminal[1]["type"], "uncommitted_steering");
        assert_eq!(terminal[1]["input"]["content"][0]["Text"], "retain me");
        drop(control);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn operation_panic_returns_uncommitted_steering() {
        let (mut control, _events, root) = fixture();
        let steering = Arc::new(SteeringInbox::new(
            control.binding.agent().limits(),
            control.input_budget.clone(),
        ));
        steering
            .push_message(Message::user_input("retain after panic".into(), []), ())
            .unwrap();
        control.active = Some(Active {
            stop: CancellationToken::new(),
            operation: Operation::Turn {
                steering,
                submission: empty_submission(),
            },
            task: tokio::spawn(async { panic!("injected RPC operation panic") }),
        });
        let joined = (&mut control.active.as_mut().unwrap().task).await;
        let completion = control.finish_operation(joined);
        assert!(completion.error.is_some());
        let terminal = completion.records;
        assert!(
            terminal
                .iter()
                .any(|record| record["type"] == "uncommitted_steering"
                    && record["input"]["content"][0]["Text"] == "retain after panic")
        );
        drop(control);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn task_panic_preserves_queued_input_until_admission_or_recovery() {
        for accepted in [false, true] {
            let (mut control, _events, root) = fixture();
            let message = Message::user_input("QUEUED_ORIGINAL".into(), []);
            let id = Some(json!("queued-id"));
            control.input_budget =
                InputBudget::new(serde_json::to_vec(&(&message, &id)).unwrap().len());
            let input = control
                .input_budget
                .admit(message, id, control.binding.agent().limits())
                .unwrap();
            let (message, id, reservation) = input.into_parts();
            let submission = Arc::new(Submission {
                id,
                admission: Mutex::new(Admission {
                    turn: None,
                    pending: Some((message, reservation)),
                }),
            });
            if accepted {
                submission.accept(1);
            }
            control.active = Some(Active {
                stop: CancellationToken::new(),
                operation: Operation::Turn {
                    steering: Arc::new(SteeringInbox::new(
                        control.binding.agent().limits(),
                        control.input_budget.clone(),
                    )),
                    submission,
                },
                task: tokio::spawn(async { panic!("injected RPC operation panic") }),
            });
            let joined = (&mut control.active.as_mut().unwrap().task).await;
            if !accepted {
                assert!(matches!(
                    control.input_budget.admit(
                        Message::user_input("x".into(), []),
                        &(),
                        control.binding.agent().limits()
                    ),
                    Err(ion_core::CodingAgentError::InputQueueFull { .. })
                ));
            }
            let completion = control.finish_operation(joined);
            assert!(completion.error.is_some());
            assert_eq!(completion.records[0]["type"], "operation_failed");
            let recovery = completion
                .records
                .iter()
                .find(|record| record["type"] == "follow_up_failed");
            if accepted {
                assert!(
                    recovery.is_none(),
                    "committed input was returned as uncommitted"
                );
                assert_eq!(completion.records[0]["turn"], 1);
            } else {
                let recovery = recovery.unwrap();
                assert_eq!(recovery["id"], "queued-id");
                assert_eq!(recovery["input"]["content"][0]["Text"], "QUEUED_ORIGINAL");
            }
            assert!(
                control
                    .input_budget
                    .admit(
                        Message::user_input("x".into(), []),
                        &(),
                        control.binding.agent().limits()
                    )
                    .is_ok()
            );
            drop(control);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn output_failure_stops_publication_but_awaits_owned_work() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        struct FailedOutput(Arc<AtomicUsize>);
        impl Write for FailedOutput {
            fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::other("injected RPC output failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                unreachable!("write failed")
            }
        }
        let (mut control, events, root) = fixture();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let settled = Arc::new(AtomicBool::new(false));
        let task_settled = settled.clone();
        control.output.try_send(json!({"type":"progress"})).unwrap();
        control.active = Some(Active {
            stop,
            operation: Operation::Compact { id: None },
            task: tokio::spawn(async move {
                task_stop.cancelled().await;
                task_settled.store(true, Ordering::SeqCst);
                vec![json!({"type":"settled_terminal"})]
            }),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let (input, _open_client) = tokio::io::duplex(64);
        let error = connection(
            control,
            CommandReader::new(BufReader::new(input)),
            events,
            FailedOutput(calls.clone()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected RPC output failure"));
        assert!(settled.load(Ordering::SeqCst));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "cleanup retried broken physical output"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_submission_holds_capacity_until_acceptance_or_recovery() {
        let (mut control, _events, root) = fixture();
        let session = control.binding.session().clone();
        let permit = session
            .begin_user_shell("hold gate".into(), false, CancellationToken::new())
            .await
            .unwrap();
        let input = Message::user_input("queued".into(), []);
        control.input_budget = InputBudget::new(serde_json::to_vec(&(&input, &())).unwrap().len());
        let input = control
            .input_budget
            .admit(input, None, control.binding.agent().limits())
            .unwrap();
        control.follow_ups.push_back(input);
        control.start_next_follow_up().unwrap();
        tokio::task::yield_now().await;
        assert!(matches!(
            control.input_budget.admit(
                Message::user_input("x".into(), []),
                &(),
                control.binding.agent().limits()
            ),
            Err(ion_core::CodingAgentError::InputQueueFull { .. })
        ));
        control.active.as_ref().unwrap().stop.cancel();
        let joined = (&mut control.active.as_mut().unwrap().task).await;
        let completion = control.finish_operation(joined);
        assert!(completion.error.is_none());
        let terminal = completion.records;
        assert_eq!(terminal[0]["type"], "follow_up_failed");
        assert_eq!(terminal[0]["input"]["content"][0]["Text"], "queued");
        assert!(
            control
                .input_budget
                .admit(
                    Message::user_input("x".into(), []),
                    &(),
                    control.binding.agent().limits()
                )
                .is_ok()
        );
        drop(permit);
        drop(session);
        drop(control);
        std::fs::remove_dir_all(root).unwrap();
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
        assert!(active.operation.steering().is_none());
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
            vec![json!({"type":"settled_terminal"})]
        });
        let steering = Arc::new(SteeringInbox::new(
            control.binding.agent().limits(),
            control.input_budget.clone(),
        ));
        steering
            .push_message(Message::user_input("KEEP_STEERING".into(), []), ())
            .unwrap();
        let queued = control
            .input_budget
            .admit(
                Message::user_input("KEEP_FOLLOW_UP".into(), []),
                Some(json!("keep")),
                control.binding.agent().limits(),
            )
            .unwrap();
        control.follow_ups.push_back(queued);
        control.active = Some(Active {
            stop,
            operation: Operation::Turn {
                steering,
                submission: empty_submission(),
            },
            task,
        });
        let mut records = Vec::new();
        let error = connection(
            control,
            CommandReader::new(BufReader::new(FailedInput)),
            events,
            &mut records,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected RPC input failure"));
        let records = records
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records[0]["type"], "settled_terminal");
        assert_eq!(records[1]["type"], "uncommitted_steering");
        assert_eq!(records[1]["input"]["content"][0]["Text"], "KEEP_STEERING");
        assert_eq!(records[2]["type"], "uncommitted_follow_up");
        assert_eq!(records[2]["id"], "keep");
        assert_eq!(records[2]["input"]["content"][0]["Text"], "KEEP_FOLLOW_UP");
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
            operation: Operation::Shell {
                id: Some(json!("shell-panic")),
            },
            task: tokio::spawn(async { panic!("injected RPC operation panic") }),
        });
        let (input, _open_client) = tokio::io::duplex(64);
        let mut records = Vec::new();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connection(
                control,
                CommandReader::new(BufReader::new(input)),
                events,
                &mut records,
            ),
        )
        .await
        .expect("RPC stranded its active slot after panic")
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected RPC operation panic"));
        let record: Value =
            serde_json::from_slice(records.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
        assert_eq!(record["type"], "operation_failed");
        assert_eq!(record["operation"], "shell");
        assert_eq!(record["id"], "shell-panic");
        std::fs::remove_dir_all(root).unwrap();
    }
}

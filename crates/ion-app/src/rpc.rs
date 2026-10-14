//! Long-lived JSONL client of the shared host and coding loop.
use std::{
    collections::VecDeque,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ion_ai::Message;
use ion_core::{
    AcceptedInput, CodingAgentEvent, ForkPoint, InputBudget, InputReservation, SteeringInbox,
};
use serde_json::{Value, json};
use tokio::{io::BufReader, sync::mpsc};
use tokio_util::sync::CancellationToken;

use crate::{agent_events::event_record, expand_input, inspection, preview_input};

mod connection;
mod input;
mod preparation;
use crate::{publication::Output, stdio};
use connection::connection;
use input::CommandReader;
use preparation::PendingInput;

struct OperationResult {
    records: Vec<Value>,
    // A successful native observation can still contain a nonzero exit.
    failed: bool,
}

type CompletionFuture = Pin<Box<dyn Future<Output = Result<OperationResult>> + Send>>;

struct Active {
    stop: CancellationToken,
    operation: Operation,
    completion: CompletionFuture,
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
/// inside each owned Core operation, so idle commands cannot change its binding.
struct Control {
    binding: ion_host::SessionBinding,
    active: Option<Active>,
    pending_input: Option<PendingInput>,
    follow_ups: VecDeque<AcceptedInput<Option<Value>>>,
    input_budget: InputBudget,
    output: mpsc::Sender<Value>,
}

pub async fn run(binding: ion_host::SessionBinding) -> Result<()> {
    let (output, events) = mpsc::channel(128);
    let control = Control {
        binding,
        active: None,
        pending_input: None,
        follow_ups: VecDeque::new(),
        input_budget: InputBudget::default(),
        output,
    };
    let (input, writer) = stdio::Stdio::open(
        (std::io::stdin(), tokio::io::Interest::READABLE),
        (std::io::stdout(), tokio::io::Interest::WRITABLE),
    )?;
    let mut output = Output::new(writer);
    output.enqueue(
        json!({"type":"ready","session":control.session_id(),"cwd":control.binding.session().cwd()}),
    )?;
    connection(
        control,
        CommandReader::new(BufReader::new(input)),
        events,
        output,
    )
    .await
}

impl Control {
    fn finish_operation(&mut self, joined: Result<OperationResult>) -> Completion {
        // The connection joins the occupied slot, then consumes it here.
        let active = self.active.take().expect("joined active operation");
        let return_inputs =
            active.stop.is_cancelled() || joined.as_ref().map_or(true, |result| result.failed);
        let (mut records, error) = match joined {
            Ok(result) => (result.records, None),
            Err(error) => {
                let error =
                    error.context("RPC operation task failed; unfinished effects remain unknown");
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
            if let Some(pending) = self.pending_input.as_ref()
                && matches!(pending.kind, preparation::InputKind::Steer(_))
            {
                // This input's target ended; invalidation alone is not a user
                // withdrawal of the separate follow-up queue.
                pending.stop.cancel();
            }
            records.extend(steering.take_uncommitted().into_iter().map(
                |pending| json!({"type":"uncommitted_steering","input":pending.into_message()}),
            ));
        }
        if return_inputs {
            self.cancel_preparation();
            records.extend(self.take_uncommitted_follow_ups());
        }
        Completion { records, error }
    }

    fn session_id(&self) -> String {
        self.binding.session_id()
    }

    fn idle(&self) -> Result<()> {
        ensure!(
            self.active.is_none() && self.pending_input.is_none(),
            "an operation or input preparation is active; abort or wait for settlement"
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
        if matches!(command, "prompt" | "steer" | "follow_up") {
            return match self.prepare_input(&value, id.clone(), command) {
                Ok(record) => record,
                Err(error) => Some(failure(id, command, &format!("{error:#}"))),
            };
        }
        let outcome: Result<Value> = (|| {
            match command {
                "clear_queue" => {
                    self.cancel_preparation();
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
                    ensure!(self.active.is_some() || self.pending_input.is_some(), "no active operation or input preparation");
                    if let Some(active) = &self.active { active.stop.cancel(); }
                    self.cancel_preparation();
                    Ok(json!({"disposition":"requested"}))
                }
                "get_state" => {
                    let view = self.binding.session().view()?;
                    let operation = self.active.as_ref().map(|active| active.operation.name());
                    Ok(json!({"session":self.session_id(),"cwd":view.cwd,"name":view.name,"model":self.binding.selected().identity(),"reasoning":view.reasoning.to_string(),"busy":self.active.is_some() || self.pending_input.is_some(),"operation":operation,"preparing_input":self.pending_input.is_some(),"entries":view.entries.len(),"follow_ups":self.follow_ups.len()}))
                }
                "inspect" => {
                    inspection::to_value(&self.binding.session().view()?)
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
                "set_reasoning" => {
                    self.idle()?;
                    let reasoning = required_string(&value, "effort")?.parse::<ion_ai::Reasoning>().map_err(anyhow::Error::msg)?;
                    self.binding.select_reasoning(reasoning)?;
                    Ok(json!({"reasoning":reasoning.to_string()}))
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
                    let after = value.get("after").map(|value| {
                        value.as_bool().context("after must be a boolean")
                    }).transpose()?.unwrap_or(false);
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
                    let name = match value.get("name") {
                        None | Some(Value::Null) => None,
                        Some(value) => Some(value.as_str().context("name must be a string or null")?),
                    };
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

    fn start_next_follow_up(&mut self) -> Result<()> {
        if self.active.is_some() || self.pending_input.is_some() {
            return Ok(());
        }
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
            .run_user_shell(command, stop.clone(), exclude_from_context)?;
        let task_id = id.clone();
        let task: CompletionFuture = Box::pin(async move {
            let result = running.await;
            if let Err(ion_core::CodingSessionError::Operation(
                ion_core::OperationError::TaskFailed(_),
            )) = &result
            {
                return Err(result.unwrap_err().into());
            }
            let failed = result.is_err();
            let record = match result {
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
            Ok(OperationResult {
                records: vec![record],
                failed,
            })
        });
        self.active = Some(Active {
            stop,
            operation: Operation::Shell { id },
            completion: task,
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
        let output_fault = Arc::new(Mutex::new(None));
        let observer_fault = output_fault.clone();
        let observer_stop = task_stop.clone();
        let running = agent.compact(session, task_stop, move |event| {
            if output.try_send(event_record(event)).is_err() {
                *observer_fault
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some("RPC progress queue is unavailable".to_owned());
                observer_stop.cancel();
            }
        });
        let task: CompletionFuture = Box::pin(async move {
            let result = running.await;
            fail_on_unknown_agent_task(&result)?;
            let failed = result.is_err();
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
            if let Some(fault) = output_fault
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                record["output_error"] = json!(fault);
            }
            Ok(OperationResult {
                records: vec![record],
                failed,
            })
        });
        self.active = Some(Active {
            stop,
            operation: Operation::Compact { id },
            completion: task,
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
        let output_fault = Arc::new(Mutex::new(None));
        let observer_fault = output_fault.clone();
        let observer_submission = task_submission.clone();
        let observer_stop = task_stop.clone();
        let running = agent.submit_message_with_steering(
            session,
            input,
            instructions,
            task_stop,
            task_steering,
            move |event| match event {
                CodingAgentEvent::TurnAccepted { turn } => {
                    observer_submission.accept(turn);
                    let record = if queued {
                        json!({"type":"follow_up_started","id":observer_submission.id,"turn":turn})
                    } else {
                        success(
                            observer_submission.id.clone(),
                            "prompt",
                            json!({"disposition":"started","turn":turn}),
                        )
                    };
                    if output.try_send(record).is_err() {
                        *observer_fault
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some("RPC progress queue is unavailable".to_owned());
                        observer_stop.cancel();
                    }
                }
                event => {
                    if let Some(turn) = observer_submission.turn() {
                        let mut record = event_record(event);
                        record["turn"] = json!(turn);
                        if output.try_send(record).is_err() {
                            *observer_fault
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some("RPC progress queue is unavailable".to_owned());
                            observer_stop.cancel();
                        }
                    }
                }
            },
        );
        let task: CompletionFuture = Box::pin(async move {
            let result = running.await;
            fail_on_unknown_agent_task(&result)?;
            let failed = result.is_err();
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
                if let Some(fault) = output_fault
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
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
            Ok(OperationResult {
                records: vec![terminal],
                failed,
            })
        });
        self.active = Some(Active {
            stop,
            operation: Operation::Turn {
                steering,
                submission,
            },
            completion: task,
        });
        Ok(())
    }
}

fn fail_on_unknown_agent_task<T>(
    result: &std::result::Result<T, ion_core::CodingAgentError>,
) -> Result<()> {
    if let Err(ion_core::CodingAgentError::Session(ion_core::CodingSessionError::Operation(
        ion_core::OperationError::TaskFailed(error),
    ))) = result
    {
        bail!("Core operation task failed; local effects remain unknown: {error}");
    }
    Ok(())
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
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::AsyncWrite;

    use super::*;

    fn mock_completion(task: tokio::task::JoinHandle<Vec<Value>>) -> CompletionFuture {
        Box::pin(async move {
            Ok(OperationResult {
                records: task.await?,
                failed: false,
            })
        })
    }

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
        let binding = SessionBinding::new(host, session, selected, None, None).unwrap();
        let (output, events) = mpsc::channel(16);
        let control = Control {
            binding,
            active: None,
            pending_input: None,
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
            completion: mock_completion(tokio::spawn(async {
                vec![json!({"type":"turn_end","status":"completed"})]
            })),
        });
        // The task is complete but the connection has not closed admission yet.
        let joined = (&mut control.active.as_mut().unwrap().completion).await;
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
    async fn stopped_or_failed_settlement_returns_follow_ups_even_after_success_race() {
        for (stopped, failed) in [(false, false), (true, false), (false, true)] {
            let (mut control, _events, root) = fixture();
            let message = Message::user_input("retain queued input".into(), []);
            let id = Some(json!("queued"));
            control.input_budget =
                InputBudget::new(serde_json::to_vec(&(&message, &id)).unwrap().len());
            control.follow_ups.push_back(
                control
                    .input_budget
                    .admit(message.clone(), id, control.binding.agent().limits())
                    .unwrap(),
            );
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
                completion: Box::pin(async move {
                    Ok(OperationResult {
                        records: Vec::new(),
                        failed,
                    })
                }),
            });
            let joined = (&mut control.active.as_mut().unwrap().completion).await;
            // A result already succeeded, but the client has not delivered/joined
            // it yet. Explicit abort still withdraws subsequent queued dispatch.
            if stopped {
                assert!(
                    control.command(br#"{"type":"abort"}"#).unwrap()["success"]
                        .as_bool()
                        .unwrap()
                );
                let refused = control
                    .command(br#"{"type":"follow_up","message":"too late"}"#)
                    .unwrap();
                assert_eq!(refused["success"], false);
                assert!(refused["error"].as_str().unwrap().contains("stopping"));
            }
            let completion = control.finish_operation(joined);
            assert!(completion.error.is_none());
            if stopped || failed {
                assert_eq!(completion.records.len(), 1);
                assert_eq!(completion.records[0]["type"], "uncommitted_follow_up");
                assert_eq!(completion.records[0]["id"], "queued");
                assert_eq!(
                    completion.records[0]["input"],
                    serde_json::to_value(&message).unwrap()
                );
                control.start_next_follow_up().unwrap();
                assert!(control.active.is_none());
                assert!(control.binding.session().view().unwrap().turns().is_empty());
                assert!(
                    control
                        .input_budget
                        .admit(message, &(), control.binding.agent().limits())
                        .is_ok()
                );
            } else {
                assert!(completion.records.is_empty());
                assert_eq!(control.follow_ups.len(), 1);
            }
            drop(control);
            std::fs::remove_dir_all(root).unwrap();
        }
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
            completion: mock_completion(tokio::spawn(async {
                panic!("injected RPC operation panic")
            })),
        });
        let joined = (&mut control.active.as_mut().unwrap().completion).await;
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
                completion: mock_completion(tokio::spawn(async {
                    panic!("injected RPC operation panic")
                })),
            });
            let joined = (&mut control.active.as_mut().unwrap().completion).await;
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
        impl AsyncWrite for FailedOutput {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _bytes: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Poll::Ready(Err(std::io::Error::other("injected RPC output failure")))
            }
            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                self.poll_flush(cx)
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
            completion: mock_completion(tokio::spawn(async move {
                task_stop.cancelled().await;
                task_settled.store(true, Ordering::SeqCst);
                vec![json!({"type":"settled_terminal"})]
            })),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let (input, _open_client) = tokio::io::duplex(64);
        let error = connection(
            control,
            CommandReader::new(BufReader::new(input)),
            events,
            Output::new(FailedOutput(calls.clone())),
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
    async fn output_backpressure_preserves_controls_settlement_and_partial_frames() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::sync::oneshot;

        for abort in [false, true] {
            let (mut control, events, root) = fixture();
            let stop = CancellationToken::new();
            let task_stop = stop.clone();
            let (cancelled, observed) = oneshot::channel();
            let (release, held) = oneshot::channel();
            control.active = Some(Active {
                stop,
                operation: Operation::Compact { id: None },
                completion: mock_completion(tokio::spawn(async move {
                    task_stop.cancelled().await;
                    cancelled.send(()).unwrap();
                    held.await.unwrap();
                    vec![json!({"type":"settled_terminal"})]
                })),
            });
            let (input, mut commands) = tokio::io::duplex(128);
            let (writer, mut reader) = tokio::io::duplex(7);
            let progress = json!({"type":"progress","text":"x".repeat(8192)});
            let mut output = Output::new(writer);
            output.enqueue(progress.clone()).unwrap();
            let running = tokio::spawn(connection(
                control,
                CommandReader::new(BufReader::new(input)),
                events,
                output,
            ));
            let mut bytes = vec![0; 7];
            reader.read_exact(&mut bytes).await.unwrap();
            if abort {
                commands
                    .write_all(b"{\"type\":\"abort\",\"id\":\"abort\"}\n")
                    .await
                    .unwrap();
            } else {
                commands.shutdown().await.unwrap();
            }
            // No more output reads: the current JSON frame cannot finish, but
            // input cancellation must still reach the owned operation.
            tokio::time::timeout(Duration::from_secs(2), observed)
                .await
                .expect("backpressure prevented cancellation")
                .unwrap();
            release.send(()).unwrap();
            if abort {
                commands.shutdown().await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(2), reader.read_to_end(&mut bytes))
                .await
                .expect("publication did not finish after the reader resumed")
                .unwrap();
            running.await.unwrap().unwrap();
            let records = bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(records[0], progress, "partial frame was lost or duplicated");
            assert_eq!(records.last().unwrap()["type"], "settled_terminal");
            assert_eq!(records.len(), if abort { 3 } else { 2 });
            if abort {
                assert_eq!(records[1]["id"], "abort");
                assert_eq!(records[1]["success"], true);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publication_high_water_preserves_settlement_and_pending_inputs() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut control, events, root) = fixture();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let steering = Arc::new(SteeringInbox::new(
            control.binding.agent().limits(),
            control.input_budget.clone(),
        ));
        steering
            .push_message(Message::user_input("KEEP_STEERING".into(), []), ())
            .unwrap();
        let follow_up = control
            .input_budget
            .admit(
                Message::user_input("KEEP_FOLLOW_UP".into(), []),
                Some(json!("follow")),
                control.binding.agent().limits(),
            )
            .unwrap();
        control.follow_ups.push_back(follow_up);
        control.active = Some(Active {
            stop,
            operation: Operation::Turn {
                steering,
                submission: empty_submission(),
            },
            completion: mock_completion(tokio::spawn(async move {
                task_stop.cancelled().await;
                vec![json!({"type":"settled_terminal"})]
            })),
        });
        let (input, mut commands) = tokio::io::duplex(64);
        let (writer, mut reader) = tokio::io::duplex(4096);
        let first = json!({"type":"progress","text":"a".repeat(8 * 1024 * 1024)});
        let second = json!({"type":"progress","text":"b".repeat(8 * 1024 * 1024)});
        let mut output = Output::new(writer);
        output.enqueue(first.clone()).unwrap();
        output.enqueue(second.clone()).unwrap();
        let running = tokio::spawn(connection(
            control,
            CommandReader::new(BufReader::new(input)),
            events,
            output,
        ));
        let mut bytes = vec![0; 4096];
        reader.read_exact(&mut bytes).await.unwrap();
        commands.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        running.await.unwrap().unwrap();
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 5);
        assert_eq!(records[0], first);
        assert_eq!(records[1], second);
        assert_eq!(records[2]["type"], "settled_terminal");
        assert_eq!(records[3]["type"], "uncommitted_steering");
        assert_eq!(records[3]["input"]["content"][0]["Text"], "KEEP_STEERING");
        assert_eq!(records[4]["type"], "uncommitted_follow_up");
        assert_eq!(records[4]["input"]["content"][0]["Text"], "KEEP_FOLLOW_UP");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_submission_holds_capacity_until_acceptance_or_recovery() {
        let (mut control, _events, root) = fixture();
        let session = control.binding.session().clone();
        let (release, held) = tokio::sync::oneshot::channel();
        let permit = session.run_user_shell(
            "hold gate".into(),
            false,
            CancellationToken::new(),
            move |_, _| async move {
                held.await.unwrap();
                ion_core::CodingToolOutput {
                    value: json!({"stdout":""}),
                    images: Vec::new(),
                    is_error: false,
                }
            },
        );
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
        let joined = (&mut control.active.as_mut().unwrap().completion).await;
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
        release.send(()).unwrap();
        permit.await.unwrap();
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

        let terminal = control.active.take().unwrap().completion.await.unwrap();
        let cancelled = &terminal.records[0];
        assert_eq!(cancelled["type"], "compact_end");
        assert_eq!(cancelled["id"], "cancelled");
        assert_eq!(cancelled["status"], "cancelled");

        assert_eq!(
            control.start_compaction(Some(json!("noop"))).unwrap(),
            json!({"disposition":"started"})
        );
        let terminal = control.active.take().unwrap().completion.await.unwrap();
        let completed = &terminal.records[0];
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
        let preparation_settled = Arc::new(AtomicBool::new(false));
        let task_preparation_settled = preparation_settled.clone();
        let preparation_stop = CancellationToken::new();
        let task_preparation_stop = preparation_stop.clone();
        control.pending_input = Some(PendingInput {
            kind: preparation::InputKind::FollowUp,
            id: Some(json!("preparation-fault")),
            stop: preparation_stop,
            withdraw_follow_ups: false,
            task: tokio::spawn(async move {
                task_preparation_stop.cancelled().await;
                task_preparation_settled.store(true, Ordering::SeqCst);
                panic!("injected preparation panic during settlement");
            }),
        });
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
            completion: mock_completion(task),
        });
        let mut records = Vec::new();
        let error = connection(
            control,
            CommandReader::new(BufReader::new(FailedInput)),
            events,
            Output::new(&mut records),
        )
        .await
        .unwrap_err();
        let detail = format!("{error:#}");
        assert!(detail.contains("injected RPC input failure"));
        assert!(detail.contains("injected preparation panic during settlement"));
        assert!(preparation_settled.load(Ordering::SeqCst));
        let records = records
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records[0]["type"], "settled_terminal");
        assert_eq!(records[1]["type"], "uncommitted_steering");
        assert_eq!(records[1]["input"]["content"][0]["Text"], "KEEP_STEERING");
        let preparation = records
            .iter()
            .find(|record| record["id"] == "preparation-fault")
            .unwrap();
        assert_eq!(preparation["success"], false);
        let follow_up = records
            .iter()
            .find(|record| record["type"] == "uncommitted_follow_up")
            .unwrap();
        assert_eq!(follow_up["id"], "keep");
        assert_eq!(follow_up["input"]["content"][0]["Text"], "KEEP_FOLLOW_UP");
        assert!(
            settled.load(Ordering::SeqCst),
            "connection returned before operation settlement"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pending_input_keeps_controls_live_and_eof_joins_cancelled_success() {
        use tokio::io::AsyncWriteExt;
        struct SharedOutput(Arc<Mutex<Vec<u8>>>);
        impl AsyncWrite for SharedOutput {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Poll::Ready(Ok(bytes.len()))
            }
            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                self.poll_flush(cx)
            }
        }
        let (mut control, events, root) = fixture();
        let session = control.binding.session().clone();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let (cancelled, observed) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        control.pending_input = Some(PendingInput {
            kind: preparation::InputKind::Prompt,
            id: Some(json!("original")),
            stop,
            withdraw_follow_ups: false,
            task: tokio::spawn(async move {
                task_stop.cancelled().await;
                cancelled.send(()).unwrap();
                held.await.unwrap();
                Ok(Message::user_input("MUST_NOT_COMMIT".into(), []))
            }),
        });
        let output = Arc::new(Mutex::new(Vec::new()));
        let (input, mut client) = tokio::io::duplex(4096);
        let running = tokio::spawn(connection(
            control,
            CommandReader::new(BufReader::new(input)),
            events,
            Output::new(SharedOutput(output.clone())),
        ));
        client.write_all(b"{\"type\":\"get_state\",\"id\":\"state\"}\n{\"type\":\"prompt\",\"id\":\"second\",\"message\":\"no\"}\n{\"type\":\"new_session\",\"id\":\"replace\"}\n{\"type\":\"abort\",\"id\":\"abort\"}\n").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), observed)
            .await
            .unwrap()
            .unwrap();
        client.shutdown().await.unwrap();
        tokio::task::yield_now().await;
        assert!(!running.is_finished(), "EOF abandoned owned preparation");
        let records = output
            .lock()
            .unwrap()
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["data"]["busy"], true);
        assert_eq!(records[0]["data"]["preparing_input"], true);
        assert_eq!(records[1]["success"], false);
        assert_eq!(records[2]["success"], false);
        assert_eq!(records[3]["id"], "abort");
        assert_eq!(records[3]["success"], true);
        release.send(()).unwrap();
        running.await.unwrap().unwrap();
        let records = output
            .lock()
            .unwrap()
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.last().unwrap()["id"], "original");
        assert_eq!(records.last().unwrap()["success"], false);
        assert!(session.view().unwrap().turns().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn turn_settlement_preserves_follow_up_preparation_only_on_success() {
        for (follow_up, stopped) in [(false, false), (true, false), (false, true), (true, true)] {
            let (mut control, _events, root) = fixture();
            let steering = Arc::new(SteeringInbox::new(
                control.binding.agent().limits(),
                control.input_budget.clone(),
            ));
            control.active = Some(Active {
                stop: CancellationToken::new(),
                operation: Operation::Turn {
                    steering: steering.clone(),
                    submission: empty_submission(),
                },
                completion: mock_completion(tokio::spawn(async { Vec::new() })),
            });
            let (release, held) = tokio::sync::oneshot::channel();
            control.pending_input = Some(PendingInput {
                kind: if follow_up {
                    preparation::InputKind::FollowUp
                } else {
                    preparation::InputKind::Steer(steering.clone())
                },
                id: Some(json!("prepared")),
                stop: CancellationToken::new(),
                withdraw_follow_ups: false,
                task: tokio::spawn(async {
                    held.await.unwrap();
                    Ok(Message::user_input("next".into(), []))
                }),
            });
            control.follow_ups.push_back(
                control
                    .input_budget
                    .admit(
                        Message::user_input("already queued".into(), []),
                        Some(json!("queued")),
                        control.binding.agent().limits(),
                    )
                    .unwrap(),
            );
            let joined = (&mut control.active.as_mut().unwrap().completion).await;
            if stopped {
                control.active.as_ref().unwrap().stop.cancel();
            }
            let completion = control.finish_operation(joined);
            assert_eq!(completion.records.len(), usize::from(stopped));
            control.start_next_follow_up().unwrap();
            assert!(control.active.is_none());
            assert!(control.idle().is_err());
            release.send(()).unwrap();
            let joined = (&mut control.pending_input.as_mut().unwrap().task).await;
            let completion = control.finish_preparation(joined);
            assert!(completion.error.is_none());
            assert_eq!(completion.records[0]["id"], "prepared");
            assert_eq!(completion.records[0]["success"], follow_up && !stopped);
            assert_eq!(
                control.follow_ups.len(),
                if stopped {
                    0
                } else {
                    1 + usize::from(follow_up)
                }
            );
            assert!(steering.take_uncommitted().is_empty());
            drop(control);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aborted_preparation_returns_follow_ups_only_after_active_work_joins() {
        for active in [false, true] {
            let (mut control, _events, root) = fixture();
            let queued = control
                .input_budget
                .admit(
                    Message::user_input("retain behind preparation".into(), []),
                    Some(json!("queued")),
                    control.binding.agent().limits(),
                )
                .unwrap();
            control.follow_ups.push_back(queued);
            let (settle, operation_held) = tokio::sync::oneshot::channel();
            if active {
                control.active = Some(Active {
                    stop: CancellationToken::new(),
                    operation: Operation::Turn {
                        steering: Arc::new(SteeringInbox::new(
                            control.binding.agent().limits(),
                            control.input_budget.clone(),
                        )),
                        submission: empty_submission(),
                    },
                    completion: Box::pin(async {
                        operation_held.await.unwrap();
                        Ok(OperationResult {
                            records: Vec::new(),
                            failed: false,
                        })
                    }),
                });
            }
            let (release, held) = tokio::sync::oneshot::channel();
            control.pending_input = Some(PendingInput {
                kind: preparation::InputKind::FollowUp,
                id: Some(json!("prepared")),
                stop: CancellationToken::new(),
                withdraw_follow_ups: false,
                task: tokio::spawn(async {
                    held.await.unwrap();
                    Ok(Message::user_input("not admitted".into(), []))
                }),
            });
            assert_eq!(
                control.command(br#"{"type":"abort"}"#).unwrap()["success"],
                true
            );
            assert!(control.idle().is_err());
            assert_eq!(control.follow_ups.len(), 1, "stop is not yet settlement");
            release.send(()).unwrap();
            let joined = (&mut control.pending_input.as_mut().unwrap().task).await;
            let completion = control.finish_preparation(joined);
            assert!(completion.error.is_none());
            assert_eq!(completion.records[0]["id"], "prepared");
            assert_eq!(completion.records[0]["success"], false);
            let records = if active {
                assert_eq!(
                    completion.records.len(),
                    1,
                    "preparation cannot return inputs before active effects settle"
                );
                assert_eq!(control.follow_ups.len(), 1);
                control.start_next_follow_up().unwrap();
                assert!(control.active.is_some());
                settle.send(()).unwrap();
                let joined = (&mut control.active.as_mut().unwrap().completion).await;
                control.finish_operation(joined).records
            } else {
                completion.records.into_iter().skip(1).collect()
            };
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["type"], "uncommitted_follow_up");
            assert_eq!(records[0]["id"], "queued");
            assert_eq!(
                records[0]["input"]["content"][0]["Text"],
                "retain behind preparation"
            );
            control.start_next_follow_up().unwrap();
            assert!(control.active.is_none());
            drop(control);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn operation_panic_ends_connection_instead_of_stranding_busy_state() {
        let (mut control, events, root) = fixture();
        control.active = Some(Active {
            stop: CancellationToken::new(),
            operation: Operation::Shell {
                id: Some(json!("shell-panic")),
            },
            completion: mock_completion(tokio::spawn(async {
                panic!("injected RPC operation panic")
            })),
        });
        let (input, _open_client) = tokio::io::duplex(64);
        let mut records = Vec::new();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connection(
                control,
                CommandReader::new(BufReader::new(input)),
                events,
                Output::new(&mut records),
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

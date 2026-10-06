//! Session-owned composed effects. Guest completion is not effect settlement.
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::Arc,
};

use futures_util::{StreamExt, stream::FuturesUnordered};
use ion_ai::{BoxFuture, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    ChildIntent, ChildOutcome, CodeLimits, CodeReply, CodeRequest, CodeRequestKind, CodeRuntime,
    CodeTask, ToolOccurrence,
    agent::{AgentError, AgentEvent},
    composition::failure,
    session::{Session, SessionError},
    tool_result::ToolOutput,
    tool_set::ToolCatalog,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    code: String,
}
struct Admitted {
    intent: ChildIntent,
    reply: oneshot::Sender<CodeReply>,
}

struct Gate<'a, F> {
    session: &'a Session,
    catalog: &'a ToolCatalog,
    observe: &'a mut F,
    turn: u64,
    parent: ToolOccurrence,
    parent_call_id: &'a str,
    limits: CodeLimits,
    stop: CancellationToken,
    waiting: VecDeque<Admitted>,
    calls: usize,
    failed: usize,
    skipped: usize,
    host_bytes: usize,
    audit_bytes: usize,
    fault: Option<SessionError>,
    problem: Option<String>,
}

impl<F: FnMut(AgentEvent)> Gate<'_, F> {
    fn close(&mut self, message: &str) {
        self.problem.get_or_insert_with(|| message.into());
        self.stop.cancel();
    }

    fn storage_fault(&mut self, error: SessionError) {
        if self.fault.is_none() {
            self.fault = Some(error);
        }
        self.stop.cancel();
    }

    fn audit(&mut self, observation: &impl Serialize) {
        let mut count = ByteCount(0);
        serde_json::to_writer(&mut count, observation)
            .expect("audit contains JSON values and validated images");
        self.audit_bytes = self.audit_bytes.saturating_add(count.0);
        if self.audit_bytes > self.limits.audit_admission_bytes {
            self.close("code_mode audit admission byte limit exceeded");
        }
    }

    fn reply(&mut self, sender: oneshot::Sender<CodeReply>, value: &impl Serialize) {
        // An unawaited result still commits, but a departed guest needs no
        // serialization or reply budget. This is not discarded observation.
        if sender.is_closed() {
            return;
        }
        let limit = self
            .limits
            .max_json_bytes
            .min(self.limits.max_host_bytes.saturating_sub(self.host_bytes));
        let mut writer = BoundedJson {
            data: Vec::new(),
            limit,
        };
        if serde_json::to_writer(&mut writer, value).is_err() {
            self.close("code_mode host reply byte limit exceeded");
            let _ = sender.send(Err("code_mode host reply byte limit exceeded".into()));
        } else {
            self.host_bytes += writer.data.len();
            let _ = sender.send(Ok(String::from_utf8(writer.data).expect("JSON is UTF-8")));
        }
    }

    fn accept(&mut self, request: CodeRequest) {
        match request.kind {
            CodeRequestKind::Describe { name } => {
                if self.stop.is_cancelled() {
                    let _ = request.reply.send(Err("guest stopped".into()));
                    return;
                }
                let query = name.to_ascii_lowercase();
                let specs = self
                    .catalog
                    .definitions()
                    .filter(|definition| !self.catalog.is_intrinsic(&definition.spec.name))
                    .filter(|definition| {
                        definition.spec.name.to_ascii_lowercase().contains(&query)
                            || definition
                                .spec
                                .description
                                .to_ascii_lowercase()
                                .contains(&query)
                    })
                    .map(|definition| &definition.spec)
                    .take(10)
                    .collect::<Vec<_>>();
                self.reply(request.reply, &specs);
            }
            CodeRequestKind::Call { name, args_json } => {
                if self.stop.is_cancelled() {
                    let _ = request
                        .reply
                        .send(Err("code_mode admission is closed".into()));
                    return;
                }
                if self.fault.is_some() || self.calls >= self.limits.max_calls {
                    self.close("code_mode call admission stopped or call limit exceeded");
                    let _ = request.reply.send(Err(
                        "code_mode call admission stopped or call limit exceeded".into(),
                    ));
                    return;
                }
                if args_json.len() > self.limits.max_json_bytes {
                    let _ = request
                        .reply
                        .send(Err("tool argument JSON byte limit exceeded".into()));
                    return;
                }
                let arguments = match serde_json::from_str(&args_json) {
                    Ok(Value::Object(arguments)) => Value::Object(arguments),
                    _ => {
                        let _ = request
                            .reply
                            .send(Err("tool arguments must be a JSON object".into()));
                        return;
                    }
                };
                let Some(definition) = self
                    .catalog
                    .definition(&name)
                    .filter(|_| !self.catalog.is_intrinsic(&name))
                else {
                    let _ = request
                        .reply
                        .send(Err(format!("unknown or non-guest capability: {name}")));
                    return;
                };
                let call = ToolCall {
                    id: format!(
                        "{}:{}:{}",
                        self.parent.assistant_entry, self.parent.ordinal, self.calls
                    ),
                    name,
                    arguments,
                    raw_arguments: None,
                };
                let intent = ChildIntent {
                    parent: self.parent,
                    child: self.calls,
                    definition: definition.spec.clone(),
                    activity: self.catalog.activity(&call),
                    call,
                };
                match self.session.record_child_intent(self.turn, intent.clone()) {
                    Ok(()) => {
                        self.calls += 1;
                        self.audit(&intent);
                        (self.observe)(AgentEvent::ChildToolAdmitted {
                            parent_call_id: self.parent_call_id.to_owned(),
                            intent: intent.clone(),
                        });
                        self.waiting.push_back(Admitted {
                            intent,
                            reply: request.reply,
                        });
                    }
                    Err(error) => {
                        self.storage_fault(error);
                        let _ = request
                            .reply
                            .send(Err("child intent was not committed; not dispatched".into()));
                    }
                }
            }
        }
    }

    fn complete(&mut self, admitted: Admitted, outcome: ChildOutcome) {
        self.failed +=
            usize::from(matches!(&outcome, ChildOutcome::Observed { output } if output.is_error));
        self.skipped += usize::from(matches!(outcome, ChildOutcome::NotDispatched { .. }));
        match self.session.record_child_outcome(
            self.turn,
            self.parent,
            admitted.intent.child,
            outcome,
        ) {
            Ok(outcome) => {
                self.audit(&outcome);
                match &outcome {
                    ChildOutcome::Observed { output } => {
                        #[derive(Serialize)]
                        struct Reply<'a> {
                            value: &'a Value,
                            is_error: bool,
                            image_mime_types: Vec<&'a str>,
                        }
                        let value = Reply {
                            value: &output.value,
                            is_error: output.is_error,
                            image_mime_types: output
                                .images
                                .iter()
                                .map(|image| image.mime_type().as_str())
                                .collect(),
                        };
                        if self.stop.is_cancelled() {
                            // The observation remains committed/inspectable, but
                            // no successful consumption follows a fault or stop.
                            let _ = admitted
                                .reply
                                .send(Err("code_mode admission is closed".into()));
                        } else {
                            self.reply(admitted.reply, &value);
                        }
                    }
                    ChildOutcome::NotDispatched { reason } => {
                        let _ = admitted.reply.send(Err(reason.clone()));
                    }
                    ChildOutcome::Unknown => {
                        let _ = admitted.reply.send(Err("child effects are unknown".into()));
                    }
                }
                (self.observe)(AgentEvent::ChildToolFinished {
                    parent: self.parent,
                    child: admitted.intent.child,
                    outcome,
                });
            }
            Err(error) => {
                self.storage_fault(error);
                let _ = admitted.reply.send(Err(
                    "child result was not committed; effect is unknown".into(),
                ));
            }
        }
    }
}

struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct BoundedJson {
    data: Vec<u8>,
    limit: usize,
}
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.data.len()) {
            return Err(io::Error::other("JSON byte limit"));
        }
        self.data.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Retain both worker and admitted effects through every expected fault. The
/// closed worker sender is the FIFO fence for already-transferred requests.
#[expect(
    clippy::too_many_arguments,
    reason = "one composed call binds its active operation, frozen catalogue and publisher"
)]
pub(crate) async fn run<F: FnMut(AgentEvent) + Send>(
    runtime: &Arc<dyn CodeRuntime>,
    limits: CodeLimits,
    session: &Session,
    turn: u64,
    catalog: &ToolCatalog,
    parent_call: &ToolCall,
    operation_stop: &CancellationToken,
    observe: &mut F,
) -> Result<ToolOutput, AgentError> {
    let input: Input = match serde_json::from_value(parent_call.arguments.clone()) {
        Ok(input) => input,
        Err(error) => return Ok(failure(format!("invalid code_mode arguments: {error}"))),
    };
    if input.code.len() > limits.max_code_bytes {
        return Ok(failure("code_mode code byte limit exceeded"));
    }
    if limits.max_calls == 0
        || limits.max_concurrency == 0
        || limits.max_concurrency > limits.max_calls
        || limits.max_host_bytes == 0
        || limits.max_json_bytes == 0
        || limits.audit_admission_bytes == 0
        || limits.deadline.is_zero()
        || tokio::time::Instant::now()
            .checked_add(limits.deadline)
            .is_none()
    {
        return Ok(failure("invalid code_mode admission limits"));
    }
    let parent = session.tool_occurrence(turn, &parent_call.id)?;
    let stop = operation_stop.child_token();
    let CodeTask {
        mut requests,
        mut result,
    } = match runtime.start(input.code, limits, stop.clone()) {
        Ok(task) => task,
        Err(error) => return Ok(failure(error)),
    };
    let mut gate = Gate {
        session,
        catalog,
        observe,
        turn,
        parent,
        parent_call_id: &parent_call.id,
        limits,
        stop,
        waiting: VecDeque::new(),
        calls: 0,
        failed: 0,
        skipped: 0,
        host_bytes: 0,
        audit_bytes: 0,
        fault: None,
        problem: None,
    };
    let mut started = FuturesUnordered::<BoxFuture<'_, (Admitted, ChildOutcome)>>::new();
    let mut root = None;
    let mut requests_closed = false;
    let deadline = tokio::time::sleep(limits.deadline);
    tokio::pin!(deadline);
    let mut expired = false;
    loop {
        while started.len() < limits.max_concurrency {
            let Some(admitted) = gate.waiting.pop_front() else {
                break;
            };
            if gate.stop.is_cancelled() || gate.fault.is_some() {
                let reason = gate
                    .problem
                    .clone()
                    .unwrap_or_else(|| "guest failed or operation stopped".into());
                gate.complete(admitted, ChildOutcome::NotDispatched { reason });
                continue;
            }
            (gate.observe)(AgentEvent::ChildToolStarted {
                parent,
                child: admitted.intent.child,
            });
            let child_stop = gate.stop.clone();
            started.push(Box::pin(async move {
                let outcome = if child_stop.is_cancelled() {
                    ChildOutcome::NotDispatched {
                        reason: "stopped before dispatch".into(),
                    }
                } else {
                    ChildOutcome::Observed {
                        output: catalog.execute(&admitted.intent.call, child_stop).await,
                    }
                };
                (admitted, outcome)
            }));
        }
        if requests_closed && root.is_some() && started.is_empty() && gate.waiting.is_empty() {
            break;
        }
        tokio::select! {
            // A joined guest outranks an expired host timer. The deadline
            // governs guest execution, not post-return effect settlement.
            biased;
            joined = &mut result, if root.is_none() => {
                let outcome = joined.unwrap_or_else(|error| Err(format!("code_mode worker failed: {error}")));
                if outcome.is_err() { gate.stop.cancel(); }
                root = Some(outcome);
            }
            request = requests.recv(), if !requests_closed => {
                if let Some(request) = request { gate.accept(request); } else { requests_closed = true; }
            }
            completed = started.next(), if !started.is_empty() => {
                let (admitted, outcome) = completed.expect("nonempty started set");
                gate.complete(admitted, outcome);
            }
            () = &mut deadline, if !expired && root.is_none() => {
                expired = true; gate.close("code_mode guest deadline exceeded");
            }
        }
    }
    if let Some(fault) = gate.fault {
        return Err(fault.into());
    }
    let outcome = match (
        gate.problem,
        root.expect("worker joined before gateway exits"),
    ) {
        (Some(problem), _) | (None, Err(problem)) => Err(problem),
        (None, Ok(json)) if json.len() <= limits.max_json_bytes => {
            serde_json::from_str::<Value>(&json)
                .map_err(|error| format!("invalid guest JSON: {error}"))
        }
        _ => Err("guest JSON byte limit exceeded".into()),
    };
    let mut output = match outcome {
        Ok(value) => ToolOutput {
            value: json!({"result":value}),
            images: Vec::new(),
            is_error: false,
        },
        Err(error) => failure(error),
    };
    let counters = output
        .value
        .as_object_mut()
        .expect("gateway output is an envelope");
    counters.insert("calls".into(), json!(gate.calls));
    counters.insert("failed_calls".into(), json!(gate.failed));
    counters.insert("skipped_calls".into(), json!(gate.skipped));
    Ok(output)
}

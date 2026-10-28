//! Session-owned composed effects. Guest completion is not effect settlement.
mod declaration;
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Write},
    sync::Arc,
};

use futures_util::{StreamExt, stream::FuturesUnordered};
use ion_ai::{BoxFuture, Reasoning, ToolCall};
use serde::{Deserialize, Serialize, Serializer as _, ser::SerializeSeq};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    ChildIntent, ChildOutcome, CodeLimits, CodeOutput, CodeReply, CodeRequest, CodeRequestKind,
    CodeResult, CodeRuntime, CodeTask, ToolOccurrence,
    agent::{AgentError, AgentEvent},
    composition::failure,
    json_size::encoded_len,
    session::{Session, SessionError, code_inspection::InspectionError},
    tool_result::ToolOutput,
    tool_set::{ToolCatalog, ToolDispatch, ToolExecution},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    code: String,
}
#[derive(Serialize)]
struct ToolDescription<'a> {
    #[serde(flatten)]
    spec: &'a ion_ai::ToolSpec,
    result_schema: Option<ResultSchemaDescription<'a>>,
    declaration: String,
}

#[derive(Serialize)]
struct ResultSchemaDescription<'a> {
    path: Vec<&'a str>,
    schema: &'a Value,
}

struct Admitted {
    intent: ChildIntent,
    reply: oneshot::Sender<CodeReply>,
}

// Validate even requests from runtimes other than Host's JS bridge. Two
// disjoint ranks preserve frozen order and need no sorting or duplicate set.
fn search_tools(
    catalog: &ToolCatalog,
    query: &str,
    limit: usize,
) -> Result<Vec<Value>, &'static str> {
    if !(1..=10).contains(&limit) {
        return Err("searchTools limit must be an integer 1..10");
    }
    let query = query.to_ascii_lowercase();
    let query = query.as_str();
    Ok([true, false]
        .into_iter()
        .flat_map(|exact| {
            catalog.definitions().filter(move |definition| {
                let spec = &definition.spec;
                if catalog.is_intrinsic(&spec.name) {
                    return false;
                }
                let matches_name = spec.name.eq_ignore_ascii_case(query);
                if exact {
                    matches_name
                } else {
                    !matches_name
                        && (spec.name.to_ascii_lowercase().contains(query)
                            || spec.description.to_ascii_lowercase().contains(query))
                }
            })
        })
        .take(limit)
        .map(|definition| {
            json!({"name": definition.spec.name, "description": definition.spec.description})
        })
        .collect())
}

struct Inspection {
    reply: oneshot::Sender<CodeReply>,
    result: oneshot::Receiver<Result<Option<Value>, InspectionError>>,
    worker: std::thread::JoinHandle<()>,
}

struct Gate<'a, F> {
    session: &'a Arc<Session>,
    catalog: &'a ToolCatalog,
    observe: &'a mut F,
    turn: u64,
    parent: ToolOccurrence,
    parent_call_id: &'a str,
    limits: CodeLimits,
    stop: CancellationToken,
    waiting: VecDeque<Admitted>,
    inspection: Option<Inspection>,
    calls: usize,
    failed: usize,
    skipped: usize,
    host_bytes: usize,
    audit_bytes: usize,
    images: BTreeMap<String, ion_ai::ImageContent>,
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
        let bytes =
            encoded_len(observation).expect("audit contains JSON values and validated images");
        self.audit_bytes = self.audit_bytes.saturating_add(bytes);
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
            CodeRequestKind::DescribeTool { name } => {
                if self.stop.is_cancelled() {
                    let _ = request.reply.send(Err("guest stopped".into()));
                    return;
                }
                let description = self
                    .catalog
                    .definition(&name)
                    .filter(|_| !self.catalog.is_intrinsic(&name))
                    .map(|definition| ToolDescription {
                        spec: &definition.spec,
                        declaration: declaration::render(&definition.spec),
                        result_schema: definition.result_schema.as_ref().map(|hint| {
                            ResultSchemaDescription {
                                path: std::iter::once("value")
                                    .chain(hint.value_path.iter().map(String::as_str))
                                    .collect(),
                                schema: &hint.schema,
                            }
                        }),
                    });
                self.reply(request.reply, &description);
            }
            CodeRequestKind::SearchTools { query, limit } => {
                if self.stop.is_cancelled() {
                    let _ = request.reply.send(Err("guest stopped".into()));
                    return;
                }
                match search_tools(self.catalog, &query, limit) {
                    Ok(specs) => self.reply(request.reply, &specs),
                    Err(error) => {
                        let _ = request.reply.send(Err(error.into()));
                    }
                }
            }
            CodeRequestKind::Inspect { query_json } => {
                if self.stop.is_cancelled() {
                    let _ = request.reply.send(Err("guest stopped".into()));
                } else if query_json.len() > self.limits.max_json_bytes {
                    let _ = request
                        .reply
                        .send(Err("inspection query JSON byte limit exceeded".into()));
                } else {
                    match self.session.prepare_code_inspection(&query_json) {
                        Ok(inspection) => {
                            let stop = self.stop.clone();
                            let (sender, result) = oneshot::channel();
                            // A guest can occupy the runtime's only blocking
                            // thread while awaiting this reply. This bounded,
                            // owned reader must have independent capacity.
                            let worker = std::thread::Builder::new()
                                .name("ion-code-inspection".into())
                                .spawn(move || {
                                    let outcome = if stop.is_cancelled() {
                                        Ok(None)
                                    } else {
                                        inspection.read().map(Some)
                                    };
                                    let _ = sender.send(outcome);
                                });
                            match worker {
                                Ok(worker) => {
                                    self.inspection = Some(Inspection {
                                        reply: request.reply,
                                        result,
                                        worker,
                                    });
                                }
                                Err(error) => {
                                    self.close(&format!(
                                        "saved child inspection could not start: {error}"
                                    ));
                                    let _ = request
                                        .reply
                                        .send(Err("saved child inspection could not start".into()));
                                }
                            }
                        }
                        Err(InspectionError::Request(error)) => {
                            let _ = request.reply.send(Err(error));
                        }
                        Err(InspectionError::Session(error)) => {
                            self.storage_fault(error);
                            let _ = request
                                .reply
                                .send(Err("saved child evidence could not be read".into()));
                        }
                    }
                }
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

    fn complete_inspection(
        &mut self,
        inspection: Inspection,
        result: Result<Result<Option<Value>, InspectionError>, oneshot::error::RecvError>,
    ) {
        // The completion channel follows all database/decoding cleanup. Only
        // thread exit remains to join; no filesystem work follows the signal.
        if inspection.worker.join().is_err() {
            self.close("saved child inspection worker panicked");
            let _ = inspection
                .reply
                .send(Err("saved child inspection worker panicked".into()));
            return;
        }
        let reply = inspection.reply;
        match result {
            Ok(Ok(Some(value))) if !self.stop.is_cancelled() => self.reply(reply, &value),
            Ok(Ok(_)) => {
                let _ = reply.send(Err("guest stopped".into()));
            }
            Ok(Err(InspectionError::Request(error))) => {
                let _ = reply.send(Err(error));
            }
            Ok(Err(InspectionError::Session(error))) => {
                self.storage_fault(error);
                let _ = reply.send(Err("saved child evidence could not be read".into()));
            }
            Err(error) => {
                self.close(&format!("saved child inspection worker failed: {error}"));
                let _ = reply.send(Err("saved child inspection worker failed".into()));
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
                            images: Vec<Value>,
                        }
                        let images = output
                            .images
                            .iter()
                            .map(|image| {
                                // A fresh ID prevents a copied or earlier invocation's
                                // block from accidentally selecting a different image.
                                let id = uuid::Uuid::new_v4().to_string();
                                let block =
                                    json!({"type":"image", "id":id, "mime_type":image.mime_type()});
                                self.images.insert(id, image.clone());
                                block
                            })
                            .collect();
                        let value = Reply {
                            value: &output.value,
                            is_error: output.is_error,
                            images,
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

fn tool_manifest<'a>(
    names: impl Iterator<Item = &'a str>,
    limits: CodeLimits,
) -> Result<String, String> {
    let too_large = |_| "code_mode tool manifest byte limit exceeded".to_owned();
    let mut writer = BoundedJson {
        data: Vec::new(),
        limit: limits.max_json_bytes.min(limits.max_host_bytes),
    };
    let mut encoder = serde_json::Serializer::new(&mut writer);
    let mut array = encoder.serialize_seq(None).map_err(too_large)?;
    for name in names {
        if matches!(name, "then" | "toJSON") {
            return Err(format!(
                "code_mode cannot expose reserved tool name {name:?}"
            ));
        }
        array.serialize_element(name).map_err(too_large)?;
    }
    array.end().map_err(too_large)?;
    Ok(String::from_utf8(writer.data).expect("JSON is UTF-8"))
}

fn selected_output(
    root: CodeResult,
    images: &BTreeMap<String, ion_ai::ImageContent>,
    limits: CodeLimits,
) -> ToolOutput {
    let result = root.result.and_then(|json| {
        if json.len() > limits.max_json_bytes {
            return Err("guest JSON byte limit exceeded".into());
        }
        serde_json::from_str::<Value>(&json).map_err(|error| format!("invalid guest JSON: {error}"))
    });
    let mut output = match result {
        Ok(value) => ToolOutput {
            value: json!({"result":value}),
            images: Vec::new(),
            is_error: false,
        },
        Err(error) => failure(error),
    };
    output.value["output"] = json!([]);
    // Charge the actual selected image, not its tiny guest handle. Check each
    // addition before cloning: repeated selections must not allocate unbounded
    // copies before discovering that the complete result is too large.
    let mut bytes = encoded_len(&(&output.value, &output.images)).expect("selected JSON");
    let too_large = || {
        failure("code_mode selected output byte limit exceeded; child observations remain saved")
    };
    if bytes > limits.max_output_bytes {
        return too_large();
    }
    let mut selected = Vec::new();
    for item in root.output {
        let (item, image) = match item {
            CodeOutput::Text { text } => (json!({"type":"text", "text":text}), None),
            CodeOutput::Image { id } => {
                let Some(image) = images.get(&id) else {
                    return failure(
                        "image block does not belong to a committed child observation of this invocation",
                    );
                };
                (
                    json!({"type":"image", "index":output.images.len()}),
                    Some(image),
                )
            }
        };
        let mut added =
            encoded_len(&item).expect("selected JSON") + usize::from(!selected.is_empty());
        if let Some(image) = image {
            added = added
                .saturating_add(encoded_len(image).expect("validated image"))
                .saturating_add(usize::from(!output.images.is_empty()));
        }
        if added > limits.max_output_bytes.saturating_sub(bytes) {
            return too_large();
        }
        bytes += added;
        selected.push(item);
        if let Some(image) = image {
            output.images.push(image.clone());
        }
    }
    output.value["output"] = Value::Array(selected);
    output
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
    session: &Arc<Session>,
    turn: u64,
    catalog: &ToolCatalog,
    parent_call: &ToolCall,
    reasoning: Reasoning,
    model_messages: &[ion_ai::Message],
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
        || limits.max_output_bytes == 0
        || limits.audit_admission_bytes == 0
        || limits.execution_budget.is_zero()
        || tokio::time::Instant::now()
            .checked_add(limits.execution_budget)
            .is_none()
    {
        return Ok(failure("invalid code_mode admission limits"));
    }
    let parent = session.tool_occurrence(turn, &parent_call.id)?;
    let names = catalog
        .definitions()
        .filter(|definition| !catalog.is_intrinsic(&definition.spec.name))
        .map(|definition| definition.spec.name.as_str());
    let tools_json = match tool_manifest(names, limits) {
        Ok(json) => json,
        Err(error) => return Ok(failure(error)),
    };
    let host_bytes = tools_json.len();
    let stop = operation_stop.child_token();
    let CodeTask {
        mut requests,
        mut result,
    } = match runtime.start(input.code, tools_json, limits, stop.clone()) {
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
        inspection: None,
        calls: 0,
        failed: 0,
        skipped: 0,
        host_bytes,
        audit_bytes: 0,
        images: BTreeMap::new(),
        fault: None,
        problem: None,
    };
    let mut started =
        FuturesUnordered::<BoxFuture<'_, (Admitted, Result<ChildOutcome, SessionError>)>>::new();
    let mut root = None;
    let mut requests_closed = false;
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
            let session_id = session.id();
            started.push(Box::pin(async move {
                let outcome = if child_stop.is_cancelled() {
                    Ok(ChildOutcome::NotDispatched {
                        reason: "stopped before dispatch".into(),
                    })
                } else {
                    let (invocation, gateway) = crate::ToolInvocation::admitted(
                        &admitted.intent.call,
                        child_stop.clone(),
                        session_id,
                        turn,
                        admitted.intent.parent,
                        Some(admitted.intent.child),
                        reasoning,
                    );
                    let invocation = invocation.with_model_messages(model_messages);
                    let execution = async {
                        match catalog.dispatch(invocation).await {
                            ToolDispatch::Rejected(reason) => {
                                ToolExecution::NotDispatched { reason }
                            }
                            ToolDispatch::Execution(execution) => execution.await,
                            ToolDispatch::History => ToolExecution::output(
                                crate::session::history::execute(
                                    session,
                                    &admitted.intent.call,
                                    child_stop.clone(),
                                )
                                .await,
                            ),
                            ToolDispatch::Composition(..) => ToolExecution::NotDispatched {
                                reason: "intrinsic tools cannot be composed".into(),
                            },
                        }
                    };
                    gateway
                        .run(session, &child_stop, execution)
                        .await
                        .map(|execution| match execution {
                            ToolExecution::Observed { output, .. } => {
                                ChildOutcome::Observed { output }
                            }
                            ToolExecution::NotDispatched { reason } => {
                                ChildOutcome::NotDispatched { reason }
                            }
                        })
                };
                (admitted, outcome)
            }));
        }
        if requests_closed
            && root.is_some()
            && started.is_empty()
            && gate.waiting.is_empty()
            && gate.inspection.is_none()
        {
            break;
        }
        tokio::select! {
            // Observe joined guest failure before admitting more queued work.
            // The runtime owns runnable-guest time; host waits and settlement
            // retain their tool contracts, cancellation and owned joins.
            biased;
            joined = &mut result, if root.is_none() => {
                let outcome = joined.unwrap_or_else(|error| CodeResult {
                    result: Err(format!("code_mode worker failed: {error}")),
                    output: Vec::new(),
                });
                if outcome.result.is_err() { gate.stop.cancel(); }
                root = Some(outcome);
            }
            // One owned read at a time also bounds inspection admission. The
            // guest may stop/return, but its read joins before parent settlement.
            inspected = async { (&mut gate.inspection.as_mut().expect("active inspection").result).await }, if gate.inspection.is_some() => {
                let inspection = gate.inspection.take().expect("completed inspection");
                gate.complete_inspection(inspection, inspected);
            }
            request = requests.recv(), if !requests_closed && gate.inspection.is_none() => {
                if let Some(request) = request { gate.accept(request); } else { requests_closed = true; }
            }
            completed = started.next(), if !started.is_empty() => {
                let (admitted, outcome) = completed.expect("nonempty started set");
                match outcome {
                    Ok(outcome) => gate.complete(admitted, outcome),
                    Err(error) => {
                        gate.storage_fault(error);
                        let _ = admitted.reply.send(Err("delegation infrastructure failed".into()));
                    }
                }
            }
        }
    }
    if let Some(fault) = gate.fault {
        return Err(fault.into());
    }
    let mut root = root.expect("worker joined before gateway exits");
    if let Some(problem) = gate.problem {
        root.result = Err(problem);
    }
    let mut output = selected_output(root, &gate.images, limits);
    let counters = output
        .value
        .as_object_mut()
        .expect("gateway output is an envelope");
    counters.insert("parent".into(), json!(parent));
    counters.insert("calls".into(), json!(gate.calls));
    counters.insert("failed_calls".into(), json!(gate.failed));
    counters.insert("skipped_calls".into(), json!(gate.skipped));
    Ok(output)
}

#[cfg(test)]
#[path = "code_gateway_tests.rs"]
mod tests;

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn manifest_preserves_exact_names_and_never_publishes_a_partial_namespace() {
        let names = [
            "a-b",
            "a_b",
            "Read",
            "read",
            "__proto__",
            "call",
            "inspect",
            "quote\"\\\n",
        ];
        let json = tool_manifest(names.into_iter(), CodeLimits::default()).unwrap();
        assert_eq!(serde_json::from_str::<Vec<String>>(&json).unwrap(), names);
        for budget in [json.len() - 1, json.len()] {
            for host_budget in [true, false] {
                let mut limits = CodeLimits::default();
                if host_budget {
                    limits.max_host_bytes = budget;
                } else {
                    limits.max_json_bytes = budget;
                }
                let result = tool_manifest(names.into_iter(), limits);
                if budget == json.len() {
                    assert_eq!(result.unwrap(), json);
                } else {
                    assert!(result.unwrap_err().contains("manifest byte limit"));
                }
            }
        }
        for name in ["then", "toJSON"] {
            assert!(
                tool_manifest(["read", name].into_iter(), CodeLimits::default())
                    .unwrap_err()
                    .contains("reserved tool name")
            );
        }
    }

    #[test]
    fn untrusted_search_limits_reject_in_core() {
        let catalog = crate::ToolSet::new([]).snapshot();
        for limit in [0, 11, usize::MAX] {
            assert_eq!(
                search_tools(&catalog, "", limit),
                Err("searchTools limit must be an integer 1..10")
            );
        }
        for limit in 1..=10 {
            assert_eq!(
                search_tools(&catalog, "", limit).unwrap(),
                Vec::<Value>::new()
            );
        }
    }
}

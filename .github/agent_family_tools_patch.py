from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if text.count(old) < count:
        raise SystemExit(f"anchor missing in {path}: {old[:180]!r}")
    p.write_text(text.replace(old, new, count))


# Family control becomes a production model-facing surface. Keep tool behavior
# on the existing durable Family authority rather than creating another child
# registry or lifecycle implementation.
replace(
    "crates/ion-core/src/agent.rs",
    '''use tokio::sync::{OwnedSemaphorePermit, Semaphore};\nuse tokio_util::sync::CancellationToken;\n\nuse crate::error::{CommandError, RuntimeError};\n''',
    '''use serde_json::{Value, json};\nuse tokio::sync::{OwnedSemaphorePermit, Semaphore};\nuse tokio_util::sync::CancellationToken;\n\nuse crate::error::{CommandError, RuntimeError};\n''',
)
replace(
    "crates/ion-core/src/agent.rs",
    '''use crate::store::{AgentRecord, LoadedSession, SessionStore, StoreError};\n''',
    '''use crate::store::{AgentRecord, LoadedSession, SessionStore, StoreError};\nuse crate::tool::{Tool, ToolCatalog, ToolOutcome, ToolSpec};\n''',
)

replace(
    "crates/ion-core/src/agent.rs",
    '''#[derive(Debug, Clone, PartialEq, Eq)]\npub enum Status {\n    Admitted,\n    Active {\n        operation_id: OperationId,\n        state: OperationState,\n    },\n    Suspended {\n        operation_id: OperationId,\n    },\n    Finished {\n        operation_id: OperationId,\n        outcome: OperationOutcome,\n    },\n}\n''',
    '''#[derive(Debug, Clone, PartialEq, Eq)]\npub enum Status {\n    Admitted,\n    Active {\n        operation_id: OperationId,\n        state: OperationState,\n    },\n    Suspended {\n        operation_id: OperationId,\n    },\n    Finished {\n        operation_id: OperationId,\n        outcome: OperationOutcome,\n    },\n}\n\n/// Store-derived view of one retained agent. Result text is resolved for the\n/// exact observed operation boundary, never copied into family residency.\n#[derive(Debug, Clone, PartialEq, Eq)]\npub struct Observation {\n    pub agent_id: AgentId,\n    pub status: Status,\n    pub result: Option<String>,\n}\n\nimpl Observation {\n    fn render(&self) -> String {\n        let status = match &self.status {\n            Status::Admitted => "admitted".to_owned(),\n            Status::Active { operation_id, state } => {\n                format!("active ({operation_id}, {state:?})")\n            }\n            Status::Suspended { operation_id } => format!("suspended ({operation_id})"),\n            Status::Finished { operation_id, outcome } => {\n                format!("finished ({operation_id}, {outcome:?})")\n            }\n        };\n        match &self.result {\n            Some(result) => format!("agent {}: {status}\\n\\n{result}", self.agent_id),\n            None => format!("agent {}: {status}", self.agent_id),\n        }\n    }\n}\n''',
)

# Idle delivery starts execution, so it must consume the same family permit as
# explicit start. Active delivery is continuation input and consumes no second
# permit. A race that discovers an existing execution simply drops the extra
# permit after verifying the returned operation identity.
replace(
    "crates/ion-core/src/agent.rs",
    '''    pub async fn send(\n        &self,\n        from: AgentId,\n        to: AgentId,\n        text: impl Into<String>,\n    ) -> Result<OperationId, Error> {\n        let lane_name = {\n            let retained = self.retained.lock().expect("agent family poisoned");\n            if !retained.contains_key(&from) {\n                return Err(Error::UnknownAgent(from));\n            }\n            retained\n                .get(&to)\n                .map(|agent| agent.lane_name.clone())\n                .ok_or(Error::UnknownAgent(to))?\n        };\n        Ok(self\n            .session\n            .send_agent_message(from, lane_name, text)\n            .await?)\n    }\n''',
    '''    pub async fn send(\n        &self,\n        from: AgentId,\n        to: AgentId,\n        text: impl Into<String>,\n    ) -> Result<OperationId, Error> {\n        let lane_name = {\n            let retained = self.retained.lock().expect("agent family poisoned");\n            if !retained.contains_key(&from) {\n                return Err(Error::UnknownAgent(from));\n            }\n            retained\n                .get(&to)\n                .map(|agent| agent.lane_name.clone())\n                .ok_or(Error::UnknownAgent(to))?\n        };\n        let needs_permit = !matches!(self.status(to).await?, Status::Active { .. });\n        let permit = if needs_permit {\n            Some(\n                Arc::clone(&self.permits)\n                    .try_acquire_owned()\n                    .map_err(|_| Error::Capacity)?,\n            )\n        } else {\n            None\n        };\n        let operation_id = self\n            .session\n            .send_agent_message(from, lane_name, text)\n            .await?;\n        if let Some(permit) = permit {\n            let mut executions = self.executions.lock().expect("agent family poisoned");\n            if let Some(existing) = executions.get(&to) {\n                if existing.operation_id != operation_id {\n                    return Err(Error::Inconsistent(to));\n                }\n            } else {\n                executions.insert(\n                    to,\n                    Execution {\n                        operation_id,\n                        _permit: permit,\n                    },\n                );\n            }\n        }\n        Ok(operation_id)\n    }\n''',
)

replace(
    "crates/ion-core/src/agent.rs",
    '''        self.release_nonexecuting(&loaded);\n        status_from_loaded(&loaded, agent_id)\n    }\n\n    /// Wait for the execution that is current at this call's subscription\n''',
    '''        self.release_nonexecuting(&loaded);\n        status_from_loaded(&loaded, agent_id)\n    }\n\n    /// Observe the latest durable execution and its exact semantic result.\n    pub async fn observe(&self, agent_id: AgentId) -> Result<Observation, Error> {\n        if !self\n            .retained\n            .lock()\n            .expect("agent family poisoned")\n            .contains_key(&agent_id)\n        {\n            return Err(Error::UnknownAgent(agent_id));\n        }\n        let loaded = self.store.load(self.session_id).await?;\n        self.release_nonexecuting(&loaded);\n        let status = status_from_loaded(&loaded, agent_id)?;\n        let result = match status_operation_id(&status) {\n            Some(operation_id) => operation_result(&loaded, agent_id, operation_id)?,\n            None => None,\n        };\n        Ok(Observation {\n            agent_id,\n            status,\n            result,\n        })\n    }\n\n    /// Observe one captured operation even if the agent has since started a\n    /// later run. This is the wait/result boundary used by model-facing tools.\n    pub async fn observe_operation(\n        &self,\n        agent_id: AgentId,\n        operation_id: OperationId,\n    ) -> Result<Observation, Error> {\n        let loaded = self.store.load(self.session_id).await?;\n        self.release_nonexecuting(&loaded);\n        let status = status_for_operation(&loaded, agent_id, operation_id)?;\n        let result = operation_result(&loaded, agent_id, operation_id)?;\n        Ok(Observation {\n            agent_id,\n            status,\n            result,\n        })\n    }\n\n    /// Wait for the execution that is current at this call's subscription\n''',
)

# Exact operation-result extraction follows the lane's serialized operation
# boundaries: the next operation's immutable source leaf is the previous run's
# terminal leaf. This remains correct after later runs without storing a second
# result copy in the family registry.
append = r'''

fn status_operation_id(status: &Status) -> Option<OperationId> {
    match status {
        Status::Admitted => None,
        Status::Active { operation_id, .. }
        | Status::Suspended { operation_id }
        | Status::Finished { operation_id, .. } => Some(*operation_id),
    }
}

fn operation_result(
    loaded: &LoadedSession,
    agent_id: AgentId,
    operation_id: OperationId,
) -> Result<Option<String>, Error> {
    let agent = loaded
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .ok_or(Error::UnknownAgent(agent_id))?;
    let lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == agent.lane_name)
        .ok_or(Error::Inconsistent(agent_id))?;
    let operation = loaded
        .operations
        .iter()
        .find(|operation| operation.id == operation_id && operation.lane_name == lane.name)
        .ok_or(Error::Inconsistent(agent_id))?;
    let end_leaf = loaded
        .operations
        .iter()
        .filter(|candidate| {
            candidate.lane_name == lane.name && candidate.accepted_seq > operation.accepted_seq
        })
        .min_by_key(|candidate| candidate.accepted_seq)
        .and_then(|candidate| candidate.source_leaf)
        .or(lane.state.leaf);

    let mut cursor = end_leaf;
    let mut result = None;
    while cursor != operation.source_leaf {
        let Some(entry_id) = cursor else {
            return Err(Error::Inconsistent(agent_id));
        };
        let record = loaded
            .entries
            .iter()
            .find(|record| record.id == entry_id)
            .ok_or(Error::Inconsistent(agent_id))?;
        if result.is_none()
            && let crate::operation::SessionEntry::AssistantMessage { text } = &record.entry
        {
            result = Some(text.clone());
        }
        cursor = record.parent;
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum AgentToolKind {
    Spawn,
    Start,
    Status,
    Wait,
    Cancel,
    Send,
}

struct AgentTool {
    family: Arc<Family>,
    kind: AgentToolKind,
}

/// Compose model-facing shared-history agent controls over the durable family
/// authority. These tools do not own agent state; they only call [`Family`].
pub fn agent_tools(family: Arc<Family>) -> Vec<Arc<dyn Tool>> {
    [
        AgentToolKind::Spawn,
        AgentToolKind::Start,
        AgentToolKind::Status,
        AgentToolKind::Wait,
        AgentToolKind::Cancel,
        AgentToolKind::Send,
    ]
    .into_iter()
    .map(|kind| Arc::new(AgentTool { family: Arc::clone(&family), kind }) as Arc<dyn Tool>)
    .collect()
}

/// Publish shared-history agent control as a structural host capability. MCP
/// and extensions cannot self-declare this approval bypass.
pub fn install_agent_tools(catalog: &ToolCatalog, family: Arc<Family>) {
    catalog.register_structural_scope("agents", agent_tools(family));
}

impl Tool for AgentTool {
    fn spec(&self) -> ToolSpec {
        match self.kind {
            AgentToolKind::Spawn => ToolSpec {
                name: "spawn_agent".to_owned(),
                description: "Admit a read-only shared-history agent and start it when execution capacity is available. Returns a durable agent handle even when start is capacity-blocked.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"objective": {"type": "string"}},
                    "required": ["objective"]
                }),
            },
            AgentToolKind::Start => ToolSpec {
                name: "agent_start".to_owned(),
                description: "Start a previously admitted idle agent with an objective.".to_owned(),
                input_schema: agent_handle_schema(Some(("objective", "string"))),
            },
            AgentToolKind::Status => ToolSpec {
                name: "agent_status".to_owned(),
                description: "Inspect durable agent status and its latest exact operation result without waiting.".to_owned(),
                input_schema: agent_handle_schema(None),
            },
            AgentToolKind::Wait => ToolSpec {
                name: "agent_wait".to_owned(),
                description: "Wait for the agent's current durable operation and return its exact result.".to_owned(),
                input_schema: agent_handle_schema(None),
            },
            AgentToolKind::Cancel => ToolSpec {
                name: "agent_cancel".to_owned(),
                description: "Cancel the running operation of a retained agent.".to_owned(),
                input_schema: agent_handle_schema(None),
            },
            AgentToolKind::Send => ToolSpec {
                name: "agent_send".to_owned(),
                description: "Send durable input from the root agent to a retained agent. Active work receives continuation input; idle delivery starts a capacity-accounted run.".to_owned(),
                input_schema: agent_handle_schema(Some(("message", "string"))),
            },
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            match self.kind {
                AgentToolKind::Spawn => {
                    let objective = match string_arg(&arguments, "objective") {
                        Ok(value) => value.to_owned(),
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let agent_id = match self.family.admit_lane(self.family.root()).await {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(format!("agent admission failed: {err}")),
                    };
                    match self.family.start(agent_id, objective).await {
                        Ok(operation_id) => ToolOutcome::text(format!(
                            "agent handle: {agent_id}\nstarted: {operation_id}"
                        )),
                        Err(Error::Capacity) => ToolOutcome::text(format!(
                            "agent handle: {agent_id}\nadmitted; execution capacity is exhausted; use agent_start later"
                        )),
                        Err(err) => ToolOutcome::error(format!(
                            "agent handle: {agent_id}\nadmitted, but start failed: {err}"
                        )),
                    }
                }
                AgentToolKind::Start => {
                    let agent_id = match parse_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let objective = match string_arg(&arguments, "objective") {
                        Ok(value) => value.to_owned(),
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.start(agent_id, objective).await {
                        Ok(operation_id) => ToolOutcome::text(format!(
                            "agent {agent_id} started: {operation_id}"
                        )),
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                AgentToolKind::Status => {
                    let agent_id = match parse_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.observe(agent_id).await {
                        Ok(observation) => ToolOutcome::text(observation.render()),
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                AgentToolKind::Wait => {
                    let agent_id = match parse_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let status = match self
                        .family
                        .wait_one(agent_id, cancel, None)
                        .await
                    {
                        Ok(status) => status,
                        Err(err) => return ToolOutcome::error(err.to_string()),
                    };
                    let operation_id = status_operation_id(&status)
                        .expect("wait rejects admitted agents without an operation");
                    match self.family.observe_operation(agent_id, operation_id).await {
                        Ok(observation) => ToolOutcome::text(observation.render()),
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                AgentToolKind::Cancel => {
                    let agent_id = match parse_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.cancel(agent_id).await {
                        Ok(()) => ToolOutcome::text(format!("cancellation accepted for agent {agent_id}")),
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                AgentToolKind::Send => {
                    let agent_id = match parse_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let message = match string_arg(&arguments, "message") {
                        Ok(value) => value.to_owned(),
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.send(self.family.root(), agent_id, message).await {
                        Ok(operation_id) => ToolOutcome::text(format!(
                            "message accepted for agent {agent_id}: {operation_id}"
                        )),
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
            }
        })
    }
}

fn agent_handle_schema(extra: Option<(&str, &str)>) -> Value {
    let mut properties = serde_json::Map::from_iter([(
        "handle".to_owned(),
        json!({"type": "string", "description": "agent-<uuid> returned by spawn_agent"}),
    )]);
    let mut required = vec![Value::String("handle".to_owned())];
    if let Some((name, kind)) = extra {
        properties.insert(name.to_owned(), json!({"type": kind}));
        required.push(Value::String(name.to_owned()));
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn string_arg<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("malformed arguments: `{name}` must be a non-empty string"))
}

fn parse_agent_handle(arguments: &Value) -> Result<AgentId, String> {
    let raw = string_arg(arguments, "handle")?;
    let uuid = raw.strip_prefix("agent-").unwrap_or(raw);
    AgentId::parse(uuid).ok_or_else(|| format!("malformed agent handle {raw:?}"))
}
'''
p = Path("crates/ion-core/src/agent.rs")
p.write_text(p.read_text() + append)

# Export the production family observation/tool surface.
replace(
    "crates/ion-core/src/lib.rs",
    '''pub use agent::{Error as AgentError, Family as AgentFamily, Status as AgentStatus};\n''',
    '''pub use agent::{\n    Error as AgentError, Family as AgentFamily, Observation as AgentObservation, Status as AgentStatus,\n    agent_tools, install_agent_tools,\n};\n''',
)

# Behavioral production-surface tests: tool spawn/wait reports the exact result,
# and idle messaging participates in family execution capacity.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
text += r'''

#[tokio::test]
async fn model_facing_agent_tools_use_family_authority_and_report_exact_result() {
    let provider = SharedLogProvider::default();
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let family = Arc::new(runtime.agent_family(1).await.expect("family"));
    let tools = crate::agent_tools(Arc::clone(&family));

    let spawn = tools[0]
        .call(
            json!({"objective": "inspect the shared branch"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!spawn.is_error, "spawn failed: {spawn:?}");
    let handle = spawn
        .output
        .lines()
        .find_map(|line| line.strip_prefix("agent handle: "))
        .expect("durable agent handle")
        .to_owned();

    let waited = tools[3]
        .call(json!({"handle": handle}), CancellationToken::new())
        .await;
    assert!(!waited.is_error, "wait failed: {waited:?}");
    assert!(waited.output.contains("finished"), "{waited:?}");
    assert!(waited.output.contains("working"), "{waited:?}");

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn idle_agent_message_respects_execution_capacity() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(500),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    let first = family.admit_lane(root).await.expect("first agent");
    let second = family.admit_lane(root).await.expect("second agent");

    family
        .send(root, first, "start from a message")
        .await
        .expect("first message starts execution");
    assert!(matches!(
        family.send(root, second, "must remain blocked").await,
        Err(crate::AgentError::Capacity)
    ));
    assert_eq!(
        family.status(second).await.expect("second status"),
        crate::AgentStatus::Admitted
    );

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
p.write_text(text)

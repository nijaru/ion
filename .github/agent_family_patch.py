from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"anchor missing in {path}: {old[:100]!r}")
    p.write_text(text.replace(old, new, 1))


def insert_after(path: str, anchor: str, addition: str) -> None:
    replace(path, anchor, anchor + addition)


# The agent module owns retained family identity and execution capacity. Durable
# operation state stays in the session/store; the controller only keeps the
# process-local permit residency needed to bound concurrent descendants.
Path("crates/ion-core/src/agent.rs").write_text(r'''use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::CommandError;
use crate::ids::{AgentId, OperationId, SessionId};
use crate::operation::{OperationOutcome, OperationState};
use crate::runtime::SessionHandle;
use crate::store::{AgentRecord, LoadedSession, SessionStore, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("unknown agent {0}")]
    UnknownAgent(AgentId),
    #[error("the root agent is controlled through the session's main lane")]
    RootExecution,
    #[error("agent {0} already has execution residency")]
    AlreadyRunning(AgentId),
    #[error("agent execution capacity is exhausted")]
    Capacity,
    #[error("configured execution capacity {capacity} is below {active} already-active agents")]
    CapacityBelowActive { capacity: usize, active: usize },
    #[error("agent {0} has no running operation")]
    NotRunning(AgentId),
    #[error("agent {0} durable topology is inconsistent")]
    Inconsistent(AgentId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Admitted,
    Active {
        operation_id: OperationId,
        state: OperationState,
    },
    Suspended {
        operation_id: OperationId,
    },
    Finished {
        operation_id: OperationId,
        outcome: OperationOutcome,
    },
}

#[derive(Debug, Clone)]
struct RetainedAgent {
    lane_name: String,
}

struct Execution {
    operation_id: OperationId,
    _permit: OwnedSemaphorePermit,
}

/// Family-scoped agent authority for one root session.
///
/// Durable identity/topology is published by the session writer. This object
/// retains addressability separately from process-local execution permits;
/// completion and operation state are always read from authoritative session
/// state rather than copied into the registry.
pub struct Family {
    session_id: SessionId,
    root: AgentId,
    session: SessionHandle,
    store: SessionStore,
    retained: Mutex<HashMap<AgentId, RetainedAgent>>,
    executions: Mutex<HashMap<AgentId, Execution>>,
    permits: Arc<Semaphore>,
}

impl std::fmt::Debug for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Family")
            .field("session_id", &self.session_id)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Family {
    pub(crate) async fn attach(
        session_id: SessionId,
        session: SessionHandle,
        store: SessionStore,
        max_active: usize,
    ) -> Result<Self, Error> {
        // A command round-trip establishes that a newly spawned session has
        // committed its root record before we read family topology.
        let _ = session.snapshot().await?;
        let loaded = store.load(session_id).await?;
        let root = AgentId::root(session_id);
        let retained = loaded
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.id,
                    RetainedAgent {
                        lane_name: agent.lane_name.clone(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        if !retained.contains_key(&root) {
            return Err(Error::Inconsistent(root));
        }

        let active = active_agent_operations(&loaded);
        if active.len() > max_active {
            return Err(Error::CapacityBelowActive {
                capacity: max_active,
                active: active.len(),
            });
        }
        let permits = Arc::new(Semaphore::new(max_active));
        let mut executions = HashMap::new();
        for (agent_id, operation_id) in active {
            let permit = Arc::clone(&permits)
                .try_acquire_owned()
                .map_err(|_| Error::Capacity)?;
            executions.insert(
                agent_id,
                Execution {
                    operation_id,
                    _permit: permit,
                },
            );
        }

        Ok(Self {
            session_id,
            root,
            session,
            store,
            retained: Mutex::new(retained),
            executions: Mutex::new(executions),
            permits,
        })
    }

    #[must_use]
    pub const fn root(&self) -> AgentId {
        self.root
    }

    /// Admit a retained shared-history agent from the control parent's current
    /// lane boundary. Identity + lane publication is one durable transaction;
    /// no execution permit is consumed by admission.
    pub async fn admit_lane(&self, control_parent: AgentId) -> Result<AgentId, Error> {
        let source_lane = self
            .retained
            .lock()
            .expect("agent family poisoned")
            .get(&control_parent)
            .map(|agent| agent.lane_name.clone())
            .ok_or(Error::UnknownAgent(control_parent))?;
        let agent_id = AgentId::generate();
        let lane_name = self
            .session
            .admit_agent_lane(agent_id, control_parent, source_lane)
            .await?;
        let previous = self
            .retained
            .lock()
            .expect("agent family poisoned")
            .insert(agent_id, RetainedAgent { lane_name });
        debug_assert!(previous.is_none(), "new AgentId is unique in its family");
        Ok(agent_id)
    }

    /// Start one admitted descendant if execution capacity is available.
    /// Capacity belongs to residency, not identity: an admitted agent remains
    /// addressable when this returns [`Error::Capacity`].
    pub async fn start(
        &self,
        agent_id: AgentId,
        prompt: impl Into<String>,
    ) -> Result<OperationId, Error> {
        if agent_id == self.root {
            return Err(Error::RootExecution);
        }
        let loaded = self.store.load(self.session_id).await?;
        self.release_nonexecuting(&loaded);
        if self
            .executions
            .lock()
            .expect("agent family poisoned")
            .contains_key(&agent_id)
        {
            return Err(Error::AlreadyRunning(agent_id));
        }
        let lane_name = self
            .retained
            .lock()
            .expect("agent family poisoned")
            .get(&agent_id)
            .map(|agent| agent.lane_name.clone())
            .ok_or(Error::UnknownAgent(agent_id))?;
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let operation_id = self
            .session
            .submit_if_idle_on_lane(lane_name, prompt)
            .await?;
        self.executions
            .lock()
            .expect("agent family poisoned")
            .insert(
                agent_id,
                Execution {
                    operation_id,
                    _permit: permit,
                },
            );
        Ok(operation_id)
    }

    /// Observe authoritative durable operation state for one retained agent.
    /// Reading a terminal/suspended state also releases any stale local permit;
    /// it never consumes or deletes the durable completion.
    pub async fn status(&self, agent_id: AgentId) -> Result<Status, Error> {
        if !self
            .retained
            .lock()
            .expect("agent family poisoned")
            .contains_key(&agent_id)
        {
            return Err(Error::UnknownAgent(agent_id));
        }
        let loaded = self.store.load(self.session_id).await?;
        self.release_nonexecuting(&loaded);
        status_from_loaded(&loaded, agent_id)
    }

    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {
        let status = self.status(agent_id).await?;
        let operation_id = match status {
            Status::Active { operation_id, .. } => operation_id,
            Status::Admitted | Status::Suspended { .. } | Status::Finished { .. } => {
                return Err(Error::NotRunning(agent_id));
            }
        };
        self.session.cancel(operation_id).await?;
        Ok(())
    }

    fn release_nonexecuting(&self, loaded: &LoadedSession) {
        let active = active_agent_operations(loaded)
            .into_iter()
            .collect::<HashMap<_, _>>();
        self.executions
            .lock()
            .expect("agent family poisoned")
            .retain(|agent_id, execution| {
                active.get(agent_id).copied() == Some(execution.operation_id)
            });
    }
}

fn active_agent_operations(loaded: &LoadedSession) -> Vec<(AgentId, OperationId)> {
    loaded
        .agents
        .iter()
        .filter(|agent| !matches!(agent.history, crate::store::AgentHistory::Root))
        .filter_map(|agent| {
            let lane = loaded.lanes.iter().find(|lane| lane.name == agent.lane_name)?;
            let operation_id = lane.state.current_operation?;
            let operation = loaded
                .operations
                .iter()
                .find(|operation| operation.id == operation_id)?;
            if matches!(
                operation.latest.1.state,
                OperationState::Finished(_) | OperationState::Suspended
            ) {
                None
            } else {
                Some((agent.id, operation_id))
            }
        })
        .collect()
}

fn status_from_loaded(loaded: &LoadedSession, agent_id: AgentId) -> Result<Status, Error> {
    let agent: &AgentRecord = loaded
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .ok_or(Error::UnknownAgent(agent_id))?;
    let lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == agent.lane_name)
        .ok_or(Error::Inconsistent(agent_id))?;
    let latest = loaded
        .operations
        .iter()
        .filter(|operation| operation.lane_name == lane.name)
        .max_by_key(|operation| operation.accepted_seq);
    let Some(operation) = latest else {
        return Ok(Status::Admitted);
    };
    match &operation.latest.1.state {
        OperationState::Finished(outcome) => Ok(Status::Finished {
            operation_id: operation.id,
            outcome: outcome.clone(),
        }),
        OperationState::Suspended => Ok(Status::Suspended {
            operation_id: operation.id,
        }),
        state => {
            if lane.state.current_operation != Some(operation.id) {
                return Err(Error::Inconsistent(agent_id));
            }
            Ok(Status::Active {
                operation_id: operation.id,
                state: state.clone(),
            })
        }
    }
}
''')

# Make the target domain public through deliberate re-exports.
replace(
    "crates/ion-core/src/lib.rs",
    "mod context;\n",
    "mod agent;\nmod context;\n",
)
insert_after(
    "crates/ion-core/src/lib.rs",
    '''pub use context::{\n''',
    '''''',
)
replace(
    "crates/ion-core/src/lib.rs",
    '''pub use context::{\n    CapabilitySnapshot, ContextManifest, ContextMessage, ContextPlan, SYSTEM_SECTION,\n''',
    '''pub use agent::{Error as AgentError, Family as AgentFamily, Status as AgentStatus};\npub use context::{\n    CapabilitySnapshot, ContextManifest, ContextMessage, ContextPlan, SYSTEM_SECTION,\n''',
)

# Runtime command and session-writer ownership of atomic lane-agent publication.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''use crate::ids::{\n    EffectId, EntryId, InboxId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId,\n};''',
    '''use crate::ids::{\n    AgentId, EffectId, EntryId, InboxId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId,\n};''',
)
insert_after(
    "crates/ion-core/src/runtime/mod.rs",
    '''    CreateLane {\n        lane_name: String,\n        reply: oneshot::Sender<Result<(), CommandError>>,\n    },\n''',
    '''    AdmitAgentLane {\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: String,\n        reply: oneshot::Sender<Result<String, CommandError>>,\n    },\n''',
)
insert_after(
    "crates/ion-core/src/runtime/mod.rs",
    '''    pub async fn create_lane(&self, lane_name: impl Into<String>) -> Result<(), CommandError> {\n        let (reply, rx) = oneshot::channel();\n        self.tx\n            .try_send(SessionCommand::CreateLane {\n                lane_name: lane_name.into(),\n                reply,\n            })\n            .map_err(command_send_error)?;\n        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n''',
    '''\n    pub(crate) async fn admit_agent_lane(\n        &self,\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: impl Into<String>,\n    ) -> Result<String, CommandError> {\n        let (reply, rx) = oneshot::channel();\n        self.tx\n            .try_send(SessionCommand::AdmitAgentLane {\n                agent_id,\n                control_parent_id,\n                source_lane_name: source_lane_name.into(),\n                reply,\n            })\n            .map_err(command_send_error)?;\n        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n''',
)
insert_after(
    "crates/ion-core/src/runtime/mod.rs",
    '''            SessionCommand::CreateLane { lane_name, reply } => {\n                let _ = reply.send(self.create_lane(lane_name).await);\n                false\n            }\n''',
    '''            SessionCommand::AdmitAgentLane {\n                agent_id,\n                control_parent_id,\n                source_lane_name,\n                reply,\n            } => {\n                let _ = reply.send(\n                    self.admit_agent_lane(agent_id, control_parent_id, source_lane_name)\n                        .await,\n                );\n                false\n            }\n''',
)
insert_after(
    "crates/ion-core/src/runtime/mod.rs",
    '''    async fn create_lane(&mut self, lane_name: String) -> Result<(), CommandError> {\n        if self.closed {\n            return Err(CommandError::Closed);\n        }\n        let lane_name = lane_name.trim().to_owned();\n        if lane_name.is_empty() {\n            return Err(CommandError::InvalidLaneName);\n        }\n        if self.lanes.contains_key(&lane_name) {\n            return Err(CommandError::LaneExists(lane_name));\n        }\n        let source_leaf = self.main_lane().state.leaf;\n        let model_ref = self.main_model_ref().to_owned();\n        self.store\n            .create_lane(\n                self.session_id,\n                lane_name.clone(),\n                source_leaf,\n                model_ref.clone(),\n            )\n            .await\n            .map_err(persistence_command_error)?;\n        let previous = self.lanes.insert(\n            lane_name.clone(),\n            ResidentLane::new(crate::session::lane::Lane {\n                name: lane_name,\n                state: crate::session::lane::State {\n                    leaf: source_leaf,\n                    current_operation: None,\n                    pending_next_run: None,\n                },\n                config: crate::session::lane::Config::new(model_ref),\n            }),\n        );\n        debug_assert!(previous.is_none(), "lane topology identity is unique");\n        Ok(())\n    }\n''',
    '''\n    async fn admit_agent_lane(\n        &mut self,\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: String,\n    ) -> Result<String, CommandError> {\n        if self.closed {\n            return Err(CommandError::Closed);\n        }\n        let source = self\n            .lane(&source_lane_name)\n            .ok_or_else(|| CommandError::LaneNotFound(source_lane_name.clone()))?;\n        let source_leaf = source.state.leaf;\n        let model_ref = source.config.model_ref.clone();\n        let lane_name = agent_id.to_string();\n        if self.lanes.contains_key(&lane_name) {\n            return Err(CommandError::LaneExists(lane_name));\n        }\n        self.store\n            .admit_lane_agent(\n                self.session_id,\n                agent_id,\n                control_parent_id,\n                lane_name.clone(),\n                source_leaf,\n                model_ref.clone(),\n            )\n            .await\n            .map_err(persistence_command_error)?;\n        let previous = self.lanes.insert(\n            lane_name.clone(),\n            ResidentLane::new(crate::session::lane::Lane {\n                name: lane_name.clone(),\n                state: crate::session::lane::State {\n                    leaf: source_leaf,\n                    current_operation: None,\n                    pending_next_run: None,\n                },\n                config: crate::session::lane::Config::new(model_ref),\n            }),\n        );\n        debug_assert!(previous.is_none(), "agent lane identity is unique");\n        Ok(lane_name)\n    }\n''',
)

# Retain the store at the host object so a family controller can reattach from
# durable topology without exposing SQLite to frontends.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let artifact_root = self.store.artifact_root();\n''',
    '''        let runtime_store = self.store.clone();\n        let artifact_root = self.store.artifact_root();\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        Runtime {\n            session,\n            session_id,\n            join,\n        }\n''',
    '''        Runtime {\n            session,\n            session_id,\n            store: runtime_store,\n            join,\n        }\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''pub struct Runtime {\n    session: SessionHandle,\n    session_id: SessionId,\n    join: JoinHandle<()>,\n}\n''',
    '''pub struct Runtime {\n    session: SessionHandle,\n    session_id: SessionId,\n    store: SessionStore,\n    join: JoinHandle<()>,\n}\n''',
)
insert_after(
    "crates/ion-core/src/runtime/mod.rs",
    '''    pub const fn session_id(&self) -> SessionId {\n        self.session_id\n    }\n''',
    '''\n    /// Attach the family-scoped agent authority for this durable root.\n    /// Existing retained identities and active descendant residency are\n    /// reconstructed from the store before the controller is returned.\n    pub async fn agent_family(\n        &self,\n        max_active: usize,\n    ) -> Result<crate::agent::Family, crate::agent::Error> {\n        crate::agent::Family::attach(\n            self.session_id,\n            self.session.clone(),\n            self.store.clone(),\n            max_active,\n        )\n        .await\n    }\n''',
)

# Behavioral family test: admission retains identity without consuming a permit;
# capacity is enforced only when execution starts and is reusable after terminal
# durable state is observed.
replace(
    "crates/ion-core/src/tests.rs",
    "mod agent_store;\n",
    "mod agent_family;\nmod agent_store;\n",
)
Path("crates/ion-core/src/tests/agent_family.rs").write_text(r'''use super::support::*;

#[tokio::test]
async fn retained_agent_identity_is_separate_from_execution_capacity() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(500),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime =
        start_runtime_with_store(provider, ToolRegistry::default(), store.clone());
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    assert_eq!(root, crate::AgentId::root(runtime.session_id()));

    let first = family.admit_lane(root).await.expect("first admission");
    let second = family.admit_lane(root).await.expect("second admission");
    assert_ne!(first, second);

    let first_operation = family.start(first, "first objective").await.expect("first start");
    assert!(matches!(
        family.start(second, "second objective").await,
        Err(crate::AgentError::Capacity)
    ));
    // The second identity was durably admitted even though it has no execution
    // permit yet.
    assert_eq!(family.status(second).await.expect("second status"), crate::AgentStatus::Admitted);

    sleep(Duration::from_millis(650)).await;
    assert!(matches!(
        family.status(first).await.expect("first terminal"),
        crate::AgentStatus::Finished { operation_id, .. } if operation_id == first_operation
    ));

    let second_operation = family
        .start(second, "second objective")
        .await
        .expect("capacity released after terminal observation");
    assert_ne!(first_operation, second_operation);
    assert!(matches!(
        family.status(second).await.expect("second active"),
        crate::AgentStatus::Active { operation_id, .. } if operation_id == second_operation
    ));

    let loaded = store.load(runtime.session_id()).await.expect("family load");
    assert_eq!(loaded.agents.len(), 3);
    assert!(loaded.agents.iter().any(|agent| agent.id == first));
    assert!(loaded.agents.iter().any(|agent| agent.id == second));

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
''')

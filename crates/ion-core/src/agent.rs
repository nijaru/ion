use std::collections::HashMap;
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
            let lane = loaded
                .lanes
                .iter()
                .find(|lane| lane.name == agent.lane_name)?;
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

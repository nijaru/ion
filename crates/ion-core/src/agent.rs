use std::collections::{HashMap, HashSet};
use std::future::pending;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::error::{CommandError, RuntimeError};
use crate::ids::{AgentId, OperationId, SessionId};
use crate::operation::{OperationOutcome, OperationState};
use crate::runtime::{RuntimeEvent, SessionHandle};
use crate::store::{AgentHistory, AgentRecord, LoadedSession, SessionStore, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
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
    #[error("agent {0} has no live hosted-session residency")]
    NotResident(AgentId),
    #[error("agent wait set cannot be empty")]
    EmptyWaitSet,
    #[error("agent {0} appears more than once in the wait set")]
    DuplicateWaitTarget(AgentId),
    #[error("agent wait was cancelled")]
    WaitCancelled,
    #[error("agent wait deadline elapsed")]
    WaitDeadlineElapsed,
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

/// Store-derived view of one retained agent. Result text is resolved for the
/// exact observed operation boundary, never copied into family residency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub agent_id: AgentId,
    pub status: Status,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTarget {
    SharedHistory { session_id: SessionId },
    SeparateSession { session_id: SessionId },
}

#[derive(Debug, Clone)]
struct RetainedAgent {
    lane_name: String,
}

struct Execution {
    operation_id: OperationId,
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone)]
struct WaitTarget {
    agent_id: AgentId,
    operation_id: OperationId,
    ready: Option<Status>,
}

#[derive(Debug, Clone, Copy)]
enum WaitMode {
    Any,
    All,
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
    hosted_sessions: Mutex<HashMap<SessionId, SessionHandle>>,
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
        Self::attach_inner(session_id, session, store, max_active, true).await
    }

    pub(crate) async fn attach_durable(
        session_id: SessionId,
        session: SessionHandle,
        store: SessionStore,
        max_active: usize,
    ) -> Result<Self, Error> {
        Self::attach_inner(session_id, session, store, max_active, false).await
    }

    async fn attach_inner(
        session_id: SessionId,
        session: SessionHandle,
        store: SessionStore,
        max_active: usize,
        await_session_start: bool,
    ) -> Result<Self, Error> {
        // New sessions need a command round-trip so the root row is durable. A
        // resumed interactive runtime already has that row and may deliberately
        // be waiting for host scope reattachment before recovery starts.
        if await_session_start {
            let _ = session.snapshot().await?;
        }
        let loaded = store.load(session_id).await?;
        let family_agents = store.load_agent_family(session_id).await?;
        let root = AgentId::root(session_id);
        if !family_agents.iter().any(|agent| agent.id == root) {
            return Err(Error::Inconsistent(root));
        }
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
            hosted_sessions: Mutex::new(HashMap::new()),
            permits,
        })
    }

    #[must_use]
    pub const fn root(&self) -> AgentId {
        self.root
    }

    /// Resolve one durable family address to its history/runtime topology.
    /// This lookup is store-backed so separately hosted descendants admitted
    /// after `Family::attach` are still recognized without process-local
    /// registration.
    pub(crate) async fn target(&self, agent_id: AgentId) -> Result<AgentTarget, Error> {
        let agent = self
            .store
            .load_family_agent(self.session_id, agent_id)
            .await?
            .ok_or(Error::UnknownAgent(agent_id))?;
        match &agent.history {
            AgentHistory::Root | AgentHistory::SharedLane { .. }
                if agent.session_id == self.session_id =>
            {
                Ok(AgentTarget::SharedHistory {
                    session_id: agent.session_id,
                })
            }
            AgentHistory::Fresh | AgentHistory::Fork { .. }
                if agent.session_id != self.session_id =>
            {
                Ok(AgentTarget::SeparateSession {
                    session_id: agent.session_id,
                })
            }
            _ => Err(Error::Inconsistent(agent_id)),
        }
    }

    /// Register one process-local runtime incarnation for a separately hosted
    /// family session. Durable identity/topology remains store-owned; this map
    /// exists only so family control can route live wait/cancel operations.
    pub(crate) fn register_hosted_session(&self, session_id: SessionId, session: SessionHandle) {
        self.hosted_sessions
            .lock()
            .expect("agent family poisoned")
            .insert(session_id, session);
    }

    /// Forget one hosted runtime incarnation without changing durable agent
    /// identity or completion state.
    pub(crate) fn unregister_hosted_session(&self, session_id: SessionId) {
        self.hosted_sessions
            .lock()
            .expect("agent family poisoned")
            .remove(&session_id);
    }

    fn hosted_session(&self, session_id: SessionId) -> Option<SessionHandle> {
        self.hosted_sessions
            .lock()
            .expect("agent family poisoned")
            .get(&session_id)
            .cloned()
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

    /// Send a durable message between retained agents. An active target
    /// receives it as continuation input at the next reasoning boundary; an
    /// idle target starts a new operation rooted in the typed agent message.
    pub async fn send(
        &self,
        from: AgentId,
        to: AgentId,
        text: impl Into<String>,
    ) -> Result<OperationId, Error> {
        let lane_name = {
            let retained = self.retained.lock().expect("agent family poisoned");
            if !retained.contains_key(&from) {
                return Err(Error::UnknownAgent(from));
            }
            retained
                .get(&to)
                .map(|agent| agent.lane_name.clone())
                .ok_or(Error::UnknownAgent(to))?
        };
        let needs_permit = !matches!(self.status(to).await?, Status::Active { .. });
        let permit = if needs_permit {
            Some(
                Arc::clone(&self.permits)
                    .try_acquire_owned()
                    .map_err(|_| Error::Capacity)?,
            )
        } else {
            None
        };
        let operation_id = self
            .session
            .send_agent_message(from, lane_name, text)
            .await?;
        if let Some(permit) = permit {
            let mut executions = self.executions.lock().expect("agent family poisoned");
            if let Some(existing) = executions.get(&to) {
                if existing.operation_id != operation_id {
                    return Err(Error::Inconsistent(to));
                }
            } else {
                executions.insert(
                    to,
                    Execution {
                        operation_id,
                        _permit: permit,
                    },
                );
            }
        }
        Ok(operation_id)
    }

    /// Observe authoritative durable operation state for any retained family
    /// agent. Shared-history permit cleanup remains local to the root runtime;
    /// separately hosted residency is still owned by the child host for now.
    pub async fn status(&self, agent_id: AgentId) -> Result<Status, Error> {
        let loaded = self.load_addressed_session(agent_id).await?;
        status_from_loaded(&loaded, agent_id)
    }

    /// Observe the latest durable execution and its exact semantic result for
    /// either shared-history or separately hosted topology.
    pub async fn observe(&self, agent_id: AgentId) -> Result<Observation, Error> {
        let loaded = self.load_addressed_session(agent_id).await?;
        let status = status_from_loaded(&loaded, agent_id)?;
        let result = match status_operation_id(&status) {
            Some(operation_id) => operation_result(&loaded, agent_id, operation_id)?,
            None => None,
        };
        Ok(Observation {
            agent_id,
            status,
            result,
        })
    }

    /// Observe one captured operation even if the agent has since started a
    /// later run. This exact durable result boundary spans all current family
    /// topologies; waiting/execution residency remains topology-specific.
    pub async fn observe_operation(
        &self,
        agent_id: AgentId,
        operation_id: OperationId,
    ) -> Result<Observation, Error> {
        let loaded = self.load_addressed_session(agent_id).await?;
        let status = status_for_operation(&loaded, agent_id, operation_id)?;
        let result = operation_result(&loaded, agent_id, operation_id)?;
        Ok(Observation {
            agent_id,
            status,
            result,
        })
    }

    async fn load_addressed_session(&self, agent_id: AgentId) -> Result<LoadedSession, Error> {
        let target = self.target(agent_id).await?;
        let session_id = match target {
            AgentTarget::SharedHistory { session_id }
            | AgentTarget::SeparateSession { session_id } => session_id,
        };
        let loaded = self.store.load(session_id).await?;
        if matches!(target, AgentTarget::SharedHistory { .. }) {
            self.release_nonexecuting(&loaded);
        }
        Ok(loaded)
    }

    /// Wait for the execution that is current at this call's subscription
    /// boundary. Wait cancellation/deadline affect only the waiter; they never
    /// cancel the agent or consume its durable completion.
    pub async fn wait_one(
        &self,
        agent_id: AgentId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Status, Error> {
        match self.target(agent_id).await? {
            AgentTarget::SharedHistory { .. } => {
                let mut completed = self
                    .wait_set(&[agent_id], WaitMode::All, cancel, deadline)
                    .await?;
                Ok(completed
                    .pop()
                    .expect("one-agent wait returns exactly one result")
                    .1)
            }
            AgentTarget::SeparateSession { session_id } => {
                self.wait_hosted_one(agent_id, session_id, cancel, deadline)
                    .await
            }
        }
    }

    async fn wait_hosted_one(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Status, Error> {
        let initial = self.status(agent_id).await?;
        let operation_id = match initial {
            Status::Admitted => return Err(Error::NotRunning(agent_id)),
            Status::Active { operation_id, .. } => operation_id,
            terminal @ (Status::Suspended { .. } | Status::Finished { .. }) => {
                return Ok(terminal);
            }
        };

        let Some(session) = self.hosted_session(session_id) else {
            let status = self.observe_operation(agent_id, operation_id).await?.status;
            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                return Ok(status);
            }
            return Err(Error::NotResident(agent_id));
        };

        // Subscribe before the second durable read. A completion racing the
        // first read is therefore either present in the store or retained in
        // this subscriber's event ring.
        let (_snapshot, mut events) = session.subscribe().await?;
        let status = self.observe_operation(agent_id, operation_id).await?.status;
        if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
            return Ok(status);
        }

        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err(Error::WaitCancelled),
                () = wait_until(deadline) => return Err(Error::WaitDeadlineElapsed),
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            if matches!(event, RuntimeEvent::SessionClosed { .. }) {
                                let status = self.observe_operation(agent_id, operation_id).await?.status;
                                if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                    return Ok(status);
                                }
                                return Err(RuntimeError::SubscriptionClosed.into());
                            }
                            if event.operation_id() != Some(operation_id) || !event_is_terminal(&event) {
                                continue;
                            }
                            let status = self.observe_operation(agent_id, operation_id).await?.status;
                            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                return Ok(status);
                            }
                        }
                        Err(RuntimeError::SubscriptionLagged) => {
                            let (_snapshot, replacement) = session.subscribe().await?;
                            events = replacement;
                            let status = self.observe_operation(agent_id, operation_id).await?.status;
                            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                return Ok(status);
                            }
                        }
                        Err(RuntimeError::SubscriptionClosed) => {
                            let status = self.observe_operation(agent_id, operation_id).await?.status;
                            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                return Ok(status);
                            }
                            return Err(RuntimeError::SubscriptionClosed.into());
                        }
                        Err(other) => return Err(other.into()),
                    }
                }
            }
        }
    }

    /// Return when any execution in `agent_ids` reaches a durable terminal or
    /// suspended state. Already-complete executions win immediately in input
    /// order; completion remains observable by future waits/status calls.
    pub async fn wait_any(
        &self,
        agent_ids: &[AgentId],
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<(AgentId, Status), Error> {
        let mut completed = self
            .wait_set(agent_ids, WaitMode::Any, cancel, deadline)
            .await?;
        Ok(completed
            .pop()
            .expect("non-empty any wait returns one result"))
    }

    /// Return only after every captured execution reaches a durable terminal
    /// or suspended state. Results preserve the caller's input order.
    pub async fn wait_all(
        &self,
        agent_ids: &[AgentId],
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Vec<(AgentId, Status)>, Error> {
        self.wait_set(agent_ids, WaitMode::All, cancel, deadline)
            .await
    }

    async fn wait_set(
        &self,
        agent_ids: &[AgentId],
        mode: WaitMode,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Vec<(AgentId, Status)>, Error> {
        self.validate_wait_set(agent_ids)?;

        // Subscribe first, then read durable state. A transition before the
        // subscription is visible in the load; a transition after it emits an
        // event. This closes the classic status-then-subscribe lost-wakeup gap.
        let (_snapshot, mut events) = self.session.subscribe().await?;
        let mut targets = self.load_wait_targets(agent_ids).await?;
        if let Some(done) = wait_results(&targets, mode) {
            return Ok(done);
        }

        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err(Error::WaitCancelled),
                () = wait_until(deadline) => return Err(Error::WaitDeadlineElapsed),
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            if matches!(event, RuntimeEvent::SessionClosed { .. }) {
                                self.refresh_wait_targets(&mut targets).await?;
                                if let Some(done) = wait_results(&targets, mode) {
                                    return Ok(done);
                                }
                                return Err(RuntimeError::SubscriptionClosed.into());
                            }
                            let Some(operation_id) = event.operation_id() else {
                                continue;
                            };
                            if !event_is_terminal(&event)
                                || !targets.iter().any(|target| {
                                    target.operation_id == operation_id && target.ready.is_none()
                                })
                            {
                                continue;
                            }
                            // Terminal events are emitted only after durable
                            // settlement. Re-read the exact captured operation
                            // rather than following a later run on this agent.
                            self.refresh_wait_targets(&mut targets).await?;
                            if let Some(done) = wait_results(&targets, mode) {
                                return Ok(done);
                            }
                        }
                        Err(RuntimeError::SubscriptionLagged) => {
                            // Resubscribe before the durable read to close the
                            // same lost-wakeup gap while resynchronizing.
                            let (_snapshot, replacement) = self.session.subscribe().await?;
                            events = replacement;
                            self.refresh_wait_targets(&mut targets).await?;
                            if let Some(done) = wait_results(&targets, mode) {
                                return Ok(done);
                            }
                        }
                        Err(RuntimeError::SubscriptionClosed) => {
                            self.refresh_wait_targets(&mut targets).await?;
                            if let Some(done) = wait_results(&targets, mode) {
                                return Ok(done);
                            }
                            return Err(RuntimeError::SubscriptionClosed.into());
                        }
                        Err(other) => return Err(other.into()),
                    }
                }
            }
        }
    }

    fn validate_wait_set(&self, agent_ids: &[AgentId]) -> Result<(), Error> {
        if agent_ids.is_empty() {
            return Err(Error::EmptyWaitSet);
        }
        let retained = self.retained.lock().expect("agent family poisoned");
        let mut seen = HashSet::with_capacity(agent_ids.len());
        for &agent_id in agent_ids {
            if !retained.contains_key(&agent_id) {
                return Err(Error::UnknownAgent(agent_id));
            }
            if !seen.insert(agent_id) {
                return Err(Error::DuplicateWaitTarget(agent_id));
            }
        }
        Ok(())
    }

    async fn load_wait_targets(&self, agent_ids: &[AgentId]) -> Result<Vec<WaitTarget>, Error> {
        let loaded = self.store.load(self.session_id).await?;
        self.release_nonexecuting(&loaded);
        agent_ids
            .iter()
            .copied()
            .map(|agent_id| {
                let status = status_from_loaded(&loaded, agent_id)?;
                match status {
                    Status::Admitted => Err(Error::NotRunning(agent_id)),
                    Status::Active { operation_id, .. } => Ok(WaitTarget {
                        agent_id,
                        operation_id,
                        ready: None,
                    }),
                    terminal @ (Status::Suspended { operation_id }
                    | Status::Finished { operation_id, .. }) => Ok(WaitTarget {
                        agent_id,
                        operation_id,
                        ready: Some(terminal),
                    }),
                }
            })
            .collect()
    }

    async fn refresh_wait_targets(&self, targets: &mut [WaitTarget]) -> Result<(), Error> {
        let loaded = self.store.load(self.session_id).await?;
        self.release_nonexecuting(&loaded);
        for target in targets.iter_mut().filter(|target| target.ready.is_none()) {
            let status = status_for_operation(&loaded, target.agent_id, target.operation_id)?;
            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                target.ready = Some(status);
            }
        }
        Ok(())
    }

    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {
        let target = self.target(agent_id).await?;
        let status = self.status(agent_id).await?;
        let operation_id = match status {
            Status::Active { operation_id, .. } => operation_id,
            Status::Admitted | Status::Suspended { .. } | Status::Finished { .. } => {
                return Err(Error::NotRunning(agent_id));
            }
        };
        let session = match target {
            AgentTarget::SharedHistory { .. } => self.session.clone(),
            AgentTarget::SeparateSession { session_id } => {
                let Some(session) = self.hosted_session(session_id) else {
                    let latest = self.observe_operation(agent_id, operation_id).await?.status;
                    if !matches!(latest, Status::Active { .. }) {
                        return Err(Error::NotRunning(agent_id));
                    }
                    return Err(Error::NotResident(agent_id));
                };
                session
            }
        };
        session.cancel(operation_id).await?;
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

fn status_for_operation(
    loaded: &LoadedSession,
    agent_id: AgentId,
    operation_id: OperationId,
) -> Result<Status, Error> {
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
        .find(|operation| operation.id == operation_id)
        .ok_or(Error::Inconsistent(agent_id))?;
    if operation.lane_name != lane.name {
        return Err(Error::Inconsistent(agent_id));
    }
    match &operation.latest.1.state {
        OperationState::Finished(outcome) => Ok(Status::Finished {
            operation_id,
            outcome: outcome.clone(),
        }),
        OperationState::Suspended => Ok(Status::Suspended { operation_id }),
        state => {
            if lane.state.current_operation != Some(operation_id) {
                return Err(Error::Inconsistent(agent_id));
            }
            Ok(Status::Active {
                operation_id,
                state: state.clone(),
            })
        }
    }
}

fn wait_results(targets: &[WaitTarget], mode: WaitMode) -> Option<Vec<(AgentId, Status)>> {
    match mode {
        WaitMode::Any => targets.iter().find_map(|target| {
            target
                .ready
                .clone()
                .map(|status| vec![(target.agent_id, status)])
        }),
        WaitMode::All => targets
            .iter()
            .map(|target| target.ready.clone().map(|status| (target.agent_id, status)))
            .collect::<Option<Vec<_>>>(),
    }
}

fn event_is_terminal(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::OperationFinished { .. }
            | RuntimeEvent::OperationFailed { .. }
            | RuntimeEvent::OperationIndeterminate { .. }
            | RuntimeEvent::OperationCancelled { .. }
            | RuntimeEvent::OperationApprovalRequired { .. }
    )
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => pending::<()>().await,
    }
}

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

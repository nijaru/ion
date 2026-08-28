from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"anchor missing in {path}: {old[:100]!r}")
    p.write_text(text.replace(old, new, 1))


def insert_after(path: str, anchor: str, addition: str) -> None:
    replace(path, anchor, anchor + addition)


p = Path("crates/ion-core/src/agent.rs")
text = p.read_text()
text = text.replace(
    "use std::collections::HashMap;\nuse std::sync::{Arc, Mutex};\n",
    "use std::collections::{HashMap, HashSet};\nuse std::future::pending;\nuse std::sync::{Arc, Mutex};\nuse std::time::Instant;\n",
)
text = text.replace(
    "use tokio::sync::{OwnedSemaphorePermit, Semaphore};\n",
    "use tokio::sync::{OwnedSemaphorePermit, Semaphore};\nuse tokio_util::sync::CancellationToken;\n",
)
text = text.replace(
    "use crate::error::CommandError;\n",
    "use crate::error::{CommandError, RuntimeError};\n",
)
text = text.replace(
    "use crate::runtime::SessionHandle;\n",
    "use crate::runtime::{RuntimeEvent, SessionHandle};\n",
)
text = text.replace(
    '''    #[error(transparent)]\n    Store(#[from] StoreError),\n''',
    '''    #[error(transparent)]\n    Store(#[from] StoreError),\n    #[error(transparent)]\n    Runtime(#[from] RuntimeError),\n''',
)
text = text.replace(
    '''    #[error("agent {0} has no running operation")]\n    NotRunning(AgentId),\n''',
    '''    #[error("agent {0} has no running operation")]\n    NotRunning(AgentId),\n    #[error("agent wait set cannot be empty")]\n    EmptyWaitSet,\n    #[error("agent {0} appears more than once in the wait set")]\n    DuplicateWaitTarget(AgentId),\n    #[error("agent wait was cancelled")]\n    WaitCancelled,\n    #[error("agent wait deadline elapsed")]\n    WaitDeadlineElapsed,\n''',
)
# Internal wait target/mode after execution residency.
text = text.replace(
    '''struct Execution {\n    operation_id: OperationId,\n    _permit: OwnedSemaphorePermit,\n}\n''',
    '''struct Execution {\n    operation_id: OperationId,\n    _permit: OwnedSemaphorePermit,\n}\n\n#[derive(Debug, Clone)]\nstruct WaitTarget {\n    agent_id: AgentId,\n    operation_id: OperationId,\n    ready: Option<Status>,\n}\n\n#[derive(Debug, Clone, Copy)]\nenum WaitMode {\n    Any,\n    All,\n}\n''',
)
# Insert public waits before cancel.
anchor = '''    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {\n'''
addition = r'''    /// Wait for the execution that is current at this call's subscription
    /// boundary. Wait cancellation/deadline affect only the waiter; they never
    /// cancel the agent or consume its durable completion.
    pub async fn wait_one(
        &self,
        agent_id: AgentId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Status, Error> {
        let mut completed = self
            .wait_set(&[agent_id], WaitMode::All, cancel, deadline)
            .await?;
        Ok(completed
            .pop()
            .expect("one-agent wait returns exactly one result")
            .1)
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

'''
text = text.replace(anchor, addition + anchor, 1)

# Helpers after status_from_loaded.
text += r'''

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
            .map(|target| {
                target
                    .ready
                    .clone()
                    .map(|status| (target.agent_id, status))
            })
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
'''
p.write_text(text)

# Tests cover one/any/all, explicit cancellation/deadline, and non-consuming
# completion semantics.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
text += r'''

#[tokio::test]
async fn agent_waits_are_event_driven_and_completion_is_non_consuming() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(220),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let family = runtime.agent_family(2).await.expect("family");
    let root = family.root();
    let first = family.admit_lane(root).await.expect("first admission");
    let second = family.admit_lane(root).await.expect("second admission");
    let first_operation = family.start(first, "first wait").await.expect("first start");
    let second_operation = family.start(second, "second wait").await.expect("second start");

    let cancelled = CancellationToken::new();
    let cancelled_for_wait = cancelled.clone();
    let cancelled_wait = family.wait_one(first, cancelled_for_wait, None);
    tokio::pin!(cancelled_wait);
    tokio::select! {
        result = &mut cancelled_wait => panic!("wait completed before explicit cancellation: {result:?}"),
        () = sleep(Duration::from_millis(30)) => cancelled.cancel(),
    }
    assert!(matches!(cancelled_wait.await, Err(crate::AgentError::WaitCancelled)));

    let deadline = std::time::Instant::now() + Duration::from_millis(30);
    assert!(matches!(
        family
            .wait_one(second, CancellationToken::new(), Some(deadline))
            .await,
        Err(crate::AgentError::WaitDeadlineElapsed)
    ));

    let completed = family
        .wait_all(&[first, second], CancellationToken::new(), None)
        .await
        .expect("wait all");
    assert_eq!(completed.len(), 2);
    assert!(matches!(
        &completed[0].1,
        crate::AgentStatus::Finished { operation_id, .. } if *operation_id == first_operation
    ));
    assert!(matches!(
        &completed[1].1,
        crate::AgentStatus::Finished { operation_id, .. } if *operation_id == second_operation
    ));

    // Earlier cancelled/timed-out waiters did not consume completion.
    assert!(matches!(
        family
            .wait_one(first, CancellationToken::new(), None)
            .await
            .expect("repeat wait"),
        crate::AgentStatus::Finished { operation_id, .. } if operation_id == first_operation
    ));

    let third = family.admit_lane(root).await.expect("third admission");
    let third_operation = family.start(third, "third wait").await.expect("third start");
    let (winner, winner_status) = family
        .wait_any(&[third, first], CancellationToken::new(), None)
        .await
        .expect("wait any");
    assert_eq!(winner, first);
    assert!(matches!(
        winner_status,
        crate::AgentStatus::Finished { operation_id, .. } if operation_id == first_operation
    ));
    assert!(matches!(
        family.status(third).await.expect("third remains active"),
        crate::AgentStatus::Active { operation_id, .. } if operation_id == third_operation
    ));

    family
        .wait_one(third, CancellationToken::new(), None)
        .await
        .expect("third finishes");
    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn agent_wait_set_rejects_missing_execution_and_duplicates() {
    let runtime = tool_runtime();
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    let idle = family.admit_lane(root).await.expect("idle admission");

    assert!(matches!(
        family
            .wait_one(idle, CancellationToken::new(), None)
            .await,
        Err(crate::AgentError::NotRunning(agent)) if agent == idle
    ));
    assert!(matches!(
        family
            .wait_all(&[], CancellationToken::new(), None)
            .await,
        Err(crate::AgentError::EmptyWaitSet)
    ));
    assert!(matches!(
        family
            .wait_any(&[idle, idle], CancellationToken::new(), None)
            .await,
        Err(crate::AgentError::DuplicateWaitTarget(agent)) if agent == idle
    ));

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
p.write_text(text)

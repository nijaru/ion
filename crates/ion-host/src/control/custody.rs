//! Pending input payloads and positive Core acknowledgements, separate from execution.
use super::*;

// Snapshot captures retain values across release of the lifecycle mutex. M only
// needs Send: its own mutex permits a borrowed projection without requiring Sync.
pub(super) struct SnapshotValue<T>(Arc<Mutex<T>>);
impl<T> Clone for SnapshotValue<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> SnapshotValue<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }

    pub(super) fn with<R>(&self, project: impl FnOnce(&T) -> R) -> R {
        let value = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        project(&value)
    }

    pub(super) fn get_mut(&mut self) -> &mut T {
        Arc::get_mut(&mut self.0)
            .expect("exclusive control cannot overlap a snapshot capture")
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn into_inner(self) -> T {
        Arc::into_inner(self.0)
            .expect("exclusive control cannot overlap a snapshot capture")
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) struct Input<M> {
    pub(super) delivery: Delivery,
    pub(super) metadata: SnapshotValue<M>,
    pub(super) reservation: InputReservation,
    pub(super) state: InputState,
    pub(super) steering_status: Option<SteeringMonitor>,
}

pub(super) enum InputState {
    Pending(Box<PendingInput>),
    Accepted(InputAcceptance),
}

pub(super) struct PendingInput {
    pub(super) original: UserInput,
    pub(super) instructions: String,
    pub(super) content: InputContent,
    pub(super) withdrawn: Option<ReturnReason>,
}

pub(super) enum InputContent {
    Images(Message),
    Ready(PreparedInput<InputId>),
    Transferred {
        receipt: InputReceipt,
        operation: OperationId,
    },
}

impl<M> Input<M> {
    pub(super) fn pending(&self) -> Option<&PendingInput> {
        match &self.state {
            InputState::Pending(input) => Some(input),
            InputState::Accepted(_) => None,
        }
    }

    pub(super) fn pending_mut(&mut self) -> Option<&mut PendingInput> {
        match &mut self.state {
            InputState::Pending(input) => Some(input),
            InputState::Accepted(_) => None,
        }
    }

    pub(super) fn acceptance(&self) -> Option<InputAcceptance> {
        match self.state {
            InputState::Accepted(acceptance) => Some(acceptance),
            InputState::Pending(_) => None,
        }
    }
}

impl PendingInput {
    pub(super) fn withdraw(&mut self, reason: ReturnReason) {
        self.withdrawn.get_or_insert(reason);
    }

    pub(super) fn take_ready(&mut self, operation: OperationId) -> Option<PreparedInput<InputId>> {
        let InputContent::Ready(prepared) = &self.content else {
            return None;
        };
        let transferred = InputContent::Transferred {
            receipt: prepared.receipt(),
            operation,
        };
        let InputContent::Ready(prepared) = std::mem::replace(&mut self.content, transferred)
        else {
            unreachable!("matched ready content")
        };
        Some(prepared)
    }
}

// Watch notifications and closing prepared custody can wake caller code. Defer
// both until the lifecycle mutation is complete and the mutex is released.
enum Deferred {
    Notify(SteeringMonitor, SteeringStatus),
    Pending(Box<PendingInput>),
    Content(InputContent),
    Monitor(SteeringMonitor),
}

pub(super) struct Guard<'a, M>(Option<MutexGuard<'a, Custody<M>>>);
impl<'a, M> Guard<'a, M> {
    pub(super) fn new(custody: &'a Mutex<Custody<M>>) -> Self {
        Self(Some(
            custody
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ))
    }
}
impl<M> std::ops::Deref for Guard<'_, M> {
    type Target = Custody<M>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("held custody")
    }
}
impl<M> std::ops::DerefMut for Guard<'_, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("held custody")
    }
}
impl<M> Drop for Guard<'_, M> {
    fn drop(&mut self) {
        let mut custody = self.0.take().expect("held custody");
        let deferred = std::mem::take(&mut custody.deferred);
        drop(custody);
        for action in deferred {
            match action {
                Deferred::Notify(monitor, status) => monitor.publish(status),
                Deferred::Pending(pending) => drop(pending),
                Deferred::Content(content) => drop(content),
                Deferred::Monitor(monitor) => drop(monitor),
            }
        }
    }
}

pub(super) struct Custody<M> {
    pub(super) inputs: BTreeMap<InputId, Input<M>>,
    pub(super) updates: VecDeque<SnapshotValue<ControlUpdate<M>>>,
    pub(super) occupied: usize,
    pub(super) steering_target: Option<SteeringTarget>,
    deferred: Vec<Deferred>,
}
impl<M> Default for Custody<M> {
    fn default() -> Self {
        Self {
            inputs: BTreeMap::new(),
            updates: VecDeque::new(),
            occupied: 0,
            steering_target: None,
            deferred: Vec::new(),
        }
    }
}
impl<M> Custody<M> {
    pub(super) fn push_update(&mut self, update: ControlUpdate<M>) {
        self.updates.push_back(SnapshotValue::new(update));
    }

    pub(super) fn accept(&mut self, id: InputId, operation: OperationId, turn: u64) {
        let input = self
            .inputs
            .get_mut(&id)
            .expect("accepted input retains Host custody");
        assert!(
            matches!(
                input.pending().map(|input| &input.content),
                Some(InputContent::Transferred { .. })
            ),
            "Core acknowledges transferred input once"
        );
        let acceptance = InputAcceptance { operation, turn };
        let previous = std::mem::replace(&mut input.state, InputState::Accepted(acceptance));
        let InputState::Pending(pending) = previous else {
            unreachable!("pending input")
        };
        self.deferred.push(Deferred::Pending(pending));
        if let Some(status) = &input.steering_status {
            self.deferred.push(Deferred::Notify(
                status.clone(),
                SteeringStatus::Committed { acceptance },
            ));
        }
        self.push_update(ControlUpdate::Accepted {
            id,
            operation,
            turn,
        });
    }

    pub(super) fn return_input(&mut self, id: InputId, reason: ReturnReason) {
        let input = self
            .inputs
            .remove(&id)
            .expect("returned input retains Host custody");
        let InputState::Pending(pending) = input.state else {
            panic!("committed input cannot be returned")
        };
        let PendingInput {
            original,
            content,
            withdrawn,
            ..
        } = *pending;
        let reason = withdrawn.unwrap_or(reason);
        if let Some(status) = input.steering_status {
            self.deferred.push(Deferred::Notify(
                status,
                SteeringStatus::Returned {
                    input: original.clone(),
                    reason: ReturnSnapshot::from(&reason),
                },
            ));
        }
        self.deferred.push(Deferred::Content(content));
        self.push_update(ControlUpdate::Returned(ReturnedInput {
            id,
            delivery: input.delivery,
            input: original,
            metadata: input.metadata.into_inner(),
            reason,
            preparation_error: None,
            reservation: Some(input.reservation),
        }));
    }

    pub(super) fn unresolved(&mut self, id: InputId, session: uuid::Uuid) {
        let input = self
            .inputs
            .remove(&id)
            .expect("unresolved input retains Host custody");
        let InputState::Pending(pending) = input.state else {
            panic!("committed input is resolved")
        };
        let InputContent::Transferred { receipt, operation } = pending.content else {
            panic!("only transferred custody can have an unresolved commit")
        };
        if let Some(status) = input.steering_status {
            self.deferred
                .push(Deferred::Notify(status, SteeringStatus::Unknown));
        }
        self.push_update(ControlUpdate::Unresolved(UnresolvedInput {
            session,
            id,
            delivery: input.delivery,
            operation,
            receipt,
            input: pending.original,
            metadata: input.metadata.into_inner(),
            reservation: Some(input.reservation),
        }));
    }

    pub(super) fn take_accepted(&mut self) -> Vec<AcceptedMetadata<M>> {
        let ids: Vec<_> = self
            .inputs
            .iter()
            .filter_map(|(&id, input)| input.acceptance().map(|_| id))
            .collect();
        let mut accepted = Vec::with_capacity(ids.len());
        for id in ids {
            let input = self
                .inputs
                .remove(&id)
                .expect("accepted metadata retains custody");
            let acceptance = input
                .acceptance()
                .expect("accepted metadata retains its Turn");
            if let Some(monitor) = input.steering_status {
                self.deferred.push(Deferred::Monitor(monitor));
            }
            accepted.push(AcceptedMetadata {
                id,
                acceptance,
                metadata: input.metadata.into_inner(),
                reservation: Some(input.reservation),
            });
        }
        accepted
    }

    pub(super) fn lost_control(&mut self) {
        self.steering_target = None;
        for input in self.inputs.values() {
            if input.pending().is_some()
                && let Some(status) = &input.steering_status
            {
                self.deferred
                    .push(Deferred::Notify(status.clone(), SteeringStatus::Unknown));
            }
        }
    }

    pub(super) fn take_matching(
        &mut self,
        matches: impl Fn(&ControlUpdate<M>) -> bool,
    ) -> Option<ControlUpdate<M>> {
        let index = self
            .updates
            .iter()
            .position(|update| update.with(&matches))?;
        let mut update = self
            .updates
            .remove(index)
            .expect("located reliable decision")
            .into_inner();
        self.occupied -= update.release();
        Some(update)
    }

    pub(super) fn pop(&mut self) -> Option<ControlUpdate<M>> {
        let mut update = self.updates.pop_front()?.into_inner();
        self.occupied -= update.release();
        Some(update)
    }
}

/// Core may publish before an asynchronous admission returns its actual handle.
/// Keep that bounded passive prefix private until reliable acceptance is owned.
pub(super) struct AdmissionObservations {
    sender: broadcast::Sender<Observation>,
    operation: OperationId,
    state: Mutex<AdmissionObservationState>,
}
enum AdmissionObservationState {
    Pending {
        events: VecDeque<Observation>,
        skipped: u64,
    },
    Open,
    Closed,
}
impl AdmissionObservations {
    pub(super) fn new(sender: broadcast::Sender<Observation>, operation: OperationId) -> Arc<Self> {
        Arc::new(Self {
            sender,
            operation,
            state: Mutex::new(AdmissionObservationState::Pending {
                events: VecDeque::new(),
                skipped: 0,
            }),
        })
    }
    pub(super) fn observer(self: &Arc<Self>) -> Box<dyn FnMut(AgentEvent) + Send + 'static> {
        let gate = self.clone();
        Box::new(move |event| {
            let observation = Observation::Event {
                operation: gate.operation,
                event: Arc::new(event),
            };
            let mut state = gate
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &mut *state {
                AdmissionObservationState::Pending { events, skipped } => {
                    // Leave room for the gap itself when releasing the prefix.
                    if events.len() == OBSERVATION_CAPACITY - 1 {
                        events.pop_front();
                        *skipped = skipped.saturating_add(1);
                    }
                    events.push_back(observation);
                }
                AdmissionObservationState::Open => {
                    let _ = gate.sender.send(observation);
                }
                AdmissionObservationState::Closed => {}
            }
        })
    }
    pub(super) fn publish(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let AdmissionObservationState::Pending { events, skipped } =
            std::mem::replace(&mut *state, AdmissionObservationState::Open)
        else {
            panic!("admission observation prefix is released once")
        };
        // Never called beneath custody. Serializing sends with the observer's
        // private gate prevents new events from overtaking this retained prefix.
        if skipped != 0 {
            let _ = self.sender.send(Observation::Gap { skipped });
        }
        for event in events {
            let _ = self.sender.send(event);
        }
    }
    pub(super) fn discard(&self) {
        let previous = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *state, AdmissionObservationState::Closed)
        };
        drop(previous);
    }
}

pub(super) fn turn_observer(
    observations: broadcast::Sender<Observation>,
    operation: OperationId,
) -> impl FnMut(AgentEvent) + Send + 'static {
    move |event| {
        let _ = observations.send(Observation::Event {
            operation,
            event: Arc::new(event),
        });
    }
}

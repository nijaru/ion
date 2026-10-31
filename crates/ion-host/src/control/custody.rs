//! Pending input payloads and atomic acknowledgement, separate from execution.
use super::*;

// Snapshot captures retain values across release of the lifecycle mutex. M only
// needs Send: its own mutex permits a borrowed projection without requiring Sync.
// Only the controlling owner's exclusive methods extract or mutate these values;
// Core acknowledgement and steering producers change custody, never their contents.
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
    Ready(AcceptedInput<InputId>),
    // Root dispatch keeps its message reservation here; transferred steering
    // carries that reservation into Core's inbox instead.
    Transferred {
        _reservation: Option<InputReservation>,
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
    pub(super) fn take_ready(&mut self) -> Option<AcceptedInput<InputId>> {
        if !matches!(self.content, InputContent::Ready(_)) {
            return None;
        }
        // No await or fallible publication between taking ready content and
        // handing it off. The original remains recoverable until Core commits.
        let InputContent::Ready(prepared) = std::mem::replace(
            &mut self.content,
            InputContent::Transferred { _reservation: None },
        ) else {
            unreachable!("matched ready content")
        };
        Some(prepared)
    }
}

pub(super) struct Custody<M> {
    pub(super) inputs: BTreeMap<InputId, Input<M>>,
    pub(super) updates: VecDeque<SnapshotValue<ControlUpdate<M>>>,
    pub(super) steering_order: VecDeque<InputId>,
    pub(super) occupied: usize,
    pub(super) steering_target: Option<SteeringTarget>,
}
impl<M> Default for Custody<M> {
    fn default() -> Self {
        Self {
            inputs: BTreeMap::new(),
            updates: VecDeque::new(),
            steering_order: VecDeque::new(),
            occupied: 0,
            steering_target: None,
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
        // Drop all pending payload/reservations before fallible publication.
        // Metadata remains separately budgeted until reliable settlement delivery.
        input.state = InputState::Accepted(acceptance);
        if let Some(status) = &input.steering_status {
            status.publish(SteeringStatus::Committed { acceptance });
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
            panic!("committed input cannot be returned");
        };
        if let Some(status) = &input.steering_status {
            status.publish(SteeringStatus::Returned {
                input: pending.original.clone(),
                reason: ReturnSnapshot::from(&reason),
            });
        }
        self.push_update(ControlUpdate::Returned(ReturnedInput {
            id,
            delivery: input.delivery,
            input: pending.original,
            metadata: input.metadata.into_inner(),
            reason,
            preparation_error: None,
            reservation: Some(input.reservation),
        }));
    }

    pub(super) fn pop(&mut self) -> Option<ControlUpdate<M>> {
        let mut update = self.updates.pop_front()?.into_inner();
        self.occupied -= update.release();
        Some(update)
    }
}

pub(super) fn turn_observer<M: Send + 'static>(
    custody: Arc<Mutex<Custody<M>>>,
    observations: broadcast::Sender<Observation>,
    root: Option<InputId>,
    operation: OperationId,
) -> impl FnMut(AgentEvent) + Send + 'static {
    move |event| {
        match &event {
            AgentEvent::TurnAccepted { turn } => {
                lock(&custody).accept(root.expect("new Turn retains a root"), operation, *turn);
            }
            AgentEvent::SteeringCommitted { turn, .. } => {
                let mut custody = lock(&custody);
                let id = custody
                    .steering_order
                    .pop_front()
                    .expect("Core commits controlled FIFO steering");
                custody.accept(id, operation, *turn);
            }
            AgentEvent::TurnEnded { .. } => {
                let mut custody = lock(&custody);
                if custody
                    .steering_target
                    .as_ref()
                    .is_some_and(|target| target.operation == operation)
                {
                    custody.steering_target = None;
                }
            }
            _ => {}
        }
        let _ = observations.send(Observation::Event {
            operation,
            event: Arc::new(event),
        });
    }
}

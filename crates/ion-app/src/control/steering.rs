//! Fenced producers for the controlling owner's existing input custody.
use super::*;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SteeringStatus {
    /// Local custody only, not evidence of committed model delivery.
    Queued,
    Committed {
        acceptance: InputAcceptance,
    },
    Returned {
        input: UserInput,
        reason: ReturnSnapshot,
    },
    /// Commit disposition is unresolved; inspect Session facts before retrying.
    Unknown,
}

/// A projection of shared input custody. Keeping it does not retain a Session writer.
#[derive(Clone)]
pub struct SteeringReceipt {
    pub id: InputId,
    status: watch::Receiver<SteeringStatus>,
    core: InputReceipt,
    operation: OperationId,
    core_closed: bool,
}
impl SteeringReceipt {
    pub fn status(&self) -> SteeringStatus {
        match self.core.status() {
            InputStatus::Committed(commit) => SteeringStatus::Committed {
                acceptance: InputAcceptance {
                    operation: self.operation,
                    turn: commit.turn,
                },
            },
            InputStatus::Unresolved => SteeringStatus::Unknown,
            _ => self.status.borrow().clone(),
        }
    }
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        if self.core_closed {
            return self.status.changed().await;
        }
        tokio::select! {
            result = self.core.changed() => {
                if result.is_err() { self.core_closed = true; }
                // Closing the Core sender itself changes disposition. Read the
                // final status once, then wait only on the Host monitor.
                Ok(())
            }
            result = self.status.changed() => result,
        }
    }
}

#[derive(Clone)]
pub(super) struct SteeringMonitor {
    status: watch::Sender<SteeringStatus>,
    changed: Option<std::task::Waker>,
}
impl SteeringMonitor {
    pub fn publish(&self, status: SteeringStatus) {
        self.status.send_replace(status);
        if let Some(changed) = &self.changed {
            changed.wake_by_ref();
        }
    }
}

pub(super) struct SteeringTarget {
    pub operation: OperationId,
    pub stop: CancellationToken,
    pub fence: ion_core::OperationFence,
}

/// A live, literal-input grant for exactly one model operation. It cannot start
/// another Turn or regain authority when the owner changes bindings.
pub struct TurnSteering<M> {
    custody: Arc<Mutex<Custody<M>>>,
    budget: InputBudget,
    limits: ion_core::AgentLimits,
    operation: OperationId,
    changed: Arc<tokio::sync::Notify>,
}
impl<M> Clone for TurnSteering<M> {
    fn clone(&self) -> Self {
        Self {
            custody: self.custody.clone(),
            budget: self.budget.clone(),
            limits: self.limits,
            operation: self.operation,
            changed: self.changed.clone(),
        }
    }
}
impl<M: Serialize + Send + 'static> TurnSteering<M> {
    /// Synchronously transfer custody, not commit. Failure returns the unchanged
    /// original; later stop/end returns queued input through both the receipt and
    /// the owner's ordinary reliable updates. Dropping a receipt never withdraws it.
    pub fn submit(
        &self,
        message: Message,
        metadata: M,
    ) -> Result<SteeringReceipt, Box<RejectedInput<M>>> {
        self.submit_observed(message, metadata, None, None)
    }
    pub(crate) fn submit_observed(
        &self,
        message: Message,
        metadata: M,
        changed: Option<std::task::Waker>,
        caller_stop: Option<&CancellationToken>,
    ) -> Result<SteeringReceipt, Box<RejectedInput<M>>> {
        let delivery = Delivery::Steer(self.operation);
        let input = UserInput::Message(message);
        // Validate/serialize caller data outside the custody lock. Recheck the
        // lease and capacity at the actual mutation, including racing producers.
        let admitted = (|| {
            let UserInput::Message(message) = &input else {
                unreachable!()
            };
            let reservation = self
                .budget
                .reserve(message, &(&input, &metadata), self.limits)?;
            Ok::<_, anyhow::Error>(reservation)
        })();
        let mut custody = lock(&self.custody);
        let admitted = admitted.and_then(|reservation| {
            ensure!(
                caller_stop.is_none_or(|stop| !stop.is_cancelled()),
                "steering stopped; child continues"
            );
            ensure!(
                custody
                    .steering_target
                    .as_ref()
                    .is_some_and(|target| target.operation == self.operation
                        && !target.stop.is_cancelled()
                        && !target.fence.is_finished()),
                "steering target is no longer accepting input"
            );
            ensure!(
                custody.occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            let id = InputId(identity());
            let UserInput::Message(message) = &input else {
                unreachable!("literal steering")
            };
            let prepared = self.budget.prepare(message.clone(), id, self.limits)?;
            Ok((id, reservation, prepared))
        });
        let (id, reservation, prepared) = match admitted {
            Ok(admitted) => admitted,
            Err(reason) => {
                return Err(Box::new(RejectedInput {
                    delivery,
                    input,
                    metadata,
                    reason,
                }));
            }
        };
        let core = prepared.receipt();
        let (status, receiver) = watch::channel(SteeringStatus::Queued);
        custody.occupied += 1;
        custody.inputs.insert(
            id,
            Input {
                delivery,
                metadata: SnapshotValue::new(metadata),
                reservation,
                state: InputState::Pending(Box::new(PendingInput {
                    original: input,
                    instructions: String::new(),
                    content: InputContent::Ready(prepared),
                    withdrawn: None,
                })),
                steering_status: Some(SteeringMonitor {
                    status,
                    changed: changed.clone(),
                }),
            },
        );
        custody.push_update(ControlUpdate::Input {
            id,
            delivery,
            phase: InputPhase::Queued,
        });
        drop(custody);
        // One permit survives a cancelled owner wait; no caller-owned execution
        // waker or second steering queue is introduced.
        self.changed.notify_one();
        if let Some(changed) = changed {
            changed.wake();
        }
        Ok(SteeringReceipt {
            id,
            status: receiver,
            core,
            operation: self.operation,
            core_closed: false,
        })
    }
}
impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn steering(&self) -> Option<TurnSteering<M>> {
        let custody = lock(&self.custody);
        let target = custody.steering_target.as_ref()?;
        if target.stop.is_cancelled() || target.fence.is_finished() {
            return None;
        }
        Some(TurnSteering {
            custody: self.custody.clone(),
            budget: self.budget.clone(),
            limits: self.setup.runtime.agent.limits(),
            operation: target.operation,
            changed: self.input_changes.clone(),
        })
    }
}

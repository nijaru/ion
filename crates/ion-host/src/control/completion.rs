//! Recipient policy and scheduling share the ordinary Session control owner.
use super::*;
use ion_core::{CompletionDelivery, CompletionSignal};

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn completion_delivery(&self) -> CompletionDelivery {
        self.completions.policy()
    }
    /// Changes revoke existing automatic delivery grants; they do not upgrade
    /// previously transferred work or retrieve/acknowledge any child report.
    pub fn set_completion_delivery(&self, policy: CompletionDelivery) -> Result<()> {
        ensure!(!self.closed, "session control is closed");
        if let Some(owner) = self.background() {
            owner.ensure_open()?;
        }
        self.completions.set_policy(policy);
        Ok(())
    }
    /// Read-only readiness invalidation, including while idle. This does not
    /// convey producer authority; only an explicitly issued target does that.
    pub fn completion_changes(&self) -> CompletionSignal {
        self.completions.inbox().signal()
    }
    /// Explicitly authorize a producer, including a peer, to notify this live
    /// receiver and current context. Transport/disclosure are independently owned.
    pub fn completion_target(
        &self,
        delivery: CompletionDelivery,
    ) -> Result<crate::CompletionTarget> {
        ensure!(
            !self.closed && !self.stop_requested,
            "receiving control is stopped or closed"
        );
        if let Some(owner) = self.background() {
            owner.ensure_open()?;
        }
        let position = self.session().context_position()?;
        let turn = self
            .active
            .as_ref()
            .and_then(|active| self.active_turn(active, &lock(&self.custody)));
        self.completions.target(position.head_entry, turn, delivery)
    }
    pub(super) fn turn_inbox(&self) -> Arc<SteeringInbox<InputId>> {
        Arc::new(
            SteeringInbox::new(self.setup.runtime.agent.limits(), self.budget.clone())
                .with_completions(self.completions.inbox()),
        )
    }
    pub(super) fn wake_is_runnable(&self, custody: &Custody<M>) -> bool {
        !self.closed
            && !self.stop_requested
            && custody.inputs.is_empty()
            && custody.updates.is_empty()
            && custody.occupied < MAX_ADMISSIONS
            && self.completions.inbox().has_wake()
    }
    pub(super) fn advance_completions(&mut self) {
        if self.closed
            || self.stop_requested
            || self.active.is_some()
            || self.starting.is_some()
            || self.preparing.is_some()
            || !self.completions.inbox().has_wake()
        {
            return;
        }
        {
            let custody = lock(&self.custody);
            // Already admitted human intent and reliable results have priority.
            if !self.wake_is_runnable(&custody) {
                return;
            }
        }
        let operation = OperationId(identity());
        let inbox = self.turn_inbox();
        let stop = self.work_stop();
        let turn_stop = self.bind_turn_stop(stop.clone());
        // Publish the operation/steering lease before completion callbacks can
        // clear it. This is the same fence used by keyed human admission.
        let mut custody = lock(&self.custody);
        let result = self.setup.runtime.agent.submit_completions(
            self.session().clone(),
            self.setup.resources.instructions().to_owned(),
            stop.clone(),
            inbox.clone(),
            turn_observer(
                self.custody.clone(),
                self.observations.clone(),
                None,
                operation,
            ),
        );
        match result {
            Ok(Some(ion_core::CompletionSubmission {
                turn,
                operation: work,
            })) => {
                custody.occupied += 1;
                custody.steering_target = Some(SteeringTarget { operation, stop });
                custody.push_update(ControlUpdate::Automated {
                    operation,
                    turn: Some(turn),
                });
                self.active = Some(Active {
                    id: operation,
                    turn: Some(turn),
                    root: None,
                    work: Work::Turn(work),
                    inbox,
                    automated: true,
                    metadata: None,
                    turn_stop: turn_stop.as_ref().map(|binding| binding.scope()),
                });
            }
            Ok(None) => {
                drop(custody);
                self.restore_turn_stop(turn_stop);
            }
            Err(error) => {
                drop(custody);
                self.restore_turn_stop(turn_stop);
                // Do not retry an admission/storage failure on every idle poll.
                self.request_stop();
                let mut custody = lock(&self.custody);
                custody.occupied += 1;
                custody.push_update(ControlUpdate::Automated {
                    operation,
                    turn: None,
                });
                custody.push_update(ControlUpdate::Settled(Settlement {
                    automated: true,
                    operation,
                    turn: None,
                    outcome: OperationOutcome::Turn(Err(error)),
                    stop_requested: true,
                    accepted: Vec::new(),
                    metadata: None,
                    reservation: None,
                }));
            }
        }
    }
}

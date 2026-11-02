//! Recipient policy and scheduling share the ordinary Session control owner.
use super::*;
use ion_core::{CompletionDelivery, CompletionSignal};

/// Read-only context captured for a grant, not producer authority. Finish under
/// exclusive control so Stop/rebinding cannot turn a stale read into a new grant.
pub struct CompletionTargetPlan {
    owner: uuid::Uuid,
    session: uuid::Uuid,
    inbox: Arc<ion_core::CompletionInbox>,
    revision: u64,
    operation: Option<OperationId>,
    turn: Option<u64>,
    anchor: u64,
    delivery: CompletionDelivery,
}

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
    /// Explicitly authorize a producer for the current committed context.
    pub async fn completion_target(
        &mut self,
        delivery: CompletionDelivery,
    ) -> Result<crate::CompletionTarget> {
        let plan = self.prepare_completion_target(delivery).await?;
        self.finish_completion_target(plan)
    }
    /// Owned read preparation for transports that must release their controller
    /// lock during I/O. No Store read or filesystem work occurs on this thread.
    pub fn prepare_completion_target(
        &self,
        delivery: CompletionDelivery,
    ) -> impl std::future::Future<Output = Result<CompletionTargetPlan>> + Send + 'static + use<M>
    {
        let prepared: Result<_> = (|| {
            self.ensure_completion_target_open()?;
            self.reconcile_admissions();
            let turn = self
                .active
                .as_ref()
                .and_then(|active| self.active_turn(active, &lock(&self.custody)));
            // Validate policy now without reserving any producer capacity.
            self.completions.target(0, turn, delivery)?;
            let inbox = self.completions.inbox();
            let revision = inbox.revision();
            Ok((
                self.session().reader()?,
                CompletionTargetPlan {
                    owner: self.owner,
                    session: self.session().id(),
                    inbox,
                    revision,
                    operation: self.current_operation(),
                    turn,
                    anchor: 0,
                    delivery,
                },
            ))
        })();
        async move {
            let (reader, mut plan) = prepared?;
            plan.anchor = reader.view().await?.context_position().head_entry;
            Ok(plan)
        }
    }
    pub fn finish_completion_target(
        &mut self,
        plan: CompletionTargetPlan,
    ) -> Result<crate::CompletionTarget> {
        self.ensure_completion_target_open()?;
        self.reconcile_admissions();
        let inbox = self.completions.inbox();
        let turn = self
            .active
            .as_ref()
            .and_then(|active| self.active_turn(active, &lock(&self.custody)));
        ensure!(
            plan.owner == self.owner
                && plan.session == self.session().id()
                && Arc::ptr_eq(&plan.inbox, &inbox)
                && plan.revision == inbox.revision()
                && plan.operation == self.current_operation()
                && plan.turn == turn,
            "completion target changed during context read; prepare again"
        );
        self.completions
            .target(plan.anchor, plan.turn, plan.delivery)
    }
    fn ensure_completion_target_open(&self) -> Result<()> {
        ensure!(
            !self.closed && !self.stop_requested,
            "receiving control is stopped or closed"
        );
        ensure!(
            self.admitting.is_none(),
            "receiving admission is not yet acknowledged"
        );
        if let Some(owner) = self.background() {
            owner.ensure_open()?;
        }
        Ok(())
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
            && self.binding.is_none()
            && custody.inputs.is_empty()
            && custody.updates.is_empty()
            && custody.occupied < MAX_ADMISSIONS
            && self.completions.inbox().has_wake()
    }
    pub(super) fn advance_completions(&mut self) {
        if self.closed
            || self.stop_requested
            || self.active.is_some()
            || self.admitting.is_some()
            || self.binding_running()
            || self.starting.is_some()
            || self.preparing.is_some()
            || self.mcp_reconnect.is_some()
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
        let agent = self.setup.runtime.agent.clone();
        let session = self.session().clone();
        let instructions = self.setup.resources.instructions().to_owned();
        let work_stop = stop.clone();
        let work_inbox = inbox.clone();
        self.begin_admission(
            operation,
            stop,
            inbox,
            AdmissionPayload::Completion,
            move |observer| async move {
                Ok(AdmissionOutput::Completion(
                    agent
                        .submit_completions(session, instructions, work_stop, work_inbox, observer)
                        .await?,
                ))
            },
        );
    }
}

//! Turn commands over committed state. Neither planning nor reconciliation
//! dispatches work. Only the journal owner can finish a committed change.
use super::{journal::WorkingJournal, *};

pub(super) trait TurnChange {
    type Receipt;
    fn entries(&self) -> &[SessionEntry];
    fn committed(self, tail: u64) -> Self::Receipt;
}

pub(super) enum AdmissionPlan {
    Existing(RequestReceipt),
    Write(StartTurn),
}

pub(super) struct StartTurn {
    entries: Vec<SessionEntry>,
    interrupted: usize,
}

impl TurnChange for StartTurn {
    type Receipt = TurnAdmission;

    fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    fn committed(mut self, tail: u64) -> TurnAdmission {
        let Some(SessionEntry::TurnStarted { turn, input, .. }) = self.entries.pop() else {
            unreachable!("an admission ends with its TurnStarted fact");
        };
        TurnAdmission::New {
            turn,
            entry: tail,
            interrupted: self.interrupted,
            input,
        }
    }
}

pub(super) struct ContinueTurn {
    entries: Vec<SessionEntry>,
    prior: RecoveryPoint,
    interrupted: usize,
}

impl TurnChange for ContinueTurn {
    type Receipt = (RecoveryPoint, usize);

    fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    fn committed(self, tail: u64) -> Self::Receipt {
        (
            RecoveryPoint {
                checkpoint: tail,
                ..self.prior
            },
            self.interrupted,
        )
    }
}

impl WorkingJournal {
    pub(super) fn plan_admission(
        &self,
        input: Message,
        model: ModelRef,
        request: Option<RequestKey>,
        recovery: Option<RecoveryBinding>,
        completion: Vec<crate::CompletionNotice>,
        reconcile: impl FnOnce(&RequestKey, &Message) -> Result<Option<RequestReceipt>, SessionError>,
    ) -> Result<AdmissionPlan, SessionError> {
        if !valid_user_message(&input)
            || (!completion.is_empty() && input != crate::completion::notice_message(&completion))
        {
            return Err(SessionError::InvalidUserInput);
        }
        if let Some(key) = &request
            && let Some(receipt) = reconcile(key, &input)?
        {
            return Ok(AdmissionPlan::Existing(receipt));
        }
        if recovery
            .as_ref()
            .is_some_and(|binding| binding.session != self.identity().session)
        {
            return Err(SessionError::RecoveryAuthorityChanged);
        }
        let turn = self
            .state
            .last_id
            .checked_add(1)
            .ok_or(SessionError::TurnIdExhausted)?;
        let mut entries = self.interrupted_operation_entries();
        entries.push(SessionEntry::TurnStarted {
            turn,
            input,
            model,
            completion,
            request,
            recovery,
        });
        Ok(AdmissionPlan::Write(StartTurn {
            entries,
            interrupted: self.state.pending.len(),
        }))
    }

    pub(super) fn plan_continuation(
        &self,
        expected: RecoveryPoint,
        binding: &RecoveryBinding,
    ) -> Result<ContinueTurn, SessionError> {
        let session = self.identity().session;
        if expected.session != session
            || self.state.active != Some(expected.turn)
            || expected.checkpoint != self.state.sequence
        {
            return Err(SessionError::StaleRecoveryPoint);
        }
        if binding.session != session || self.state.recovery.as_ref() != Some(binding) {
            return Err(SessionError::RecoveryAuthorityChanged);
        }
        let mut entries = vec![SessionEntry::TurnResumed {
            turn: expected.turn,
            checkpoint: expected.checkpoint,
        }];
        entries.extend(self.orphaned_backgrounds(session));
        entries.extend(pending_results(
            expected.turn,
            &self.state.pending,
            &BTreeSet::new(),
        ));
        Ok(ContinueTurn {
            entries,
            prior: expected,
            interrupted: self.state.pending.len(),
        })
    }

    pub(super) fn interrupted_operation_entries(&self) -> Vec<SessionEntry> {
        let state = &self.state;
        let mut entries = self.orphaned_backgrounds(self.identity().session);
        if let Some(shell) = &state.pending_shell {
            entries.push(SessionEntry::UserShellSettled {
                admission_entry: shell.admission_entry,
                outcome: UserShellOutcome::Unknown,
            });
        }
        if let Some(turn) = state.active {
            entries.extend(pending_results(turn, &state.pending, &BTreeSet::new()));
            entries.push(SessionEntry::TurnEnded {
                turn,
                reason: TurnEndReason::Interrupted,
            });
        }
        entries
    }
}

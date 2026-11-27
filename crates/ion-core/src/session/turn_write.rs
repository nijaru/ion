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

/// A typed mutation and its receipt. Only a journal owner can release the
/// receipt, after applying this exact batch to the committed projection.
pub(super) struct TurnRecords<R> {
    entries: Vec<SessionEntry>,
    receipt: R,
}

impl<R> TurnChange for TurnRecords<R> {
    type Receipt = R;

    fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    fn committed(self, _: u64) -> R {
        self.receipt
    }
}

impl WorkingJournal {
    pub(super) fn plan_request(
        &self,
        turn: u64,
        model: ModelRef,
        context: ModelContextSnapshot,
    ) -> Result<TurnRecords<bool>, SessionError> {
        if self.state.active != Some(turn) || !self.state.pending.is_empty() {
            return Err(SessionError::InvalidHistory);
        }
        let mut entries = Vec::new();
        // The first observed assistant establishes the initial effective
        // model; preparing a request does not establish an execution fact.
        if self
            .state
            .last_effective_model
            .as_ref()
            .is_some_and(|previous| previous != &model)
        {
            entries.push(SessionEntry::EffectiveModelChanged { turn, model });
        }
        if self.state.last_context.as_ref() != Some(&context) {
            entries.push(SessionEntry::ModelContextChanged { turn, context });
        }
        Ok(TurnRecords {
            receipt: !entries.is_empty(),
            entries,
        })
    }

    pub(super) fn plan_assistant(
        &self,
        turn: u64,
        assistant: AssistantRecord,
        steering: Vec<Message>,
        completion: Vec<crate::CompletionNotice>,
    ) -> Result<TurnRecords<(CommittedAssistant, CommittedInputEntries)>, SessionError> {
        let closes = steering.is_empty()
            && completion.is_empty()
            && !assistant
                .message
                .content
                .iter()
                .any(|part| matches!(part, Content::ToolCall(_)));
        let entry = self.next_entry()?;
        let mut entries = vec![assistant.into_entry(turn, ResponseTermination::Completed)?];
        let end = if closes {
            entries.push(SessionEntry::TurnEnded {
                turn,
                reason: TurnEndReason::Completed,
            });
            Some(entry.checked_add(1).ok_or(SessionError::InvalidHistory)?)
        } else {
            None
        };
        let inputs = add_inputs(&mut entries, entry, turn, steering, completion)?;
        Ok(TurnRecords {
            entries,
            receipt: (CommittedAssistant { entry, end }, inputs),
        })
    }

    pub(super) fn plan_truncated_assistant(
        &self,
        turn: u64,
        assistant: AssistantRecord,
    ) -> Result<TurnRecords<(u64, Vec<crate::ToolSettlement>)>, SessionError> {
        let results = assistant.message.content.iter().filter_map(|part| match part {
            Content::ToolCall(call) => Some(crate::ToolSettlement {
                call_id: call.id.clone(),
                name: call.name.clone(),
                outcome: crate::ToolOutcome::NotDispatched {
                    reason: "the model response hit the output token limit and its arguments may be truncated. Reissue the complete call.".into(),
                },
            }),
            _ => None,
        }).collect::<Vec<_>>();
        if results.is_empty() {
            return Err(SessionError::InvalidHistory);
        }
        let mut entries = vec![assistant.into_entry(
            turn,
            ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
        )?];
        entries.extend(
            results
                .iter()
                .cloned()
                .map(|result| SessionEntry::ToolResult { turn, result }),
        );
        Ok(TurnRecords {
            entries,
            receipt: (self.next_entry()?, results),
        })
    }

    pub(super) fn plan_inputs(
        &self,
        turn: u64,
        steering: Vec<Message>,
        completion: Vec<crate::CompletionNotice>,
    ) -> Result<TurnRecords<CommittedInputEntries>, SessionError> {
        let mut entries = Vec::new();
        let receipt = add_inputs(&mut entries, self.next_entry()?, turn, steering, completion)?;
        Ok(TurnRecords { entries, receipt })
    }

    pub(super) fn plan_settlement(
        &self,
        turn: u64,
        occurrence: crate::ToolOccurrence,
        result: crate::ToolSettlement,
        context: Option<ModelContextSnapshot>,
    ) -> Result<TurnRecords<()>, SessionError> {
        if self.state.active != Some(turn)
            || !self.state.pending.iter().any(|pending| {
                pending.occurrence == occurrence
                    && pending.call_id == result.call_id
                    && pending.name == result.name
            })
        {
            return Err(SessionError::InvalidHistory);
        }
        let mut entries = vec![SessionEntry::ToolResult { turn, result }];
        if let Some(context) = context {
            entries.push(SessionEntry::ModelContextChanged { turn, context });
        }
        Ok(TurnRecords {
            entries,
            receipt: (),
        })
    }

    pub(super) fn plan_turn_end(
        &self,
        turn: u64,
        reason: TurnEndReason,
        unstarted: &BTreeSet<String>,
    ) -> Result<TurnRecords<TurnClosure>, SessionError> {
        let pending = &self.state.pending;
        if self.state.active != Some(turn)
            || unstarted
                .iter()
                .any(|id| !pending.iter().any(|call| &call.call_id == id))
        {
            return Err(SessionError::InvalidHistory);
        }
        let settlements = pending
            .iter()
            .map(|call| (pending_settlement(call, unstarted), call.activity.clone()))
            .collect();
        let mut entries = pending_results(turn, pending, unstarted);
        entries.push(SessionEntry::TurnEnded { turn, reason });
        let entry = self
            .state
            .sequence
            .checked_add(entries.len() as u64)
            .ok_or(SessionError::InvalidHistory)?;
        Ok(TurnRecords {
            entries,
            receipt: TurnClosure { entry, settlements },
        })
    }

    fn next_entry(&self) -> Result<u64, SessionError> {
        self.state
            .sequence
            .checked_add(1)
            .ok_or(SessionError::InvalidHistory)
    }
}

fn add_inputs(
    entries: &mut Vec<SessionEntry>,
    first_entry: u64,
    turn: u64,
    steering: Vec<Message>,
    completion: Vec<crate::CompletionNotice>,
) -> Result<CommittedInputEntries, SessionError> {
    if steering.is_empty() && completion.is_empty() {
        return Ok(CommittedInputEntries::default());
    }
    let start = first_entry
        .checked_add(entries.len() as u64)
        .ok_or(SessionError::InvalidHistory)?;
    let end = start
        .checked_add(steering.len() as u64)
        .ok_or(SessionError::InvalidHistory)?;
    entries.extend(
        steering
            .into_iter()
            .map(|input| SessionEntry::Steering { turn, input }),
    );
    let completion = if completion.is_empty() {
        None
    } else {
        entries.push(SessionEntry::Completion {
            turn,
            notices: completion,
        });
        Some(end)
    };
    Ok(CommittedInputEntries {
        steering: start..end,
        completion,
    })
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

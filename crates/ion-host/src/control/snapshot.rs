//! Complete pull inspection without transferring reliable result custody.
use super::*;
use ion_core::SessionView;

/// Orders replacement reads from one live owner, not a replay cursor for activity.
/// A different owner or Session requires a fresh view. `committed` counts the
/// Session facts in this read; it does not order volatile queue or display changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotCursor {
    pub owner: uuid::Uuid,
    pub read: u64,
    pub session: uuid::Uuid,
    pub committed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct InputAcceptance {
    pub operation: OperationId,
    pub turn: u64,
}

/// Host acknowledgement can lag durable admission. InFlight is not evidence that
/// input was unaccepted: committed Session facts remain the execution authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum InputCustodyPhase {
    PendingPreparation,
    Prepared,
    InFlight,
    Acknowledged(InputAcceptance),
}

#[derive(Debug, Serialize)]
pub struct InputSnapshot<T> {
    pub id: InputId,
    pub delivery: Delivery,
    pub phase: InputCustodyPhase,
    pub original: Option<UserInput>,
    pub metadata: T,
    pub withdrawal: Option<ReturnSnapshot>,
    /// Present during keyed admission; committed keys live in Session records.
    pub request_key: Option<ion_core::RequestKey>,
}

#[derive(Clone, Debug, Serialize)]
pub enum ReturnSnapshot {
    Stopped,
    Cleared,
    TurnEnded,
    Preparation(String),
    NotAccepted,
    PredecessorFailed,
}
impl From<&ReturnReason> for ReturnSnapshot {
    fn from(reason: &ReturnReason) -> Self {
        match reason {
            ReturnReason::Stopped => Self::Stopped,
            ReturnReason::Cleared => Self::Cleared,
            ReturnReason::TurnEnded => Self::TurnEnded,
            ReturnReason::Preparation(error) => Self::Preparation(error.to_string()),
            ReturnReason::NotAccepted => Self::NotAccepted,
            ReturnReason::PredecessorFailed => Self::PredecessorFailed,
        }
    }
}

#[derive(Debug, Serialize)]
pub enum OutcomeSnapshot {
    Turn(std::result::Result<String, String>),
    Compact(std::result::Result<bool, String>),
    Shell(std::result::Result<ToolOutput, String>),
}
impl From<&OperationOutcome> for OutcomeSnapshot {
    fn from(outcome: &OperationOutcome) -> Self {
        match outcome {
            OperationOutcome::Turn(result) => {
                Self::Turn(result.as_ref().cloned().map_err(ToString::to_string))
            }
            OperationOutcome::Compact(result) => {
                Self::Compact(result.as_ref().copied().map_err(ToString::to_string))
            }
            OperationOutcome::Shell(result) => {
                Self::Shell(result.as_ref().cloned().map_err(ToString::to_string))
            }
        }
    }
}

/// Inspection of reliable transitions still in Host custody. Activity is separate,
/// lossy and never part of this result-recovery channel.
#[derive(Debug, Serialize)]
pub enum RequestAdmissionSnapshot<T> {
    Started {
        receipt: ion_core::RequestReceipt,
    },
    Existing {
        receipt: ion_core::RequestReceipt,
        metadata: T,
    },
    Rejected {
        input: UserInput,
        metadata: T,
        reason: String,
    },
    Unresolved {
        session: uuid::Uuid,
        key: ion_core::RequestKey,
        operation: OperationId,
        input: UserInput,
        metadata: T,
        reason: String,
    },
}

#[derive(Debug, Serialize)]
pub struct RecoveryFailureSnapshot<T> {
    pub point: ion_core::RecoveryPoint,
    pub metadata: T,
    pub unresolved: bool,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct BindingSnapshot {
    pub operation: crate::BindingOperationId,
    pub result: Option<std::result::Result<crate::BindingResult, String>>,
    pub stop_requested: bool,
}

#[derive(Debug, Serialize)]
pub enum UpdateSnapshot<T> {
    BindingFinished(BindingSnapshot),
    RequestAdmission {
        id: InputId,
        decision: RequestAdmissionSnapshot<T>,
    },
    RecoveryAdmission {
        operation: OperationId,
        result: std::result::Result<ion_core::RecoveryPoint, RecoveryFailureSnapshot<T>>,
    },
    Automated {
        operation: OperationId,
        turn: Option<u64>,
    },
    Input {
        id: InputId,
        delivery: Delivery,
        phase: InputPhase,
    },
    Dispatched {
        id: InputId,
        operation: OperationId,
    },
    Accepted {
        id: InputId,
        acceptance: InputAcceptance,
    },
    Returned {
        id: InputId,
        delivery: Delivery,
        input: UserInput,
        metadata: T,
        reason: ReturnSnapshot,
        preparation_error: Option<String>,
    },
    Unresolved {
        session: uuid::Uuid,
        id: InputId,
        delivery: Delivery,
        operation: OperationId,
        input: UserInput,
        metadata: T,
    },
    McpReconnectFinished {
        operation: OperationId,
        server: String,
        metadata: T,
        result: std::result::Result<crate::McpReconnectReport, String>,
        stop_requested: bool,
    },
    Settled {
        automated: bool,
        operation: OperationId,
        outcome: OutcomeSnapshot,
        stop_requested: bool,
        turn: Option<u64>,
        accepted: Vec<(InputId, InputAcceptance, T)>,
        metadata: Option<T>,
    },
}

/// Correlation for in-flight user management, independent of Core work.
#[derive(Debug, Serialize)]
pub struct McpReconnectSnapshot<T> {
    pub operation: OperationId,
    pub server: String,
    pub metadata: T,
    pub stop_requested: bool,
}

/// Privileged, complete pull view; NOT a safe default network serialization.
/// Session data includes private shell context, provider replay and image bytes.
/// The application authorizes the entire view and selects metadata disclosure.
///
/// Custody is sampled first; the owned committed read follows. Thus every
/// captured Host acknowledgement is covered by `session`; facts may
/// be ahead of Host acknowledgement/join. This is a causal read, not an atomic
/// Core/Host transaction or evidence that retained work is physically running.
/// Always replace a prior snapshot. Activity deltas have no cursor correspondence.
#[derive(Debug)]
pub struct ControlSnapshot<T> {
    pub cursor: SnapshotCursor,
    pub session: SessionView,
    pub state: ControlState,
    pub inputs: Vec<InputSnapshot<T>>,
    /// Shell/compaction/continuation correlation, separate from input metadata.
    pub operation_metadata: Option<(OperationId, T)>,
    pub binding: Option<BindingSnapshot>,
    pub mcp_reconnect: Option<McpReconnectSnapshot<T>>,
    /// Independently sampled management catalogue, not a remote health check.
    pub mcp_servers: Option<Vec<crate::McpServerSnapshot>>,
    /// Live-only question custody, not a durable Session fact or answer queue.
    pub pending_question: Option<crate::PendingQuestion>,
    pub updates: Vec<UpdateSnapshot<T>>,
    pub cleanup_errors: Vec<String>,
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    /// Subscribe before capturing a complete replacement view. Activity is still
    /// lossy and has no replay correspondence to the snapshot cursor.
    pub async fn attach<T>(
        &mut self,
        metadata: impl FnMut(&M) -> T,
    ) -> Result<(ControlSnapshot<T>, ControlObserver)> {
        let observer = self.observe();
        Ok((self.snapshot(metadata).await?, observer))
    }

    /// Read without accepting/consuming any lifecycle result. `metadata` borrows
    /// the originals; neither Clone, Sync nor Deserialize is required on M.
    pub async fn snapshot<T>(
        &mut self,
        metadata: impl FnMut(&M) -> T,
    ) -> Result<ControlSnapshot<T>> {
        self.prepare_snapshot(metadata)?.await
    }

    /// Capture and project the Host cut synchronously, then return an owned read
    /// that does not borrow this controller. Poll it alongside control progress,
    /// Stop and transport I/O. Bound concurrent full reads in the caller.
    ///
    /// Projection runs outside lifecycle locks, before the committed read. It may
    /// submit steering; that input belongs to a later Host cut. All metadata
    /// captures are released before returning: the controller can transfer exact
    /// non-Clone metadata while the read is pending. Projection must not block.
    /// History and snapshot materialization have no aggregate memory quota.
    pub fn prepare_snapshot<T, F>(
        &mut self,
        mut metadata: F,
    ) -> Result<impl std::future::Future<Output = Result<ControlSnapshot<T>>> + use<M, T, F>>
    where
        F: FnMut(&M) -> T,
    {
        let reader = self.setup.session.reader()?;
        let read = self
            .snapshot_sequence
            .checked_add(1)
            .context("snapshot sequence exhausted")?;
        self.reconcile_admissions();
        let mcp_servers = self.mcp_servers();
        let custody = lock(&self.custody);
        let state = self.state_with(&custody);
        let mut inputs = custody
            .inputs
            .iter()
            .map(|(&id, input)| {
                let (phase, original, withdrawal) = match &input.state {
                    InputState::Accepted(acceptance) => {
                        (InputCustodyPhase::Acknowledged(*acceptance), None, None)
                    }
                    InputState::Pending(pending) => (
                        match pending.content {
                            InputContent::Images(_) => InputCustodyPhase::PendingPreparation,
                            InputContent::Ready(_) => InputCustodyPhase::Prepared,
                            InputContent::Transferred { .. } => InputCustodyPhase::InFlight,
                        },
                        Some(pending.original.clone()),
                        pending.withdrawn.as_ref().map(ReturnSnapshot::from),
                    ),
                };
                InputSnapshot {
                    id,
                    delivery: input.delivery,
                    phase,
                    original,
                    metadata: input.metadata.clone(),
                    withdrawal,
                    request_key: None,
                }
            })
            .collect::<Vec<_>>();
        if let Some(admitting) = &self.admitting
            && let AdmissionPayload::Request {
                id,
                key,
                original,
                metadata,
                ..
            } = &admitting.payload
        {
            inputs.push(InputSnapshot {
                id: *id,
                delivery: Delivery::Start,
                phase: InputCustodyPhase::InFlight,
                original: Some(original.clone()),
                metadata: metadata.clone(),
                withdrawal: None,
                request_key: Some(key.clone()),
            });
            inputs.sort_by_key(|input| input.id);
        }
        let updates: Vec<_> = custody.updates.iter().cloned().collect();
        let pending_question = self.pending_question();
        let mcp_reconnect = self
            .mcp_reconnect
            .as_ref()
            .map(|reconnect| (reconnect, reconnect.work.is_stopped()));
        // Retained handles protect projection only. Never keep them across the
        // asynchronous read: result delivery may need unique metadata custody.
        drop(custody);
        let inputs = inputs
            .into_iter()
            .map(|input| InputSnapshot {
                id: input.id,
                delivery: input.delivery,
                phase: input.phase,
                original: input.original,
                metadata: input.metadata.with(&mut metadata),
                withdrawal: input.withdrawal,
                request_key: input.request_key,
            })
            .collect();
        let operation_metadata = self
            .active
            .as_ref()
            .and_then(|active| {
                active
                    .metadata
                    .as_ref()
                    .map(|(value, _)| (active.id, metadata(value)))
            })
            .or_else(|| {
                let admitting = self.admitting.as_ref()?;
                let AdmissionPayload::Recovery {
                    metadata: value, ..
                } = &admitting.payload
                else {
                    return None;
                };
                Some((admitting.operation, value.with(&mut metadata)))
            });
        let binding = self.binding.as_ref().map(|binding| BindingSnapshot {
            operation: binding.operation(),
            result: match binding {
                crate::binding::BindingWork::Running { .. } => None,
                crate::binding::BindingWork::Finished(finished) => Some(
                    finished
                        .result
                        .as_ref()
                        .cloned()
                        .map_err(ToString::to_string),
                ),
            },
            stop_requested: binding.is_stopped(),
        });
        let mcp_reconnect = mcp_reconnect.map(|(reconnect, stop_requested)| McpReconnectSnapshot {
            operation: reconnect.id,
            server: reconnect.server.clone(),
            metadata: metadata(&reconnect.metadata),
            stop_requested,
        });
        let updates = updates
            .into_iter()
            .filter_map(|update| update.with(|update| update_snapshot(update, &mut metadata)))
            .collect();
        self.snapshot_sequence = read;
        let owner = self.owner;
        let cleanup_errors = self
            .cleanup_errors
            .iter()
            .map(ToString::to_string)
            .collect();
        Ok(async move {
            let session = reader.view().await?;
            let committed = u64::try_from(session.entries.len())?;
            Ok(ControlSnapshot {
                cursor: SnapshotCursor {
                    owner,
                    read,
                    session: session.id,
                    committed,
                },
                session,
                state,
                inputs,
                operation_metadata,
                binding,
                mcp_reconnect,
                mcp_servers,
                pending_question,
                updates,
                cleanup_errors,
            })
        })
    }

    /// Drive preparation, joining and explicit follow-ups WITHOUT consuming
    /// reliable updates or releasing their reservations. A service retains this
    /// owner outside connection scope. Call only while busy to avoid idle spin.
    /// Cancelling this borrowed wait cannot abandon work/results. Activity may be
    /// discarded; full pull snapshots remain available, even after subscriber loss.
    pub async fn maintain(&mut self) -> Option<Observation> {
        self.advance();
        self.drive(super::ActivityRetention::Discard).await
    }

    /// Explicit FIFO transfer to the controlling application, not an observer
    /// acknowledgement. Returned originals/metadata and settlements leave Host
    /// recovery custody here, releasing their capacity. Persist or deliver them
    /// before claiming recovery elsewhere. Passive snapshots never release them.
    pub fn take_update(&mut self) -> Option<ControlUpdate<M>> {
        self.reconcile_admissions();
        let update = { lock(&self.custody).pop() };
        update.or_else(|| {
            self.take_binding_result()
                .map(ControlUpdate::BindingFinished)
        })
    }
}

fn update_snapshot<M, T>(
    update: &ControlUpdate<M>,
    metadata: &mut impl FnMut(&M) -> T,
) -> Option<UpdateSnapshot<T>> {
    Some(match update {
        ControlUpdate::BindingFinished(finished) => {
            UpdateSnapshot::BindingFinished(BindingSnapshot {
                operation: finished.operation,
                result: Some(
                    finished
                        .result
                        .as_ref()
                        .cloned()
                        .map_err(ToString::to_string),
                ),
                stop_requested: finished.stop_requested,
            })
        }
        ControlUpdate::RequestAdmission(decision) => UpdateSnapshot::RequestAdmission {
            id: decision.id,
            decision: match &decision.result {
                Ok(RequestAdmission::Started { receipt, .. }) => {
                    RequestAdmissionSnapshot::Started {
                        receipt: receipt.clone(),
                    }
                }
                Ok(RequestAdmission::Existing {
                    receipt,
                    metadata: value,
                }) => RequestAdmissionSnapshot::Existing {
                    receipt: receipt.clone(),
                    metadata: metadata(value),
                },
                Ok(RequestAdmission::Unresolved(input)) => RequestAdmissionSnapshot::Unresolved {
                    session: input.session,
                    key: input.key.clone(),
                    operation: input.operation,
                    input: input.input.clone(),
                    metadata: metadata(&input.metadata),
                    reason: input.reason.to_string(),
                },
                Err(rejected) => RequestAdmissionSnapshot::Rejected {
                    input: rejected.input.clone(),
                    metadata: metadata(&rejected.metadata),
                    reason: rejected.reason.to_string(),
                },
            },
        },
        ControlUpdate::RecoveryAdmission(decision) => UpdateSnapshot::RecoveryAdmission {
            operation: decision.operation,
            result: decision
                .result
                .as_ref()
                .map(|admitted| admitted.point)
                .map_err(|failure| RecoveryFailureSnapshot {
                    point: failure.point,
                    metadata: metadata(&failure.metadata),
                    unresolved: failure.kind == AdmissionFailureKind::Unknown,
                    reason: failure.reason.to_string(),
                }),
        },
        ControlUpdate::Automated { operation, turn } => UpdateSnapshot::Automated {
            operation: *operation,
            turn: *turn,
        },
        ControlUpdate::Input {
            id,
            delivery,
            phase,
        } => UpdateSnapshot::Input {
            id: *id,
            delivery: *delivery,
            phase: *phase,
        },
        ControlUpdate::Dispatched { id, operation } => UpdateSnapshot::Dispatched {
            id: *id,
            operation: *operation,
        },
        ControlUpdate::Accepted {
            id,
            operation,
            turn,
        } => UpdateSnapshot::Accepted {
            id: *id,
            acceptance: InputAcceptance {
                operation: *operation,
                turn: *turn,
            },
        },
        ControlUpdate::Returned(input) => UpdateSnapshot::Returned {
            id: input.id,
            delivery: input.delivery,
            input: input.input.clone(),
            metadata: metadata(&input.metadata),
            reason: (&input.reason).into(),
            preparation_error: input.preparation_error.as_ref().map(ToString::to_string),
        },
        ControlUpdate::Unresolved(input) => UpdateSnapshot::Unresolved {
            session: input.session,
            id: input.id,
            delivery: input.delivery,
            operation: input.operation,
            input: input.input.clone(),
            metadata: metadata(&input.metadata),
        },
        ControlUpdate::McpReconnectFinished(finished) => UpdateSnapshot::McpReconnectFinished {
            operation: finished.operation,
            server: finished.server.clone(),
            metadata: metadata(&finished.metadata),
            result: finished
                .result
                .as_ref()
                .cloned()
                .map_err(ToString::to_string),
            stop_requested: finished.stop_requested,
        },
        ControlUpdate::Settled(settlement) => UpdateSnapshot::Settled {
            automated: settlement.automated,
            operation: settlement.operation,
            outcome: (&settlement.outcome).into(),
            stop_requested: settlement.stop_requested,
            turn: settlement.turn,
            accepted: settlement
                .accepted
                .iter()
                .map(|input| (input.id, input.acceptance, metadata(&input.metadata)))
                .collect(),
            metadata: settlement.metadata.as_ref().map(metadata),
        },
        ControlUpdate::Activity(_) | ControlUpdate::QuestionChanged | ControlUpdate::Idle => {
            return None;
        }
    })
}

//! Managed children use the ordinary Session engine, never a second loop.
use std::{path::PathBuf, sync::Arc};

use ion_ai::{BoxFuture, Message, ModelRef, Reasoning};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{AuthorityScope, CodingSession, RequestReceipt, SubmissionRequest, ToolOccurrence};

mod background;
pub use background::{BackgroundJob, BackgroundSlot, BackgroundStartError};
pub(crate) mod gateway;
#[cfg(test)]
mod tests;

/// Serializable coordinates are inspection data, not an execution permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationSite {
    pub session: uuid::Uuid,
    pub turn: u64,
    pub tool: ToolOccurrence,
    pub composed_child: Option<usize>,
}

/// Complete child application assertion. Context sharing is explicit and does
/// not inherit live configuration, workspace, permissions or credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationSpec {
    pub target: DelegationTarget,
    pub cwd: Option<PathBuf>,
    pub model: ModelRef,
    pub reasoning: Reasoning,
    /// Host-enforced runtime budget from child request submission, not a wait budget.
    pub execution_timeout_ms: Option<u64>,
    pub authority: AuthorityScope,
    pub input: Message,
}

impl DelegationSpec {
    pub(crate) fn valid(&self) -> bool {
        self.target.valid()
            && self.cwd.as_ref().is_none_or(|cwd| cwd.is_absolute())
            && self.reasoning.validate().is_ok()
            && self.execution_timeout_ms.is_none_or(|ms| ms != 0)
            && crate::session::valid_user_message(&self.input)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DelegationTarget {
    Fresh {
        directory: PathBuf,
    },
    /// Share the actual parent's settled context prefix as data, not authority.
    Fork {
        directory: PathBuf,
        point: crate::ForkPoint,
    },
    /// Locator and expected logical identity, never authority to continue work.
    /// A new captured application must authorize disclosure and a new request.
    Resume {
        path: PathBuf,
        session: uuid::Uuid,
    },
}

impl DelegationTarget {
    /// Resolve an existing conversation without taking execution ownership.
    /// Admission later acquires its exclusive writer and rechecks identity and
    /// settlement. A saved path/header cannot supply the application's authority.
    pub fn resume(path: impl AsRef<std::path::Path>) -> Result<Self, crate::CodingSessionError> {
        let path = path.as_ref().canonicalize()?;
        let session = CodingSession::inspect_id(&path)?;
        Ok(Self::Resume { path, session })
    }

    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Fresh { directory } => directory.is_absolute(),
            Self::Fork { directory, point } => {
                directory.is_absolute()
                    && match point {
                        crate::ForkPoint::BeforeTurn(turn) | crate::ForkPoint::AfterTurn(turn) => {
                            *turn != 0
                        }
                    }
            }
            Self::Resume { path, session } => {
                path.is_absolute() && path.parent().is_some() && !session.is_nil()
            }
        }
    }

    pub fn directory(&self) -> Option<&std::path::Path> {
        match self {
            Self::Fresh { directory } | Self::Fork { directory, .. } => Some(directory),
            Self::Resume { path, .. } => path.parent(),
        }
    }

    pub(crate) fn path(&self, child: uuid::Uuid) -> PathBuf {
        match self {
            Self::Fresh { directory } | Self::Fork { directory, .. } => {
                directory.join(format!("{child}.sqlite"))
            }
            Self::Resume { path, .. } => path.clone(),
        }
    }
}

/// A resume holds the actual exclusive writer before parent intent is committed.
/// New files are still materialized only after that intent.
pub(crate) enum ChildMaterialization {
    New(Vec<crate::SessionEntry>),
    Resume(Arc<CodingSession>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationOwnership {
    Foreground,
    /// Intended destination, not proof of transfer or a live owner.
    Background {
        owner: uuid::Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationRecord {
    /// One managed request, distinct from the reusable child conversation.
    pub run: uuid::Uuid,
    pub site: DelegationSite,
    pub child: uuid::Uuid,
    pub path: PathBuf,
    pub ownership: DelegationOwnership,
    pub spec: DelegationSpec,
}

impl DelegationRecord {
    /// Correlation data for this run's request, not permission to dispatch it.
    pub fn request_key(&self) -> crate::RequestKey {
        request_key(self.run)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DelegationOutcome {
    /// No surviving owner can establish dispatch or physical settlement.
    Unknown,
    /// Host control, preparation and Core work have joined. A missing receipt or
    /// Turn end is NOT successful execution, rollback or descendant quiescence.
    Joined {
        /// Host observed the execution deadline before child control settled.
        /// Distinct from wait expiry, explicit stop and unknown effects; cleanup
        /// has still joined, and its independent failures remain in `error`.
        execution_timed_out: bool,
        receipt: Option<RequestReceipt>,
        error: Option<String>,
        /// Visible text of the requested child's committed final response,
        /// observed only after successful full joining. No thinking or replay.
        answer: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationReceipt {
    pub record: DelegationRecord,
    /// Responsibility was committed to the destination before parent release.
    /// This is not liveness, completion or authority to resume after owner loss.
    pub transferred: bool,
    pub outcome: Option<DelegationOutcome>,
}

/// Ordinary joined child failure is available in its receipt. Infrastructure
/// failure closes the supervising scope: the parent gateway before release,
/// or the destination owner after transfer. Catching a tool error cannot suppress it.
pub enum DelegationFailure {
    Execution(String),
    Infrastructure(String),
}

/// Fully joined Host observation. Deadline provenance survives a simultaneous
/// execution or infrastructure failure; it is never inferred from error text.
pub struct DelegationCompletion {
    pub result: Result<Option<String>, DelegationFailure>,
    pub execution_timed_out: bool,
}

pub(crate) type DelegationWork = Box<
    dyn FnOnce(
            Arc<CodingSession>,
            SubmissionRequest,
            CancellationToken,
        ) -> BoxFuture<'static, DelegationCompletion>
        + Send,
>;

/// A cold, owned Host supervision callback. It must use the child Session's
/// ordinary engine and finish shared-control shutdown (including preparation)
/// before returning its original visible answer, if any. It must enforce any
/// specified execution budget and attest actual deadline expiry separately from
/// failure, stopping and fully joining rather than dropping started work. Core
/// observes that answer only with the requested completed Turn and successful full joining;
/// trusted Host code attests the output, just as it attests the environment.
/// Construction and dropping an unadmitted task execute no
/// child work. Core retains the callback independently of the tool's waiter.
/// Trusted host code supplies the assertion and actual captured environment;
/// schema/model equality does not attest credentials or executable identity.
pub struct DelegationTask {
    pub(crate) spec: DelegationSpec,
    pub(crate) work: DelegationWork,
    pub(crate) background: Option<Box<dyn BackgroundSlot>>,
}

impl DelegationTask {
    pub fn new(
        spec: DelegationSpec,
        work: impl FnOnce(
            Arc<CodingSession>,
            SubmissionRequest,
            CancellationToken,
        ) -> BoxFuture<'static, DelegationCompletion>
        + Send
        + 'static,
    ) -> Result<Self, String> {
        if !spec.valid() {
            return Err("invalid child specification".into());
        }
        Ok(Self {
            spec,
            work: Box::new(work),
            background: None,
        })
    }

    /// Explicit live transfer destination. Core reconciles first, then asks
    /// Host to reserve custody before committing intent. Exact repeats need no
    /// extra capacity; dropping unadmitted work releases any reservation.
    pub fn in_background(mut self, slot: impl BackgroundSlot + 'static) -> Self {
        self.background = Some(Box::new(slot));
        self
    }

    pub(crate) fn ownership(&self) -> DelegationOwnership {
        self.background
            .as_ref()
            .map_or(DelegationOwnership::Foreground, |slot| {
                DelegationOwnership::Background {
                    owner: slot.owner(),
                }
            })
    }
}

pub(crate) struct DelegationRequest {
    pub task: DelegationTask,
    pub reply: oneshot::Sender<Result<DelegationReceipt, String>>,
}

pub(crate) type AdmissionPort = mpsc::Sender<DelegationRequest>;

pub(crate) fn request_key(run: uuid::Uuid) -> crate::RequestKey {
    crate::RequestKey::new("ion.delegation".into(), run.to_string())
        .expect("UUID and fixed namespace satisfy request-key bounds")
}

/// One request namespace per managed run, not per reusable Session or transport.
pub(crate) fn request(record: &DelegationRecord) -> SubmissionRequest {
    SubmissionRequest {
        key: record.request_key(),
        input: record.spec.input.clone(),
    }
}

//! Managed foreground work uses the ordinary Session engine, never a second loop.
use std::{path::PathBuf, sync::Arc};

use ion_ai::{BoxFuture, Message, ModelRef, Reasoning};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{AuthorityScope, CodingSession, RequestReceipt, SubmissionRequest, ToolOccurrence};

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

/// Complete application assertion for a fresh child. No parent configuration,
/// conversation, workspace or credentials are implicitly inherited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForegroundSpec {
    pub directory: PathBuf,
    pub cwd: Option<PathBuf>,
    pub model: ModelRef,
    pub reasoning: Reasoning,
    pub authority: AuthorityScope,
    pub input: Message,
}

impl ForegroundSpec {
    pub(crate) fn valid(&self) -> bool {
        self.directory.is_absolute()
            && self.cwd.as_ref().is_none_or(|cwd| cwd.is_absolute())
            && self.reasoning.validate().is_ok()
            && crate::session::valid_user_message(&self.input)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationContext {
    Fresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationOwnership {
    Foreground,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub site: DelegationSite,
    pub child: uuid::Uuid,
    pub path: PathBuf,
    pub context: DelegationContext,
    pub ownership: DelegationOwnership,
    pub spec: ForegroundSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DelegationOutcome {
    /// No surviving owner can establish dispatch or physical settlement.
    Unknown,
    /// Host control, preparation and Core work have joined. A missing receipt or
    /// Turn end is NOT successful execution, rollback or descendant quiescence.
    Joined {
        receipt: Option<RequestReceipt>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationReceipt {
    pub record: DelegationRecord,
    pub outcome: Option<DelegationOutcome>,
}

/// Ordinary child failure is available to the parent tool. Infrastructure
/// failure additionally closes the Core gateway, even if tool code catches it.
pub enum ForegroundFailure {
    Execution(String),
    Infrastructure(String),
}

pub(crate) type ForegroundWork = Box<
    dyn FnOnce(
            Arc<CodingSession>,
            SubmissionRequest,
            CancellationToken,
        ) -> BoxFuture<'static, Result<(), ForegroundFailure>>
        + Send,
>;

/// A cold, owned Host supervision callback. It must use the child Session's
/// ordinary engine and finish shared-control shutdown (including preparation)
/// before returning. Construction and dropping an unadmitted task execute no
/// child work. Core retains the callback independently of the tool's waiter.
/// Trusted host code supplies the assertion and actual captured environment;
/// schema/model equality does not attest credentials or executable identity.
pub struct ForegroundTask {
    pub(crate) spec: ForegroundSpec,
    pub(crate) work: ForegroundWork,
}

impl ForegroundTask {
    pub fn new(
        spec: ForegroundSpec,
        work: impl FnOnce(
            Arc<CodingSession>,
            SubmissionRequest,
            CancellationToken,
        ) -> BoxFuture<'static, Result<(), ForegroundFailure>>
        + Send
        + 'static,
    ) -> Result<Self, String> {
        if !spec.valid() {
            return Err("invalid foreground child specification".into());
        }
        Ok(Self {
            spec,
            work: Box::new(work),
        })
    }
}

pub(crate) struct DelegationRequest {
    pub task: ForegroundTask,
    pub reply: oneshot::Sender<Result<DelegationReceipt, String>>,
}

pub(crate) type AdmissionPort = mpsc::Sender<DelegationRequest>;

pub(crate) fn request_key(child: uuid::Uuid) -> crate::RequestKey {
    crate::RequestKey::new("ion.delegation".into(), child.to_string())
        .expect("UUID and fixed namespace satisfy request-key bounds")
}

/// One child request namespace per recorded child identity, not a transport ID.
pub(crate) fn request(record: &DelegationRecord) -> SubmissionRequest {
    SubmissionRequest {
        key: request_key(record.child),
        input: record.spec.input.clone(),
    }
}

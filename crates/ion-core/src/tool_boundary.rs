//! Deterministic tool preparation and the separate host execution boundary.

use std::{collections::BTreeMap, sync::Arc};

use ion_ai::BoxFuture;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    AttemptId, AuthorityCeiling, InvocationId, PreparedAction, SemanticCompatibilityId, SessionId,
    ToolAttempt, ToolAttemptState, ToolBinding, ToolBindingId, ToolRecoveryPolicy,
    WorkspaceBinding,
};

/// Hard bounds apply even when a host configures larger model-facing limits.
pub const MAX_TOOL_RECORD_BYTES: usize = 64 * 1024;
pub const MAX_TOOL_ATTEMPTS: usize = 4;
pub(crate) const MAX_TOOL_BATCH: usize = 128;
pub(crate) const MAX_TOOL_BATCH_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub session: SessionId,
    pub invocation: InvocationId,
    pub attempt: AttemptId,
    pub effect_key: String,
    pub binding: ToolBinding,
    pub action: PreparedAction,
    pub workspace: WorkspaceBinding,
    pub ceiling: AuthorityCeiling,
    pub output_limit: usize,
}

/// Trusted host code. Preparation is pure; execution owns live policy, claims,
/// confinement and stop/join. Dropping a future is never evidence of termination.
/// Implementations must bound output while collecting it, not after buffering it.
pub trait ToolBoundary: Send + Sync {
    /// Exact semantic contract, including schema, realm and receipt interpretation.
    fn binding(&self) -> ToolBinding;
    fn executor(&self) -> SemanticCompatibilityId;

    /// Pure deterministic canonicalization. Called only for a new logical batch;
    /// retries/recovery use the persisted PreparedAction without re-preparation.
    fn prepare(&self, arguments: Value) -> Result<PreparedAction, ToolBoundaryError>;

    /// May narrow, never widen the frozen replay policy.
    fn permits_retry(&self) -> bool {
        false
    }

    /// Recheck current live authority and workspace identity/claims immediately
    /// before effects, inside the frozen ceiling. A cancelled token must prevent
    /// new admission. Return only after local execution is joined, or return explicit
    /// indeterminate evidence with ownership retained by the host supervisor.
    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolAttemptState>;

    /// Explicit resume only. An authoritative negative is valid only under the
    /// frozen Authoritative receipt contract; absence otherwise remains unknown.
    fn reconcile<'a>(
        &'a self,
        _execution: ToolExecution,
        _attempt: ToolAttempt,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async {
            ToolAttemptState::Indeterminate {
                reason: "execution evidence unavailable".into(),
                receipt: None,
            }
        })
    }
}

#[derive(Clone, Default)]
pub struct ToolBoundaries(Arc<BTreeMap<ToolBindingId, Arc<dyn ToolBoundary>>>);

impl std::fmt::Debug for ToolBoundaries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

impl ToolBoundaries {
    pub fn new(
        boundaries: impl IntoIterator<Item = Arc<dyn ToolBoundary>>,
    ) -> Result<Self, ToolBoundaryError> {
        let mut map = BTreeMap::new();
        for boundary in boundaries {
            if map.insert(boundary.binding().id, boundary).is_some() {
                return Err(ToolBoundaryError::Incompatible);
            }
        }
        Ok(Self(Arc::new(map)))
    }

    pub fn resolve(
        &self,
        binding: &ToolBinding,
        workspace: &WorkspaceBinding,
    ) -> Result<Arc<dyn ToolBoundary>, ToolBoundaryError> {
        let boundary = self
            .0
            .get(&binding.id)
            .ok_or(ToolBoundaryError::Unavailable)?;
        if boundary.binding() != *binding || boundary.executor().as_str() != workspace.backend {
            return Err(ToolBoundaryError::Incompatible);
        }
        Ok(Arc::clone(boundary))
    }
}

pub(crate) fn prepare_action(
    boundary: &dyn ToolBoundary,
    binding: &ToolBinding,
    arguments: Value,
) -> Result<PreparedAction, ToolBoundaryError> {
    bounded(&arguments)?;
    bounded(&binding.spec.input_schema)?;
    // Remote/file schema resolution is disabled at the dependency feature boundary.
    let validator = jsonschema::validator_for(&binding.spec.input_schema)
        .map_err(|_| ToolBoundaryError::InvalidSchema)?;
    if !validator.is_valid(&arguments) {
        return Err(ToolBoundaryError::InvalidArguments);
    }
    let action = boundary.prepare(arguments)?;
    bounded(&action)?;
    let expected = PreparedAction::new(
        action.binding.clone(),
        action.arguments.clone(),
        action.egress.clone(),
        action.workspace_revision,
        action.base_facts.clone(),
    )
    .map_err(|_| ToolBoundaryError::InvalidAction)?;
    if action != expected || action.binding != binding.id || action.egress != binding.egress {
        return Err(ToolBoundaryError::InvalidAction);
    }
    Ok(action)
}

pub(crate) fn bounded(value: &impl serde::Serialize) -> Result<(), ToolBoundaryError> {
    // Values originate in already-bounded model responses or trusted host boundaries.
    if serde_json::to_vec(value)
        .map_err(|_| ToolBoundaryError::InvalidAction)?
        .len()
        > MAX_TOOL_RECORD_BYTES
    {
        return Err(ToolBoundaryError::Capacity);
    }
    Ok(())
}

pub(crate) fn permits_retry(binding: &ToolBinding, attempts: &[ToolAttempt]) -> bool {
    attempts.is_empty()
        || (binding.recovery == ToolRecoveryPolicy::RepeatAfterNotStartedOrNoMutation
            && attempts.len() < MAX_TOOL_ATTEMPTS
            && attempts.iter().all(|a| match &a.state {
                ToolAttemptState::NotStarted { .. } => true,
                ToolAttemptState::Settled {
                    result,
                    effect: crate::EffectSummary::NoMutation,
                    retryable: true,
                    ..
                } => result.is_error,
                _ => false,
            }))
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolBoundaryError {
    #[error("frozen tool implementation unavailable")]
    Unavailable,
    #[error("tool implementation/executor contract mismatch")]
    Incompatible,
    #[error("unsupported or invalid frozen tool schema")]
    InvalidSchema,
    #[error("tool arguments fail frozen schema")]
    InvalidArguments,
    #[error("invalid prepared action")]
    InvalidAction,
    #[error("tool record exceeds hard capacity")]
    Capacity,
}

//! Explicit continuation authority. References are application assertions, not credentials.
use super::*;

/// Versioned identity of the application, principal, model route, environment and
/// grants reconstructed by a trusted caller. Equal schemas/names do not attest
/// equal executors or credentials. Change this reference when those bindings change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct AuthorityScope(String);

impl AuthorityScope {
    pub fn new(value: impl Into<String>) -> Result<Self, SessionError> {
        Self::try_from(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for AuthorityScope {
    type Error = SessionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(SessionError::InvalidAuthorityScope);
        }
        Ok(Self(value))
    }
}

/// Engine-owned declaration check, not proof of host authority. The owner also
/// prevents a context copy from inheriting the source Session's continuation grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryBinding {
    pub session: uuid::Uuid,
    pub authority: AuthorityScope,
    pub(crate) fingerprint: [u8; 32],
}

/// A particular committed checkpoint. Reusing it cannot authorize another attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPoint {
    pub session: uuid::Uuid,
    pub turn: u64,
    pub checkpoint: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryState {
    pub point: RecoveryPoint,
    pub authority: AuthorityScope,
}

impl journal::WorkingJournal {
    pub(super) fn recovery_state(&self) -> Option<RecoveryState> {
        let session = self.identity().session;
        let turn = self.state.active?;
        let binding = self
            .state
            .recovery
            .as_ref()
            .filter(|binding| binding.session == session)?;
        Some(RecoveryState {
            point: RecoveryPoint {
                session,
                turn,
                checkpoint: self.state.sequence,
            },
            authority: binding.authority.clone(),
        })
    }

    pub(super) fn recovery_for_receipt(
        &self,
        receipt: RequestReceipt,
    ) -> Result<Option<RecoveryState>, SessionError> {
        if receipt.session == self.identity().session
            && self.state.active == Some(receipt.turn)
            && receipt.end.is_none()
        {
            self.recovery_state()
                .map(Some)
                .ok_or(SessionError::RecoveryAuthorityChanged)
        } else {
            Ok(None)
        }
    }
}

impl Session {
    /// Passive eligibility only: not liveness, authentication or proof old effects
    /// stopped. Callers must explicitly choose whether to continue despite uncertainty.
    pub fn recovery_state(&self) -> Result<Option<RecoveryState>, SessionError> {
        let store = self.lock_store()?;
        Ok(store.working.recovery_state())
    }

    pub(super) fn recovery_for_request(
        &self,
        key: &RequestKey,
    ) -> Result<Option<RecoveryState>, SessionError> {
        let store = self.lock_store()?;
        if store.working.state.active.is_none() {
            return Ok(None);
        }
        match submission::request_receipt(&store.connection, self.id(), key, None)? {
            Some(receipt) => store.working.recovery_for_receipt(receipt),
            None => Ok(None),
        }
    }

    pub(crate) fn resume_turn(
        &self,
        expected: RecoveryPoint,
        binding: &RecoveryBinding,
    ) -> Result<(RecoveryPoint, usize), SessionError> {
        let mut store = self.lock_store()?;
        let change = store.working.plan_continuation(expected, binding)?;
        store.commit_turn_change(change)
    }
}

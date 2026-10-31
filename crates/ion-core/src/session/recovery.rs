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

impl Session {
    /// Passive eligibility only: not liveness, authentication or proof old effects
    /// stopped. Callers must explicitly choose whether to continue despite uncertainty.
    pub fn recovery_state(&self) -> Result<Option<RecoveryState>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(store
            .state
            .active
            .zip(store.state.recovery.as_ref())
            .and_then(|(turn, binding)| {
                (binding.session == self.id()).then(|| RecoveryState {
                    point: RecoveryPoint {
                        session: self.id(),
                        turn,
                        checkpoint: store.state.sequence,
                    },
                    authority: binding.authority.clone(),
                })
            }))
    }

    pub(crate) fn resume_turn(
        &self,
        expected: RecoveryPoint,
        binding: &RecoveryBinding,
    ) -> Result<(RecoveryPoint, usize), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if expected.session != self.id()
            || store.state.active != Some(expected.turn)
            || expected.checkpoint != store.state.sequence
        {
            return Err(SessionError::StaleRecoveryPoint);
        }
        if binding.session != self.id() || store.state.recovery.as_ref() != Some(binding) {
            return Err(SessionError::RecoveryAuthorityChanged);
        }
        let interrupted = store.state.pending.len();
        let mut entries = vec![SessionEntry::TurnResumed {
            turn: expected.turn,
            checkpoint: expected.checkpoint,
        }];
        entries.extend(store.orphaned_backgrounds(self.id()));
        entries.extend(pending_results(
            expected.turn,
            &store.state.pending,
            &BTreeSet::new(),
        ));
        append(&mut store, &entries)?;
        Ok((
            RecoveryPoint {
                checkpoint: store.state.sequence,
                ..expected
            },
            interrupted,
        ))
    }
}

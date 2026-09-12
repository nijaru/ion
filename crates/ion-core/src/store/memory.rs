use thiserror::Error;

use super::{SessionState, StateError, apply_mutation};
use crate::session::transaction::{Mutation, MutationBatch};
use crate::{CommitSeq, LocalSeq, SessionId};

#[derive(Debug)]
pub(crate) struct MemoryStore {
    state: SessionState,
}

impl MemoryStore {
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            state: SessionState::empty(session_id),
        }
    }

    pub(crate) fn state(&self) -> &SessionState {
        &self.state
    }

    pub(crate) fn commit(&mut self, batch: MutationBatch) -> Result<(), StoreError> {
        if batch.commit_seq.local_seq() != batch.last_seq {
            return Err(StoreError::CommitNotLast {
                commit_seq: batch.commit_seq,
                last_seq: batch.last_seq,
            });
        }
        if let Some(current) = self.state.last_seq
            && batch.last_seq <= current
        {
            return Err(StoreError::SequenceRegression {
                current,
                proposed: batch.last_seq,
            });
        }

        let mut next = self.state.clone();
        for mutation in &batch.mutations {
            apply_mutation(&mut next, mutation)?;
        }
        for mutation in &batch.mutations {
            if let Mutation::AdmitInput(input) = mutation {
                next.input_commits.insert(input.id, batch.commit_seq);
            }
        }
        next.last_seq = Some(batch.last_seq);
        next.last_commit = Some(batch.commit_seq);
        self.state = next;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum StoreError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("commit sequence {commit_seq} is not the batch's final local sequence {last_seq}")]
    CommitNotLast {
        commit_seq: CommitSeq,
        last_seq: LocalSeq,
    },
    #[error("local sequence regressed from {current:?} to {proposed:?}")]
    SequenceRegression {
        current: LocalSeq,
        proposed: LocalSeq,
    },
}

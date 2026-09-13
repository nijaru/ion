use super::{Persistence, StoreError};
use crate::LocalSeq;
use crate::session::transaction::MutationBatch;

/// Volatile commit sink for deterministic kernel use; no duplicate resident state.
#[derive(Debug, Default)]
pub(crate) struct MemoryStore {
    last_seq: Option<LocalSeq>,
}

impl Persistence for MemoryStore {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError> {
        if batch.commit_seq.local_seq() != batch.last_seq
            || self
                .last_seq
                .is_some_and(|current| batch.last_seq <= current)
        {
            return Err(StoreError("invalid commit sequence or empty batch".into()));
        }
        self.last_seq = Some(batch.last_seq);
        Ok(())
    }
}

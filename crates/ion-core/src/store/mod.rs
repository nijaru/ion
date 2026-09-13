mod memory;
pub(crate) mod sqlite;

pub(crate) use memory::MemoryStore;

use crate::session::transaction::MutationBatch;

/// Persistence accepts only a fully validated batch. It never decides readiness
/// or changes resident semantic state. Any error fences the owner conservatively.
pub(crate) trait Persistence: std::fmt::Debug + Send {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError>;
}

#[derive(Debug, thiserror::Error)]
#[error("persistence commit failed: {0}")]
pub(crate) struct StoreError(pub(crate) String);

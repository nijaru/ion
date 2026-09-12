use thiserror::Error;

use super::{ContextError, project};
use crate::{Entry, EntryId};

pub fn validate_fork_cutoff(entries: &[Entry], cutoff: EntryId) -> Result<(), ForkError> {
    let Some(position) = entries.iter().position(|entry| entry.id == cutoff) else {
        return Err(ForkError::InvisibleCutoff(cutoff));
    };
    project(&entries[..=position]).map_err(ForkError::IncompleteContext)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ForkError {
    #[error("fork cutoff {0} is not visible")]
    InvisibleCutoff(EntryId),
    #[error("fork cutoff does not form a complete provider-safe context: {0}")]
    IncompleteContext(#[from] ContextError),
}

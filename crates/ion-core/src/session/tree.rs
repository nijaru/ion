use crate::ids::EntryId;
use crate::operation::SessionEntry;

/// One immutable node in a session's semantic conversation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) id: EntryId,
    pub(crate) parent: Option<EntryId>,
    pub(crate) seq: u64,
    pub(crate) value: SessionEntry,
}

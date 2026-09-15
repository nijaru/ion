//! Transcript reads.
//!
//! Pages are bounded by the caller and validated here. A limit is clamped to a
//! documented bound rather than allowed to overflow when it is added to, which
//! is what a `usize::MAX` page size used to do.

use rusqlite::params;

use super::codec::decode;
use super::{SqliteStore, StoreError, id_from};
use crate::entry::{Entry, EntryKind};
use crate::error::Error;
use crate::store::Command;
use crate::view::EntryPage;
use crate::{ConversationId, EntryId};

/// The largest page a caller may request in one read.
pub const MAX_ENTRY_PAGE: u32 = 1_024;

/// Read a bounded page of one conversation's transcript.
pub(crate) struct ReadEntries {
    pub(crate) conversation: ConversationId,
    pub(crate) after: Option<EntryId>,
    pub(crate) limit: u32,
}

impl Command for ReadEntries {
    type Output = EntryPage;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        if self.limit == 0 {
            return Ok(EntryPage {
                entries: Vec::new(),
                has_more: false,
            });
        }
        let limit = self.limit.min(MAX_ENTRY_PAGE);
        // Fetch one extra row to learn whether a further page exists without
        // counting the whole conversation.
        let fetch = i64::from(limit) + 1;
        let after = self.after.map_or(0, EntryId::get);
        let connection = &store.connection;
        let mut statement = connection.prepare(
            "SELECT id, kind, data, projection FROM entries \
             WHERE conversation_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
        )?;
        let mut rows = statement.query(params![self.conversation.get(), after, fetch])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            let id = id_from::<EntryId>(row.get(0)?)?;
            let kind: String = row.get(1)?;
            let data: String = row.get(2)?;
            let projection: String = row.get(3)?;
            entries.push(Entry {
                id,
                conversation_id: self.conversation,
                kind: EntryKind::new(kind).map_err(|error| {
                    StoreError::Rejected(Error::Corrupt(format!(
                        "entry {id} has an invalid kind: {error}"
                    )))
                })?,
                data: decode(&data)?,
                projection: decode(&projection)?,
            });
        }
        let has_more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        Ok(EntryPage { entries, has_more })
    }
}

/// Read every entry of a conversation up to a cutoff, in order.
///
/// Request assembly needs the whole prefix, and the caller has already bounded
/// the request by bytes, so this is deliberately not paged.
pub(crate) struct ReadEntriesUpTo {
    pub(crate) conversation: ConversationId,
    pub(crate) cut: Option<EntryId>,
}

impl Command for ReadEntriesUpTo {
    type Output = Vec<Entry>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let cut = self.cut.map_or(i64::MAX, EntryId::get);
        let connection = &store.connection;
        let mut statement = connection.prepare(
            "SELECT id, kind, data, projection FROM entries \
             WHERE conversation_id = ?1 AND id <= ?2 ORDER BY id ASC",
        )?;
        let mut rows = statement.query(params![self.conversation.get(), cut])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            let id = id_from::<EntryId>(row.get(0)?)?;
            let kind: String = row.get(1)?;
            let data: String = row.get(2)?;
            let projection: String = row.get(3)?;
            entries.push(Entry {
                id,
                conversation_id: self.conversation,
                kind: EntryKind::new(kind).map_err(|error| {
                    StoreError::Rejected(Error::Corrupt(format!(
                        "entry {id} has an invalid kind: {error}"
                    )))
                })?,
                data: decode(&data)?,
                projection: decode(&projection)?,
            });
        }
        Ok(entries)
    }
}

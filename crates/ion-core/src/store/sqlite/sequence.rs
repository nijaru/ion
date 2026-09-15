//! Local identity and commit-cursor allocation.
//!
//! Every local number — entry, input, turn, step, attempt, invocation and the
//! commit cursor — comes from one monotonic session sequence, allocated inside
//! the transaction that publishes it. Two records can therefore never share a
//! durable number, a failed transaction consumes nothing, and no caller may
//! rely on two ids being adjacent.

use rusqlite::Connection;

use super::{StoreError, id_from};
use crate::error::Error;
use crate::id::{CommitSeq, LocalSeq};

/// Numbers reserved for one transaction.
#[derive(Debug)]
pub(crate) struct Reserved {
    ids: Vec<LocalSeq>,
    commit: Option<CommitSeq>,
}

impl Reserved {
    /// The next reserved local id, in reservation order.
    pub(crate) fn next<T: TryFrom<i64, Error = crate::IdError>>(
        &mut self,
    ) -> Result<T, StoreError> {
        if self.ids.is_empty() {
            return Err(StoreError::Failed(
                "the transaction reserved fewer identities than it used".to_owned(),
            ));
        }
        let id = self.ids.remove(0);
        id_from(id.get())
    }

    /// The commit cursor this transaction publishes under.
    pub(crate) fn commit(&self) -> Result<CommitSeq, StoreError> {
        self.commit
            .ok_or_else(|| StoreError::Failed("the transaction published no commit".to_owned()))
    }
}

/// Reserve `ids` local identities, optionally ending with a commit cursor.
pub(crate) fn reserve(
    connection: &Connection,
    ids: usize,
    commit: bool,
) -> Result<Reserved, StoreError> {
    let total = ids
        .checked_add(usize::from(commit))
        .ok_or_else(|| StoreError::Failed("identity reservation overflowed".to_owned()))?;
    let total = i64::try_from(total)
        .map_err(|_| StoreError::Failed("identity reservation overflowed".to_owned()))?;
    let (last_seq, last_commit): (Option<i64>, Option<i64>) = connection.query_row(
        "SELECT last_seq, last_commit FROM session_meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let start = last_seq.unwrap_or(0);
    let end = start
        .checked_add(total)
        .ok_or_else(|| StoreError::Failed("the session identity space is exhausted".to_owned()))?;

    let mut reserved = Vec::with_capacity(ids);
    for raw in start + 1..=start + total {
        reserved.push(LocalSeq::new(raw).map_err(|error| {
            StoreError::Rejected(Error::Corrupt(format!(
                "invalid reserved id {raw}: {error}"
            )))
        })?);
    }
    let mut commit_value = None;
    if commit {
        let raw = reserved.pop().ok_or_else(|| {
            StoreError::Failed("the transaction reserved no commit cursor".to_owned())
        })?;
        commit_value = Some(CommitSeq::from(raw));
    }
    connection.execute(
        "UPDATE session_meta SET last_seq = ?1, last_commit = COALESCE(?2, last_commit) WHERE id = 1",
        rusqlite::params![end, commit_value.map(|value| value.get())],
    )?;
    debug_assert!(
        last_commit.is_none_or(|previous| commit_value.is_none_or(|c| c.get() > previous))
    );
    Ok(Reserved {
        ids: reserved,
        commit: commit_value,
    })
}

/// Read the durable content budget already occupied.
pub(crate) fn used_bytes(connection: &Connection) -> Result<u64, StoreError> {
    let used: i64 = connection.query_row(
        "SELECT used_bytes FROM session_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(used)
        .map_err(|_| StoreError::Rejected(Error::Corrupt("negative content usage".to_owned())))
}

/// Add `bytes` to the durable content budget and return the new total.
pub(crate) fn charge(connection: &Connection, bytes: u64) -> Result<u64, StoreError> {
    let used = used_bytes(connection)?;
    let total = used
        .checked_add(bytes)
        .ok_or_else(|| StoreError::Failed("content accounting overflowed".to_owned()))?;
    connection.execute(
        "UPDATE session_meta SET used_bytes = ?1 WHERE id = 1",
        [super::codec::as_i64(total)?],
    )?;
    Ok(total)
}

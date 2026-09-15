//! Conversation rows: creation, installation and history validation.
//!
//! A branch may only point at a conversation that already exists and at an
//! entry that is visible inside it. Both rules are enforced here rather than in
//! a Rust type, because a durable store can be corrupted or written by an older
//! build; validation must be able to refuse it at open.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::codec::{decode_optional, encode, id_optional};
use super::sequence::{charge, reserve};
use super::{SqliteStore, StoreError, id_from};
use crate::config::{ConversationConfig, InstalledConfig};
use crate::conversation::{Conversation, HistoryParent};
use crate::error::Error;
use crate::store::{Command, SessionInfo};
use crate::{CommitSeq, ConversationId, EntryId, SessionId};

/// The bounded depth an ancestry walk may take before it is refused.
///
/// A cycle is refused explicitly; this bound also refuses a pathologically deep
/// but acyclic graph, so a corrupted store cannot make open cost unbounded.
const MAX_HISTORY_DEPTH: usize = 4_096;

/// Create the session metadata row and the root conversation.
pub(crate) struct CreateSession {
    pub(crate) session_id: SessionId,
    pub(crate) config: ConversationConfig,
}

impl Command for CreateSession {
    type Output = SessionInfo;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        self.config
            .validate()
            .map_err(|error| StoreError::Rejected(error.into()))?;
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let existing: Option<i64> = transaction
            .query_row("SELECT id FROM session_meta WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        if existing.is_some() {
            return Err(StoreError::Rejected(Error::Invalid(
                "this database already holds a session".to_owned(),
            )));
        }
        transaction.execute(
            "INSERT INTO session_meta (id, session_id, used_bytes) VALUES (1, ?1, 0)",
            [self.session_id.as_uuid().to_string()],
        )?;
        let mut reserved = reserve(&transaction, 1, true)?;
        let root: ConversationId = reserved.next()?;
        let revision = reserved.commit()?;
        transaction.execute(
            "INSERT INTO conversations (id, parent_id, parent_at, config, config_revision) \
             VALUES (?1, NULL, NULL, ?2, ?3)",
            params![root.get(), encode(&self.config)?, revision.get()],
        )?;
        transaction.execute(
            "UPDATE session_meta SET root_conversation = ?1 WHERE id = 1",
            [root.get()],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(SessionInfo {
            session_id: self.session_id,
            root,
            last_commit: Some(revision),
        })
    }
}

/// Read the durable session identity and root conversation.
pub(crate) struct ReadSessionInfo;

impl Command for ReadSessionInfo {
    type Output = SessionInfo;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        store.check_readable()
    }
}

/// Read the current commit cursor, used to address published observations.
pub(crate) struct ReadCommit;

impl Command for ReadCommit {
    type Output = Option<CommitSeq>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        Ok(store.check_readable()?.last_commit)
    }
}

/// Replace a conversation's configuration as one complete record.
pub(crate) struct ConfigureConversation {
    pub(crate) conversation: ConversationId,
    pub(crate) expected: Option<CommitSeq>,
    pub(crate) config: ConversationConfig,
}

impl Command for ConfigureConversation {
    type Output = CommitSeq;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        self.config
            .validate()
            .map_err(|error| StoreError::Rejected(error.into()))?;
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let current: Option<Option<i64>> = transaction
            .query_row(
                "SELECT config_revision FROM conversations WHERE id = ?1",
                [self.conversation.get()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "conversation {} does not exist",
                self.conversation
            ))));
        };
        let current = id_optional::<CommitSeq>(current)?;
        if let Some(expected) = self.expected
            && current != Some(expected)
        {
            return Err(StoreError::Rejected(Error::StaleConfig {
                expected,
                actual: current,
            }));
        }
        let reserved = reserve(&transaction, 0, true)?;
        let revision = reserved.commit()?;
        transaction.execute(
            "UPDATE conversations SET config = ?2, config_revision = ?3 WHERE id = ?1",
            params![
                self.conversation.get(),
                encode(&self.config)?,
                revision.get()
            ],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(revision)
    }
}

/// Read one conversation, refusing a record this build cannot decode.
pub(crate) struct ReadConversation {
    pub(crate) conversation: ConversationId,
}

impl Command for ReadConversation {
    type Output = Option<Conversation>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &store.connection;
        let row = connection
            .query_row(
                "SELECT parent_id, parent_at, config, config_revision \
                 FROM conversations WHERE id = ?1",
                [self.conversation.get()],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((parent_id, parent_at, config, revision)) = row else {
            return Ok(None);
        };
        let parent_id = id_optional::<ConversationId>(parent_id)?;
        let parent_at = id_optional::<EntryId>(parent_at)?;
        let parent = parent_id
            .map(|conversation_id| {
                let at = parent_at.ok_or_else(|| {
                    Error::Corrupt(format!(
                        "conversation {} has a history parent without a cutoff",
                        self.conversation
                    ))
                })?;
                Ok::<_, Error>(HistoryParent {
                    conversation_id,
                    at,
                })
            })
            .transpose()
            .map_err(StoreError::Rejected)?;
        let config = match decode_optional::<ConversationConfig>(config)? {
            Some(config) => {
                let raw = revision.ok_or_else(|| {
                    StoreError::Rejected(Error::Corrupt(format!(
                        "conversation {} stores a configuration without a revision",
                        self.conversation
                    )))
                })?;
                Some(InstalledConfig::new(id_from::<CommitSeq>(raw)?, config))
            }
            None => None,
        };
        Ok(Some(Conversation {
            id: self.conversation,
            parent,
            config,
        }))
    }
}

/// Validate every history edge in the store.
///
/// Open runs this before the session can be used: a store with a cycle, a
/// missing parent or a cutoff that is not visible in its source is refused
/// rather than traversed.
pub(crate) struct ValidateAncestry;

impl Command for ValidateAncestry {
    type Output = ();

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        validate_ancestry(&store.connection)
    }
}

/// The remaining content budget after a charge, or a quota refusal.
pub(crate) fn charge_or_refuse(
    connection: &Connection,
    limits: crate::limits::SessionLimits,
    size: u64,
    settlement: bool,
) -> Result<u64, StoreError> {
    let used = super::sequence::used_bytes(connection)?;
    let admits = if settlement {
        limits.admits_settlement(used, size)
    } else {
        limits.admits(used, size)
    };
    if !admits {
        return Err(StoreError::Rejected(Error::QuotaExhausted {
            used,
            quota: if settlement {
                limits.quota_bytes
            } else {
                limits.admission_ceiling()
            },
        }));
    }
    charge(connection, size)
}

/// Validate one stored history edge.
///
/// Every failure here is corruption, not a client mistake: no command writes
/// these rows, so an edge that cannot be satisfied means the store itself is
/// not usable and must be refused rather than traversed.
fn validate_parent(connection: &Connection, parent: HistoryParent) -> Result<(), StoreError> {
    let exists: Option<i64> = connection
        .query_row(
            "SELECT id FROM conversations WHERE id = ?1",
            [parent.conversation_id.get()],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(StoreError::Rejected(Error::Corrupt(format!(
            "conversation {} has missing history parent {}",
            parent.at, parent.conversation_id
        ))));
    }
    let owner: Option<Option<i64>> = connection
        .query_row(
            "SELECT conversation_id FROM entries WHERE id = ?1",
            [parent.at.get()],
            |row| row.get(0),
        )
        .optional()?;
    match owner {
        Some(Some(owner)) if owner == parent.conversation_id.get() => Ok(()),
        Some(Some(_)) => Err(StoreError::Rejected(Error::Corrupt(format!(
            "cutoff {} is not visible in conversation {}",
            parent.at, parent.conversation_id
        )))),
        _ => Err(StoreError::Rejected(Error::Corrupt(format!(
            "cutoff {} does not exist",
            parent.at
        )))),
    }
}

fn validate_ancestry(connection: &Connection) -> Result<(), StoreError> {
    let mut statement = connection.prepare("SELECT id, parent_id, parent_at FROM conversations")?;
    let mut rows = statement.query([])?;
    let mut edges: BTreeMap<i64, Option<(i64, Option<i64>)>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let parent: Option<i64> = row.get(1)?;
        let at: Option<i64> = row.get(2)?;
        edges.insert(id, parent.map(|parent| (parent, at)));
    }
    for id in edges.keys() {
        let mut seen = BTreeSet::new();
        let mut current = Some(*id);
        let mut depth = 0usize;
        while let Some(node) = current {
            if !seen.insert(node) {
                return Err(StoreError::Rejected(Error::Corrupt(format!(
                    "conversation {node} is its own ancestor"
                ))));
            }
            depth += 1;
            if depth > MAX_HISTORY_DEPTH {
                return Err(StoreError::Rejected(Error::Corrupt(
                    "conversation history is nested more deeply than this build supports"
                        .to_owned(),
                )));
            }
            current = match edges.get(&node) {
                Some(Some((parent, at))) => {
                    let Some(at) = at else {
                        return Err(StoreError::Rejected(Error::Corrupt(format!(
                            "conversation {node} has a history parent without a cutoff"
                        ))));
                    };
                    validate_parent(
                        connection,
                        HistoryParent {
                            conversation_id: ConversationId::try_from(*parent).map_err(
                                |error| {
                                    StoreError::Rejected(Error::Corrupt(format!(
                                        "invalid history parent {parent}: {error}"
                                    )))
                                },
                            )?,
                            at: EntryId::try_from(*at).map_err(|error| {
                                StoreError::Rejected(Error::Corrupt(format!(
                                    "invalid cutoff {at}: {error}"
                                )))
                            })?,
                        },
                    )?;
                    Some(*parent)
                }
                Some(None) => None,
                None => {
                    return Err(StoreError::Rejected(Error::Corrupt(format!(
                        "conversation {node} does not exist"
                    ))));
                }
            };
        }
    }
    Ok(())
}

/// Read and decode an installed configuration, if any.
pub(crate) fn read_config(
    connection: &Connection,
    conversation: ConversationId,
) -> Result<Option<InstalledConfig>, StoreError> {
    let row: Option<(Option<String>, Option<i64>)> = connection
        .query_row(
            "SELECT config, config_revision FROM conversations WHERE id = ?1",
            [conversation.get()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((config, revision)) = row else {
        return Ok(None);
    };
    match decode_optional::<ConversationConfig>(config)? {
        Some(config) => {
            let raw = revision.ok_or_else(|| {
                StoreError::Rejected(Error::Corrupt(
                    "stored configuration has no revision".to_owned(),
                ))
            })?;
            Ok(Some(InstalledConfig::new(
                id_from::<CommitSeq>(raw)?,
                config,
            )))
        }
        None => Ok(None),
    }
}

/// One entry about to be appended.
pub(crate) struct NewEntry<'a> {
    pub(crate) id: crate::EntryId,
    pub(crate) conversation: ConversationId,
    pub(crate) kind: &'a crate::EntryKind,
    pub(crate) data: &'a serde_json::Value,
    pub(crate) projection: &'a [ion_ai::Message],
}

/// Append one transcript entry and charge its content budget.
///
/// `settlement` selects the reserved allowance: an outcome that has already
/// happened must be recordable even when ordinary admission growth is refused.
pub(crate) fn insert_entry(
    connection: &Connection,
    entry: NewEntry<'_>,
    limits: crate::limits::SessionLimits,
    settlement: bool,
) -> Result<(), StoreError> {
    let encoded = encode(entry.data)?;
    let projected = encode(&entry.projection.to_vec())?;
    let size = (encoded.len() + projected.len()) as u64;
    charge_or_refuse(connection, limits, size, settlement)?;
    connection.execute(
        "INSERT INTO entries (id, conversation_id, kind, data, projection) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            entry.id.get(),
            entry.conversation.get(),
            entry.kind.as_str(),
            encoded,
            projected
        ],
    )?;
    Ok(())
}

//! Input rows: admission, replay, queueing and placement.
//!
//! Admission is one transaction. It decides whether the input starts a turn,
//! waits for one, or is an exact replay of one already accepted, and it commits
//! that decision together with the input and the entry that carries it. A
//! client therefore never receives a receipt for work that was not durably
//! accepted, and never loses accepted input to a scheduling failure.

use rusqlite::{OptionalExtension, params};

use super::codec::{decode, encode};
use super::conversation::charge_or_refuse;
use super::sequence::reserve;
use super::{SqliteStore, StoreError, id_from};
use crate::error::Error;
use crate::input::{
    Input, InputBody, InputDisposition, InputMode, InputPlacement, InputSender, RequestKey,
};
use crate::limits::SessionLimits;
use crate::store::Command;
use crate::{CommitSeq, ConversationId, InputId, TurnId};

/// The outcome of one admission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// A turn was created and the input was placed in the transcript.
    Started {
        input: InputId,
        turn: TurnId,
        commit: CommitSeq,
    },
    /// The conversation is busy; the input waits.
    Queued { input: InputId, commit: CommitSeq },
    /// The request key was already accepted with identical content.
    Replay {
        input: InputId,
        turn: Option<TurnId>,
    },
}

/// Admit one input.
pub(crate) struct AdmitInput {
    pub(crate) conversation: ConversationId,
    pub(crate) sender: InputSender,
    pub(crate) mode: InputMode,
    pub(crate) request_key: Option<RequestKey>,
    pub(crate) body: InputBody,
    pub(crate) limits: SessionLimits,
    pub(crate) now_unix_ms: i64,
}

impl Command for AdmitInput {
    type Output = Admitted;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;

        let config = super::conversation::read_config(&transaction, self.conversation)?;
        let Some(config) = config else {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "conversation {} does not exist or has no installed configuration",
                self.conversation
            ))));
        };

        if let Some(key) = &self.request_key {
            let existing: Option<(i64, i64, String, String, String, Option<i64>)> = transaction
                .query_row(
                    "SELECT id, conversation_id, sender, mode, body, placed_turn \
                     FROM inputs WHERE request_key = ?1",
                    [key.as_str()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((id, conversation, sender, mode, body, turn)) = existing {
                let same = conversation == self.conversation.get()
                    && sender == encode(&self.sender)?
                    && mode == encode(&self.mode)?
                    && body == encode(&self.body)?;
                if !same {
                    return Err(StoreError::Rejected(Error::RequestKeyConflict {
                        key: key.as_str().to_owned(),
                    }));
                }
                let input = id_from::<InputId>(id)?;
                let turn = super::codec::id_optional::<TurnId>(turn)?;
                return Ok(Admitted::Replay { input, turn });
            }
        }

        let unfinished: Option<i64> = transaction
            .query_row(
                "SELECT id FROM turns WHERE conversation_id = ?1 AND outcome IS NULL",
                [self.conversation.get()],
                |row| row.get(0),
            )
            .optional()?;

        if unfinished.is_some() {
            if self.mode == InputMode::Submit {
                return Err(StoreError::Rejected(Error::Busy(self.conversation)));
            }
            let queued: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM inputs WHERE disposition = 'queued'",
                [],
                |row| row.get(0),
            )?;
            if u64::try_from(queued).unwrap_or(u64::MAX) >= u64::from(self.limits.max_queued_inputs)
            {
                return Err(StoreError::Rejected(Error::QueueFull {
                    limit: self.limits.max_queued_inputs,
                }));
            }
            let mut reserved = reserve(&transaction, 1, true)?;
            let input: InputId = reserved.next()?;
            let commit = reserved.commit()?;
            charge_or_refuse(&transaction, self.limits, self.body.size(), false)?;
            transaction.execute(
                "INSERT INTO inputs \
                 (id, conversation_id, request_key, sender, mode, body, disposition) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued')",
                params![
                    input.get(),
                    self.conversation.get(),
                    self.request_key.as_ref().map(|key| key.as_str()),
                    encode(&self.sender)?,
                    encode(&self.mode)?,
                    encode(&self.body)?,
                ],
            )?;
            #[cfg(test)]
            super::check_fault(&store.fault)?;
            transaction.commit()?;
            return Ok(Admitted::Queued { input, commit });
        }

        // No unfinished turn: this input starts one.
        let mut reserved = reserve(&transaction, 3, true)?;
        let input: InputId = reserved.next()?;
        let turn: TurnId = reserved.next()?;
        let entry: crate::EntryId = reserved.next()?;
        let commit = reserved.commit()?;

        transaction.execute(
            "INSERT INTO inputs \
             (id, conversation_id, request_key, sender, mode, body, disposition, placed_entry, placed_turn) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'placed', ?7, ?8)",
            params![
                input.get(),
                self.conversation.get(),
                self.request_key.as_ref().map(|key| key.as_str()),
                encode(&self.sender)?,
                encode(&self.mode)?,
                encode(&self.body)?,
                entry.get(),
                turn.get(),
            ],
        )?;
        let placement = self.body.placement();
        super::conversation::insert_entry(
            &transaction,
            super::conversation::NewEntry {
                id: entry,
                conversation: self.conversation,
                kind: &placement.kind,
                data: &placement.data,
                projection: &placement.projection,
            },
            self.limits,
            false,
        )?;
        transaction.execute(
            "INSERT INTO turns \
             (id, conversation_id, phase, generation, cancel_requested, limits, admitted_at, steps_used) \
             VALUES (?1, ?2, 'ready', 0, 0, ?3, ?4, 0)",
            params![
                turn.get(),
                self.conversation.get(),
                encode(&config.config.limits)?,
                self.now_unix_ms,
            ],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(Admitted::Started {
            input,
            turn,
            commit,
        })
    }
}

/// The columns one input row is read from.
type InputRow = (
    i64,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<i64>,
);

/// Read one input.
pub(crate) struct ReadInput {
    pub(crate) input: InputId,
}

impl Command for ReadInput {
    type Output = Option<Input>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &store.connection;
        let row: Option<InputRow> = connection
                .query_row(
                    "SELECT conversation_id, request_key, sender, mode, body, disposition, placed_entry, placed_turn \
                     FROM inputs WHERE id = ?1",
                    [self.input.get()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ))
                    },
                )
                .optional()?;
        let Some((conversation, key, sender, mode, body, disposition, entry, turn)) = row else {
            return Ok(None);
        };
        let key = key
            .map(|key| {
                RequestKey::new(key).map_err(|error| {
                    StoreError::Rejected(Error::Corrupt(format!("invalid request key: {error}")))
                })
            })
            .transpose()?;
        let placement = match super::codec::id_optional::<crate::EntryId>(entry)? {
            Some(entry) => Some(InputPlacement {
                entry,
                turn: super::codec::id_optional::<TurnId>(turn)?.ok_or_else(|| {
                    StoreError::Rejected(Error::Corrupt(format!(
                        "input {} is placed without a turn",
                        self.input
                    )))
                })?,
            }),
            None => None,
        };
        let disposition = disposition_from(&disposition, placement, self.input)?;
        Ok(Some(Input {
            id: self.input,
            target: id_from::<ConversationId>(conversation)?,
            sender: decode(&sender)?,
            mode: decode(&mode)?,
            request_key: key,
            body: decode(&body)?,
            disposition,
        }))
    }
}

/// Read the queued inputs of one conversation in admission order.
pub(crate) struct ReadQueuedInputs {
    pub(crate) conversation: ConversationId,
    pub(crate) mode: Option<InputMode>,
}

impl Command for ReadQueuedInputs {
    type Output = Vec<InputId>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &store.connection;
        let mut statement = connection.prepare(
            "SELECT id, mode FROM inputs \
             WHERE conversation_id = ?1 AND disposition = 'queued' ORDER BY id ASC",
        )?;
        let mut rows = statement.query([self.conversation.get()])?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id = id_from::<InputId>(row.get(0)?)?;
            let mode: String = row.get(1)?;
            let mode: InputMode = decode(&mode)?;
            if self.mode.is_none_or(|wanted| wanted == mode) {
                ids.push(id);
            }
        }
        Ok(ids)
    }
}

/// Withdraw an unplaced input. A placed input is abandoned instead: its entry
/// stays in the transcript.
pub(crate) struct WithdrawInput {
    pub(crate) input: InputId,
}

impl Command for WithdrawInput {
    type Output = ();

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let disposition: Option<String> = transaction
            .query_row(
                "SELECT disposition FROM inputs WHERE id = ?1",
                [self.input.get()],
                |row| row.get(0),
            )
            .optional()?;
        match disposition.as_deref() {
            Some("queued") => {}
            Some("placed") => {
                return Err(StoreError::Rejected(Error::Invalid(format!(
                    "input {} is already in the transcript; abandon it instead of withdrawing",
                    self.input
                ))));
            }
            Some(other) => {
                return Err(StoreError::Rejected(Error::Invalid(format!(
                    "input {} cannot be withdrawn from disposition {other}",
                    self.input
                ))));
            }
            None => {
                return Err(StoreError::Rejected(Error::Invalid(format!(
                    "input {} does not exist",
                    self.input
                ))));
            }
        }
        transaction.execute(
            "UPDATE inputs SET disposition = 'cancelled' WHERE id = ?1",
            [self.input.get()],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(())
    }
}

/// Decode the stored disposition literal together with its placement.
fn disposition_from(
    raw: &str,
    placement: Option<InputPlacement>,
    input: InputId,
) -> Result<InputDisposition, StoreError> {
    match raw {
        "queued" => Ok(InputDisposition::Queued),
        "cancelled" => Ok(InputDisposition::Cancelled),
        "placed" => Ok(InputDisposition::Placed(placement.ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(format!(
                "input {input} is placed without a placement"
            )))
        })?)),
        "abandoned" => Ok(InputDisposition::Abandoned(placement.ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(format!(
                "input {input} is abandoned without a placement"
            )))
        })?)),
        other => Err(StoreError::Rejected(Error::Corrupt(format!(
            "input {input} has unknown disposition {other:?}"
        )))),
    }
}

/// Read one input's body inside a transaction.
pub(crate) fn read_body(
    connection: &rusqlite::Connection,
    input: InputId,
) -> Result<InputBody, StoreError> {
    let body: String = connection.query_row(
        "SELECT body FROM inputs WHERE id = ?1",
        [input.get()],
        |row| row.get(0),
    )?;
    decode(&body)
}

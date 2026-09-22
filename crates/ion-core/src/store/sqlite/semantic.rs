//! Typed semantic transactions and bounded reads for schema v2.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::super::{StoreError, StoreMetadata};
use crate::id::LocalSeq;
use crate::session::{
    AbandonResult, Admission, AdmitInputRequest, CancellationResult, ConfiguredConversation,
    CreatedConversation, StartTurnRequest, StartedTurn,
};
use crate::{
    Cancellation, CommitReceipt, CommitSeq, Conversation, ConversationConfig, ConversationId,
    Entry, EntryData, EntryId, EntryPage, HistoryParent, Input, InputBody, InputDisposition,
    InputId, InstalledConfig, RequestKey, SessionChange, SessionId, SessionSnapshot, SessionUpdate,
    SnapshotRequest, TranscriptMessage, Turn, TurnBudget, TurnEnvironment, TurnId, TurnOutcome,
    TurnPhase,
};

pub(super) struct Sequence {
    base: i64,
    last: i64,
}

impl Sequence {
    pub(super) fn load(connection: &Connection) -> Result<Self, StoreError> {
        let last: i64 = connection.query_row(
            "SELECT last_seq FROM session_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if last < 0 {
            return Err(StoreError::Corrupt(format!(
                "session sequence is negative: {last}"
            )));
        }
        Ok(Self { base: last, last })
    }

    pub(super) fn next<T>(&mut self) -> Result<T, StoreError>
    where
        T: From<LocalSeq>,
    {
        self.last = self
            .last
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidState("session sequence exhausted".to_owned()))?;
        let local = LocalSeq::new(self.last)
            .map_err(|error| StoreError::InvalidState(error.to_string()))?;
        Ok(T::from(local))
    }
}

struct MetadataRow {
    public: StoreMetadata,
    last_commit: CommitSeq,
}

pub(super) fn metadata(connection: &Connection) -> Result<StoreMetadata, StoreError> {
    Ok(metadata_row(connection)?.public)
}

fn metadata_row(connection: &Connection) -> Result<MetadataRow, StoreError> {
    let row = connection
        .query_row(
            "SELECT session_id, last_seq, last_commit, primary_conversation
             FROM session_meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::Corrupt("session metadata is missing".to_owned()))?;
    let session_id = row.0.parse::<SessionId>().map_err(|error| {
        StoreError::Corrupt(format!(
            "session metadata has an invalid session id: {error}"
        ))
    })?;
    if row.1 <= 0 {
        return Err(StoreError::Corrupt(
            "initialized session has no positive local sequence".to_owned(),
        ));
    }
    let last_commit = id::<CommitSeq>(
        row.2
            .ok_or_else(|| StoreError::Corrupt("session has no commit cursor".to_owned()))?,
        "last commit",
    )?;
    if last_commit.get() > row.1 {
        return Err(StoreError::Corrupt(format!(
            "last commit {} exceeds local sequence {}",
            last_commit.get(),
            row.1
        )));
    }
    let primary_conversation = id::<ConversationId>(
        row.3
            .ok_or_else(|| StoreError::Corrupt("session has no primary conversation".to_owned()))?,
        "primary conversation",
    )?;
    load_conversation(connection, primary_conversation)?;
    Ok(MetadataRow {
        public: StoreMetadata {
            session_id,
            primary_conversation,
        },
        last_commit,
    })
}

pub(super) fn create_primary(
    connection: &mut Connection,
    session_id: SessionId,
    config: ConversationConfig,
) -> Result<(StoreMetadata, CommitReceipt), StoreError> {
    validate_new_config(&config)?;
    let transaction = connection.transaction()?;
    let mut sequence = Sequence::load(&transaction)?;
    if sequence.base != 0 {
        return Err(StoreError::Corrupt(
            "new session already consumed local sequence values".to_owned(),
        ));
    }

    let conversation_id: ConversationId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let conversation = Conversation {
        id: conversation_id,
        history_parent: None,
        current_config_revision: commit,
        retired: false,
    };
    let installed = InstalledConfig {
        revision: commit,
        config,
    };

    insert_conversation(&transaction, &conversation)?;
    insert_config(&transaction, conversation_id, &installed)?;
    advance_metadata(&transaction, &sequence, commit, Some(conversation_id))?;
    transaction.commit()?;

    let metadata = StoreMetadata {
        session_id,
        primary_conversation: conversation_id,
    };
    let receipt = CommitReceipt {
        seq: commit,
        update: SessionUpdate::new(vec![
            SessionChange::Conversation(conversation),
            SessionChange::Config {
                conversation: conversation_id,
                installed,
            },
        ]),
    };
    Ok((metadata, receipt))
}

pub(super) fn create_conversation(
    connection: &mut Connection,
    config: ConversationConfig,
) -> Result<CreatedConversation, StoreError> {
    validate_new_config(&config)?;
    let transaction = connection.transaction()?;
    let mut sequence = Sequence::load(&transaction)?;
    let conversation_id: ConversationId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let conversation = Conversation {
        id: conversation_id,
        history_parent: None,
        current_config_revision: commit,
        retired: false,
    };
    let installed = InstalledConfig {
        revision: commit,
        config,
    };
    insert_conversation(&transaction, &conversation)?;
    insert_config(&transaction, conversation_id, &installed)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(CreatedConversation {
        conversation: conversation.clone(),
        config: installed.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::Conversation(conversation),
                SessionChange::Config {
                    conversation: conversation_id,
                    installed,
                },
            ]),
        },
    })
}

pub(super) fn current_config(
    connection: &Connection,
    conversation: ConversationId,
) -> Result<InstalledConfig, StoreError> {
    let conversation = load_conversation(connection, conversation)?;
    load_config_revision(
        connection,
        conversation.id,
        conversation.current_config_revision,
    )
}

pub(super) fn config_as_of(
    connection: &Connection,
    conversation: ConversationId,
    revision: CommitSeq,
) -> Result<InstalledConfig, StoreError> {
    load_conversation(connection, conversation)?;
    let row = connection
        .query_row(
            "SELECT revision, config
             FROM conversation_configs
             WHERE conversation_id = ?1 AND revision <= ?2
             ORDER BY revision DESC
             LIMIT 1",
            params![conversation.get(), revision.get()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "configuration revision",
            id: revision.get(),
        })?;
    decode_installed_config(conversation, row.0, &row.1)
}

pub(super) fn configure(
    connection: &mut Connection,
    conversation_id: ConversationId,
    expected_revision: CommitSeq,
    config: ConversationConfig,
) -> Result<ConfiguredConversation, StoreError> {
    validate_new_config(&config)?;
    let transaction = connection.transaction()?;
    let mut conversation = load_conversation(&transaction, conversation_id)?;
    if conversation.retired {
        return Err(StoreError::InvalidState(format!(
            "conversation {conversation_id} is retired"
        )));
    }
    if conversation.current_config_revision != expected_revision {
        return Err(StoreError::RevisionConflict {
            conversation: conversation_id,
            expected: expected_revision,
            actual: conversation.current_config_revision,
        });
    }

    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;
    let installed = InstalledConfig {
        revision: commit,
        config,
    };
    insert_config(&transaction, conversation_id, &installed)?;
    let updated = transaction.execute(
        "UPDATE conversations SET current_config_revision = ?2
         WHERE id = ?1 AND current_config_revision = ?3",
        params![conversation_id.get(), commit.get(), expected_revision.get()],
    )?;
    if updated != 1 {
        return Err(StoreError::Corrupt(format!(
            "conversation {conversation_id} config pointer changed inside one owner transaction"
        )));
    }
    conversation.current_config_revision = commit;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(ConfiguredConversation {
        conversation: conversation.clone(),
        config: installed.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::Config {
                    conversation: conversation_id,
                    installed,
                },
                SessionChange::Conversation(conversation),
            ]),
        },
    })
}

pub(super) fn admit_input(
    connection: &mut Connection,
    conversation_id: ConversationId,
    request: AdmitInputRequest,
) -> Result<Admission, StoreError> {
    let transaction = connection.transaction()?;
    let conversation = load_conversation(&transaction, conversation_id)?;
    if conversation.retired {
        return Err(StoreError::InvalidState(format!(
            "conversation {conversation_id} is retired"
        )));
    }

    if let Some(request_key) = &request.request_key {
        let existing_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM inputs
                 WHERE conversation_id = ?1 AND request_key = ?2",
                params![conversation_id.get(), request_key.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_id) = existing_id {
            let (existing, admitted_at) =
                load_input(&transaction, id::<InputId>(existing_id, "input")?)?;
            if existing.sender == request.sender
                && existing.mode == request.mode
                && existing.body == request.body
            {
                return Ok(Admission::Replayed {
                    input: existing,
                    admitted_at,
                });
            }
            return Err(StoreError::RequestKeyConflict {
                conversation: conversation_id,
                request_key: request_key.as_str().to_owned(),
            });
        }
    }

    let mut sequence = Sequence::load(&transaction)?;
    let input_id: InputId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let input = Input {
        id: input_id,
        conversation: conversation_id,
        sender: request.sender,
        mode: request.mode,
        request_key: request.request_key,
        body: request.body,
        disposition: InputDisposition::Queued,
    };
    insert_input(&transaction, &input, commit)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    let receipt = CommitReceipt {
        seq: commit,
        update: SessionUpdate::new(vec![SessionChange::Input(input.clone())]),
    };
    Ok(Admission::Created {
        input,
        admitted_at: commit,
        receipt,
    })
}

pub(super) fn start_turn(
    connection: &mut Connection,
    request: StartTurnRequest,
) -> Result<StartedTurn, StoreError> {
    let transaction = connection.transaction()?;
    let conversation = load_conversation(&transaction, request.conversation)?;
    if conversation.retired {
        return Err(StoreError::InvalidState(format!(
            "conversation {} is retired",
            request.conversation
        )));
    }

    let active: Option<i64> = transaction
        .query_row(
            "SELECT id FROM turns
             WHERE conversation_id = ?1 AND outcome IS NULL
             LIMIT 1",
            [request.conversation.get()],
            |row| row.get(0),
        )
        .optional()?;
    if active.is_some() {
        return Err(StoreError::ConversationBusy(request.conversation));
    }

    let (mut input, _) = load_input(&transaction, request.input)?;
    if input.conversation != request.conversation {
        return Err(StoreError::InvalidState(format!(
            "input {} belongs to conversation {}, not {}",
            input.id, input.conversation, request.conversation
        )));
    }
    if !matches!(input.disposition, InputDisposition::Queued) {
        return Err(StoreError::InvalidState(format!(
            "input {} is not queued",
            input.id
        )));
    }
    let text = match &input.body {
        InputBody::Text(text) => text.clone(),
        InputBody::InteractionReply { .. } => {
            return Err(StoreError::InvalidState(
                "an interaction reply cannot start an ordinary turn".to_owned(),
            ));
        }
    };

    let installed = load_config_revision(
        &transaction,
        conversation.id,
        conversation.current_config_revision,
    )?;
    let (environment, settings) = TurnEnvironment::capture(&installed).map_err(|error| {
        StoreError::Corrupt(format!(
            "persisted conversation config cannot capture a turn: {error}"
        ))
    })?;

    let mut sequence = Sequence::load(&transaction)?;
    let entry_id: EntryId = sequence.next()?;
    let turn_id: TurnId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;

    let entry = Entry {
        id: entry_id,
        conversation: request.conversation,
        data: EntryData::UserInput { input: input.id },
        projection: vec![TranscriptMessage::user_text(text)],
    };
    let turn = Turn {
        id: turn_id,
        conversation: request.conversation,
        environment,
        settings,
        phase: TurnPhase::Ready,
        cancellation: Cancellation::default(),
        budget: TurnBudget::default(),
        admitted_at_unix_ms: request.admitted_at_unix_ms,
        wall_deadline_unix_ms: request.wall_deadline_unix_ms,
        outcome: None,
    };
    input.disposition = InputDisposition::Consumed {
        turn: turn_id,
        entry: Some(entry_id),
    };

    insert_turn(&transaction, &turn)?;
    insert_entry(&transaction, &entry, commit)?;
    update_input_disposition(&transaction, &input)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(StartedTurn {
        turn: turn.clone(),
        input: input.clone(),
        entry: entry.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::Entry(entry),
                SessionChange::Input(input),
                SessionChange::Turn(turn),
            ]),
        },
    })
}

pub(super) fn cancel_turn(
    connection: &mut Connection,
    turn_id: TurnId,
) -> Result<CancellationResult, StoreError> {
    let transaction = connection.transaction()?;
    let mut turn = load_turn(&transaction, turn_id)?;
    if turn.is_terminal() {
        return Ok(CancellationResult::Terminal(turn));
    }
    if turn.cancellation.requested {
        return Ok(CancellationResult::AlreadyRequested(turn));
    }

    let generation =
        turn.cancellation.generation.checked_add(1).ok_or_else(|| {
            StoreError::InvalidState("cancellation generation exhausted".to_owned())
        })?;
    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;

    let updated = transaction.execute(
        "UPDATE turns
         SET cancellation_generation = ?2, cancel_requested = 1
         WHERE id = ?1 AND outcome IS NULL
           AND cancel_requested = 0 AND cancellation_generation = ?3",
        params![
            turn_id.get(),
            i64_from_u64(generation, "cancellation generation")?,
            i64_from_u64(
                turn.cancellation.generation,
                "previous cancellation generation"
            )?
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} changed before cancellation could commit"
        )));
    }
    turn.cancellation = Cancellation {
        requested: true,
        generation,
    };
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(CancellationResult::Committed {
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![SessionChange::Turn(turn)]),
        },
    })
}

pub(super) fn abandon_turn(
    connection: &mut Connection,
    turn_id: TurnId,
) -> Result<AbandonResult, StoreError> {
    let transaction = connection.transaction()?;
    let mut turn = load_turn(&transaction, turn_id)?;
    if turn.is_terminal() {
        return Ok(AbandonResult::Terminal(turn));
    }

    let model_attempts: i64 = transaction.query_row(
        "SELECT COUNT(*)
         FROM model_attempts ma
         JOIN model_steps ms ON ms.id = ma.step_id
         WHERE ms.turn_id = ?1",
        [turn_id.get()],
        |row| row.get(0),
    )?;
    let tool_attempts: i64 = transaction.query_row(
        "SELECT COUNT(*)
         FROM tool_attempts ta
         JOIN tool_invocations ti ON ti.id = ta.invocation_id
         JOIN model_steps ms ON ms.id = ti.step_id
         WHERE ms.turn_id = ?1",
        [turn_id.get()],
        |row| row.get(0),
    )?;
    if model_attempts != 0 || tool_attempts != 0 {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} has physical attempts and cannot be passively abandoned"
        )));
    }

    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;
    let outcome = TurnOutcome::Abandoned {
        unresolved_attempts: Vec::new(),
    };
    let updated = transaction.execute(
        "UPDATE turns SET outcome = ?2 WHERE id = ?1 AND outcome IS NULL",
        params![turn_id.get(), json_to(&outcome)?],
    )?;
    if updated != 1 {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} changed before abandonment could commit"
        )));
    }
    turn.outcome = Some(outcome);
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(AbandonResult::Committed {
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![SessionChange::Turn(turn)]),
        },
    })
}

pub(super) fn snapshot(
    connection: &mut Connection,
    request: SnapshotRequest,
) -> Result<SessionSnapshot, StoreError> {
    request
        .validate()
        .map_err(|error| StoreError::InvalidRequest(error.to_string()))?;

    let transaction = connection.transaction()?;
    let metadata = metadata_row(&transaction)?;
    let conversation = load_conversation(&transaction, request.conversation)?;
    let config = load_config_revision(
        &transaction,
        conversation.id,
        conversation.current_config_revision,
    )?;

    let unfinished_turn_id: Option<i64> = transaction
        .query_row(
            "SELECT id FROM turns
             WHERE conversation_id = ?1 AND outcome IS NULL
             LIMIT 1",
            [request.conversation.get()],
            |row| row.get(0),
        )
        .optional()?;
    let unfinished_turn = unfinished_turn_id
        .map(|raw| load_turn(&transaction, id::<TurnId>(raw, "unfinished turn")?))
        .transpose()?;

    let current_model_step = match unfinished_turn.as_ref().map(|turn| &turn.phase) {
        Some(TurnPhase::Model(step) | TurnPhase::Tools(step)) => {
            Some(super::model_state::load_step(&transaction, *step)?)
        }
        Some(
            TurnPhase::Ready
            | TurnPhase::WaitingInteraction(_)
            | TurnPhase::BlockedEffect { .. }
            | TurnPhase::Parked(_),
        )
        | None => None,
    };
    let model_attempts = current_model_step
        .as_ref()
        .map(|step| {
            let maximum = unfinished_turn
                .as_ref()
                .expect("current model step implies an unfinished turn")
                .environment
                .limits
                .max_model_attempts_per_step;
            super::model_state::load_attempts(&transaction, step.id, maximum)
        })
        .transpose()?
        .unwrap_or_default();

    let (tool_invocations, tool_attempts) = match unfinished_turn.as_ref().map(|turn| &turn.phase) {
        Some(TurnPhase::Tools(step)) => {
            let records = super::tool_state::records(&transaction, *step)?;
            (records.invocations, records.attempts)
        }
        _ => (Vec::new(), Vec::new()),
    };

    let input_limit = sql_limit_plus_one(request.max_inputs)?;
    let mut input_statement = transaction.prepare(
        "SELECT id FROM inputs
         WHERE conversation_id = ?1 AND disposition_kind = 'queued'
         ORDER BY id
         LIMIT ?2",
    )?;
    let input_rows = input_statement
        .query_map(params![request.conversation.get(), input_limit], |row| {
            row.get::<_, i64>(0)
        })?;
    let mut queued_inputs = Vec::new();
    for row in input_rows {
        let input_id = id::<InputId>(row?, "queued input")?;
        queued_inputs.push(load_input(&transaction, input_id)?.0);
    }
    drop(input_statement);
    let mut has_more_inputs = queued_inputs.len() > request.max_inputs;
    queued_inputs.truncate(request.max_inputs);

    let entry_limit = sql_limit_plus_one(request.max_entries)?;
    let mut entry_statement = transaction.prepare(
        "SELECT id FROM entries
         WHERE conversation_id = ?1
         ORDER BY id DESC
         LIMIT ?2",
    )?;
    let entry_rows = entry_statement
        .query_map(params![request.conversation.get(), entry_limit], |row| {
            row.get::<_, i64>(0)
        })?;
    let mut transcript_tail = Vec::new();
    for row in entry_rows {
        transcript_tail.push(load_entry(&transaction, id::<EntryId>(row?, "entry")?)?);
    }
    drop(entry_statement);
    let mut has_older_entries = transcript_tail.len() > request.max_entries;
    transcript_tail.truncate(request.max_entries);
    transcript_tail.reverse();

    let mut snapshot = SessionSnapshot {
        coverage: metadata.last_commit,
        session: metadata.public.session_id,
        primary_conversation: metadata.public.primary_conversation,
        conversation,
        config,
        unfinished_turn,
        current_model_step,
        model_attempts,
        tool_invocations,
        tool_attempts,
        queued_inputs,
        has_more_inputs,
        transcript_tail,
        has_older_entries,
    };

    while snapshot.encoded_len() > request.max_bytes && !snapshot.transcript_tail.is_empty() {
        snapshot.transcript_tail.remove(0);
        has_older_entries = true;
        snapshot.has_older_entries = has_older_entries;
    }
    while snapshot.encoded_len() > request.max_bytes && !snapshot.queued_inputs.is_empty() {
        snapshot.queued_inputs.pop();
        has_more_inputs = true;
        snapshot.has_more_inputs = has_more_inputs;
    }
    if snapshot.encoded_len() > request.max_bytes {
        return Err(StoreError::SnapshotTooLarge {
            maximum: request.max_bytes,
        });
    }

    transaction.commit()?;
    Ok(snapshot)
}

pub(super) fn page_entries(
    connection: &Connection,
    conversation: ConversationId,
    before: Option<EntryId>,
    limit: usize,
) -> Result<EntryPage, StoreError> {
    if limit == 0 || limit > crate::MAX_SNAPSHOT_ENTRIES {
        return Err(StoreError::InvalidRequest(format!(
            "entry page limit must be in 1..={}",
            crate::MAX_SNAPSHOT_ENTRIES
        )));
    }
    load_conversation(connection, conversation)?;
    let sql_limit = sql_limit_plus_one(limit)?;
    let sql = if before.is_some() {
        "SELECT id FROM entries
         WHERE conversation_id = ?1 AND id < ?2
         ORDER BY id DESC
         LIMIT ?3"
    } else {
        "SELECT id FROM entries
         WHERE conversation_id = ?1
         ORDER BY id DESC
         LIMIT ?3"
    };

    let mut ids = Vec::new();
    if let Some(before) = before {
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map(
            params![conversation.get(), before.get(), sql_limit],
            |row| row.get::<_, i64>(0),
        )?;
        for row in rows {
            ids.push(row?);
        }
    } else {
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map(params![conversation.get(), 0_i64, sql_limit], |row| {
            row.get::<_, i64>(0)
        })?;
        for row in rows {
            ids.push(row?);
        }
    }

    let has_more = ids.len() > limit;
    ids.truncate(limit);
    let mut entries = Vec::with_capacity(ids.len());
    for raw in ids {
        entries.push(load_entry(connection, id::<EntryId>(raw, "entry")?)?);
    }
    entries.reverse();
    let next_before = entries.first().map(|entry| entry.id);
    Ok(EntryPage {
        entries,
        has_more,
        next_before,
    })
}

fn insert_conversation(
    connection: &Connection,
    conversation: &Conversation,
) -> Result<(), StoreError> {
    let (parent, parent_at) = conversation.history_parent.map_or((None, None), |parent| {
        (Some(parent.conversation.get()), Some(parent.at.get()))
    });
    connection.execute(
        "INSERT INTO conversations
         (id, history_parent, history_parent_at, current_config_revision, retired)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            conversation.id.get(),
            parent,
            parent_at,
            conversation.current_config_revision.get(),
            if conversation.retired { 1_i64 } else { 0_i64 }
        ],
    )?;
    Ok(())
}

fn insert_config(
    connection: &Connection,
    conversation: ConversationId,
    installed: &InstalledConfig,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO conversation_configs (conversation_id, revision, config)
         VALUES (?1, ?2, ?3)",
        params![
            conversation.get(),
            installed.revision.get(),
            json_to(&installed.config)?
        ],
    )?;
    Ok(())
}

fn insert_input(
    connection: &Connection,
    input: &Input,
    admitted_commit: CommitSeq,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO inputs
         (id, conversation_id, request_key, sender, mode, body, disposition,
          disposition_kind, placed_turn, placed_entry, admitted_commit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', NULL, NULL, ?8)",
        params![
            input.id.get(),
            input.conversation.get(),
            input.request_key.as_ref().map(RequestKey::as_str),
            json_to(&input.sender)?,
            json_to(&input.mode)?,
            json_to(&input.body)?,
            json_to(&input.disposition)?,
            admitted_commit.get(),
        ],
    )?;
    Ok(())
}

fn update_input_disposition(connection: &Connection, input: &Input) -> Result<(), StoreError> {
    let (kind, placed_turn, placed_entry) = match &input.disposition {
        InputDisposition::Queued => ("queued", None, None),
        InputDisposition::Consumed { turn, entry } => {
            ("consumed", Some(turn.get()), entry.map(EntryId::get))
        }
        InputDisposition::Cancelled => ("cancelled", None, None),
        InputDisposition::Abandoned { turn, entry } => {
            ("abandoned", turn.map(TurnId::get), entry.map(EntryId::get))
        }
    };
    let updated = connection.execute(
        "UPDATE inputs
         SET disposition = ?2, disposition_kind = ?3, placed_turn = ?4, placed_entry = ?5
         WHERE id = ?1",
        params![
            input.id.get(),
            json_to(&input.disposition)?,
            kind,
            placed_turn,
            placed_entry
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::Corrupt(format!(
            "input {} disappeared while updating disposition",
            input.id
        )));
    }
    Ok(())
}

pub(super) fn insert_entry(
    connection: &Connection,
    entry: &Entry,
    commit: CommitSeq,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO entries (id, conversation_id, commit_seq, kind, data, projection)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.id.get(),
            entry.conversation.get(),
            commit.get(),
            entry.kind_name(),
            json_to(&entry.data)?,
            json_to(&entry.projection)?,
        ],
    )?;
    Ok(())
}

fn insert_turn(connection: &Connection, turn: &Turn) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO turns
         (id, conversation_id, environment, settings_revision, settings, phase,
          cancellation_generation, cancel_requested, budget, admitted_at,
          wall_deadline, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            turn.id.get(),
            turn.conversation.get(),
            json_to(&turn.environment)?,
            i64::from(turn.settings.revision),
            json_to(&turn.settings)?,
            json_to(&turn.phase)?,
            i64_from_u64(turn.cancellation.generation, "cancellation generation")?,
            if turn.cancellation.requested {
                1_i64
            } else {
                0_i64
            },
            json_to(&turn.budget)?,
            turn.admitted_at_unix_ms,
            turn.wall_deadline_unix_ms,
            turn.outcome.as_ref().map(json_to).transpose()?,
        ],
    )?;
    Ok(())
}

fn load_conversation(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Conversation, StoreError> {
    let row = connection
        .query_row(
            "SELECT history_parent, history_parent_at, current_config_revision, retired
             FROM conversations WHERE id = ?1",
            [conversation_id.get()],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "conversation",
            id: conversation_id.get(),
        })?;
    let history_parent = match (row.0, row.1) {
        (None, None) => None,
        (Some(parent), Some(at)) => Some(HistoryParent {
            conversation: id::<ConversationId>(parent, "history parent")?,
            at: id::<EntryId>(at, "history parent cutoff")?,
        }),
        _ => {
            return Err(StoreError::Corrupt(format!(
                "conversation {conversation_id} has a partial history parent"
            )));
        }
    };
    Ok(Conversation {
        id: conversation_id,
        history_parent,
        current_config_revision: id::<CommitSeq>(row.2, "config revision")?,
        retired: strict_bool(row.3, "conversation retired flag")?,
    })
}

fn load_config_revision(
    connection: &Connection,
    conversation: ConversationId,
    revision: CommitSeq,
) -> Result<InstalledConfig, StoreError> {
    let raw = connection
        .query_row(
            "SELECT config FROM conversation_configs
             WHERE conversation_id = ?1 AND revision = ?2",
            params![conversation.get(), revision.get()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "configuration revision",
            id: revision.get(),
        })?;
    decode_installed_config(conversation, revision.get(), &raw)
}

fn decode_installed_config(
    conversation: ConversationId,
    revision: i64,
    raw: &str,
) -> Result<InstalledConfig, StoreError> {
    let revision = id::<CommitSeq>(revision, "config revision")?;
    let config: ConversationConfig = json_from(raw, "conversation config")?;
    config.validate().map_err(|error| {
        StoreError::Corrupt(format!(
            "conversation {conversation} config {revision} is invalid: {error}"
        ))
    })?;
    Ok(InstalledConfig { revision, config })
}

fn load_input(
    connection: &Connection,
    input_id: InputId,
) -> Result<(Input, CommitSeq), StoreError> {
    let row = connection
        .query_row(
            "SELECT conversation_id, request_key, sender, mode, body, disposition,
                    disposition_kind, placed_turn, placed_entry, admitted_commit
             FROM inputs WHERE id = ?1",
            [input_id.get()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "input",
            id: input_id.get(),
        })?;
    let disposition: InputDisposition = json_from(&row.5, "input disposition")?;
    let expected_kind = match &disposition {
        InputDisposition::Queued => "queued",
        InputDisposition::Consumed { .. } => "consumed",
        InputDisposition::Cancelled => "cancelled",
        InputDisposition::Abandoned { .. } => "abandoned",
    };
    if row.6 != expected_kind {
        return Err(StoreError::Corrupt(format!(
            "input {input_id} disposition index {:?} disagrees with payload {expected_kind:?}",
            row.6
        )));
    }
    let expected_placement = match &disposition {
        InputDisposition::Queued | InputDisposition::Cancelled => (None, None),
        InputDisposition::Consumed { turn, entry } => (Some(turn.get()), entry.map(EntryId::get)),
        InputDisposition::Abandoned { turn, entry } => {
            (turn.map(TurnId::get), entry.map(EntryId::get))
        }
    };
    if (row.7, row.8) != expected_placement {
        return Err(StoreError::Corrupt(format!(
            "input {input_id} placement columns disagree with disposition payload"
        )));
    }
    let request_key = row.1.map(RequestKey::new).transpose().map_err(|error| {
        StoreError::Corrupt(format!("input {input_id} has invalid request key: {error}"))
    })?;
    Ok((
        Input {
            id: input_id,
            conversation: id::<ConversationId>(row.0, "input conversation")?,
            sender: json_from(&row.2, "input sender")?,
            mode: json_from(&row.3, "input mode")?,
            request_key,
            body: json_from(&row.4, "input body")?,
            disposition,
        },
        id::<CommitSeq>(row.9, "input admission commit")?,
    ))
}

pub(super) fn load_entry(connection: &Connection, entry_id: EntryId) -> Result<Entry, StoreError> {
    let row = connection
        .query_row(
            "SELECT conversation_id, kind, data, projection
             FROM entries WHERE id = ?1",
            [entry_id.get()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "entry",
            id: entry_id.get(),
        })?;
    let data: EntryData = json_from(&row.2, "entry data")?;
    let entry = Entry {
        id: entry_id,
        conversation: id::<ConversationId>(row.0, "entry conversation")?,
        data,
        projection: json_from(&row.3, "entry projection")?,
    };
    if row.1 != entry.kind_name() {
        return Err(StoreError::Corrupt(format!(
            "entry {entry_id} kind {:?} disagrees with payload {:?}",
            row.1,
            entry.kind_name()
        )));
    }
    Ok(entry)
}

pub(super) fn load_turn(connection: &Connection, turn_id: TurnId) -> Result<Turn, StoreError> {
    type Row = (
        i64,
        String,
        i64,
        String,
        String,
        i64,
        i64,
        String,
        i64,
        Option<i64>,
        Option<String>,
    );
    let row: Row = connection
        .query_row(
            "SELECT conversation_id, environment, settings_revision, settings, phase,
                    cancellation_generation, cancel_requested, budget, admitted_at,
                    wall_deadline, outcome
             FROM turns WHERE id = ?1",
            [turn_id.get()],
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
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "turn",
            id: turn_id.get(),
        })?;
    let conversation = id::<ConversationId>(row.0, "turn conversation")?;
    let environment: crate::TurnEnvironment = json_from(&row.1, "turn environment")?;
    let settings_revision = u32::try_from(row.2).map_err(|_| {
        StoreError::Corrupt(format!(
            "turn {turn_id} has invalid settings revision {}",
            row.2
        ))
    })?;
    let settings: crate::TurnSettings = json_from(&row.3, "turn settings")?;
    if settings.revision != settings_revision {
        return Err(StoreError::Corrupt(format!(
            "turn {turn_id} settings revision column {settings_revision} disagrees with payload {}",
            settings.revision
        )));
    }
    settings.validate(&environment).map_err(|error| {
        StoreError::Corrupt(format!("turn {turn_id} settings are invalid: {error}"))
    })?;
    let config_exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM conversation_configs
            WHERE conversation_id = ?1 AND revision = ?2
         )",
        params![conversation.get(), environment.config_revision.get()],
        |row| row.get(0),
    )?;
    if !config_exists {
        return Err(StoreError::Corrupt(format!(
            "turn {turn_id} references missing config revision {}",
            environment.config_revision
        )));
    }
    let generation = u64::try_from(row.5).map_err(|_| {
        StoreError::Corrupt(format!(
            "turn {turn_id} has negative cancellation generation {}",
            row.5
        ))
    })?;
    Ok(Turn {
        id: turn_id,
        conversation,
        environment,
        settings,
        phase: json_from(&row.4, "turn phase")?,
        cancellation: Cancellation {
            requested: strict_bool(row.6, "turn cancellation flag")?,
            generation,
        },
        budget: json_from(&row.7, "turn budget")?,
        admitted_at_unix_ms: row.8,
        wall_deadline_unix_ms: row.9,
        outcome: row
            .10
            .as_deref()
            .map(|raw| json_from(raw, "turn outcome"))
            .transpose()?,
    })
}

pub(super) fn advance_metadata(
    transaction: &Transaction<'_>,
    sequence: &Sequence,
    commit: CommitSeq,
    primary: Option<ConversationId>,
) -> Result<(), StoreError> {
    let updated = if let Some(primary) = primary {
        transaction.execute(
            "UPDATE session_meta
             SET last_seq = ?1, last_commit = ?2, primary_conversation = ?3
             WHERE id = 1 AND last_seq = ?4 AND primary_conversation IS NULL",
            params![sequence.last, commit.get(), primary.get(), sequence.base],
        )?
    } else {
        transaction.execute(
            "UPDATE session_meta
             SET last_seq = ?1, last_commit = ?2
             WHERE id = 1 AND last_seq = ?3",
            params![sequence.last, commit.get(), sequence.base],
        )?
    };
    if updated != 1 {
        return Err(StoreError::Sqlite(
            "session sequence compare-and-set failed".to_owned(),
        ));
    }
    Ok(())
}

fn validate_new_config(config: &ConversationConfig) -> Result<(), StoreError> {
    config.validate().map_err(|error| {
        StoreError::InvalidRequest(format!("invalid conversation config: {error}"))
    })
}

pub(super) fn json_to<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::InvalidState(format!("cannot encode durable value: {error}")))
}

pub(super) fn json_from<T: DeserializeOwned>(raw: &str, label: &str) -> Result<T, StoreError> {
    serde_json::from_str(raw)
        .map_err(|error| StoreError::Corrupt(format!("invalid {label}: {error}")))
}

fn strict_bool(value: i64, label: &str) -> Result<bool, StoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(StoreError::Corrupt(format!(
            "{label} must be 0 or 1, got {other}"
        ))),
    }
}

fn id<T>(value: i64, label: &str) -> Result<T, StoreError>
where
    T: TryFrom<i64>,
    <T as TryFrom<i64>>::Error: std::fmt::Display,
{
    T::try_from(value)
        .map_err(|error| StoreError::Corrupt(format!("invalid {label} {value}: {error}")))
}

fn i64_from_u64(value: u64, label: &str) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::InvalidState(format!("{label} exceeds SQLite integer range")))
}

fn sql_limit_plus_one(limit: usize) -> Result<i64, StoreError> {
    let value = limit
        .checked_add(1)
        .ok_or_else(|| StoreError::InvalidRequest("limit overflow".to_owned()))?;
    i64::try_from(value)
        .map_err(|_| StoreError::InvalidRequest("limit exceeds SQLite integer range".to_owned()))
}

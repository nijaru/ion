//! Schema and version handling for the per-session database.
//!
//! One database holds exactly one session. The schema is deliberately fresh:
//! development databases written by the replaced task runtime are refused
//! rather than migrated, because reinterpreting their records would invent
//! semantics this build does not share.

use rusqlite::Connection;

use super::StoreError;
use crate::error::Error;

/// The turn-engine schema. Version 6 and below belonged to the replaced task
/// runtime.
pub(crate) const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r"
CREATE TABLE session_meta (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    session_id        TEXT    NOT NULL,
    last_seq          INTEGER,
    last_commit       INTEGER,
    root_conversation INTEGER,
    used_bytes        INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE conversations (
    id              INTEGER PRIMARY KEY,
    parent_id       INTEGER REFERENCES conversations(id),
    parent_at       INTEGER,
    config          TEXT,
    config_revision INTEGER
);

CREATE INDEX conversations_by_parent ON conversations (parent_id);

CREATE TABLE entries (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id),
    kind            TEXT    NOT NULL,
    data            TEXT    NOT NULL,
    projection      TEXT    NOT NULL
);

CREATE INDEX entries_by_conversation ON entries (conversation_id, id);

CREATE TABLE inputs (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id),
    request_key     TEXT    UNIQUE,
    sender          TEXT    NOT NULL,
    mode            TEXT    NOT NULL,
    body            TEXT    NOT NULL,
    disposition     TEXT    NOT NULL,
    placed_entry    INTEGER,
    placed_turn     INTEGER
);

CREATE INDEX inputs_by_conversation ON inputs (conversation_id, id);
CREATE INDEX inputs_queued ON inputs (conversation_id, id) WHERE disposition = 'queued';

CREATE TABLE turns (
    id                 INTEGER PRIMARY KEY,
    conversation_id    INTEGER NOT NULL REFERENCES conversations(id),
    phase              TEXT    NOT NULL,
    step               INTEGER,
    blocked_invocation INTEGER,
    generation         INTEGER NOT NULL DEFAULT 0,
    cancel_requested   INTEGER NOT NULL DEFAULT 0,
    limits             TEXT    NOT NULL,
    admitted_at        INTEGER NOT NULL,
    steps_used         INTEGER NOT NULL DEFAULT 0,
    outcome            TEXT
);

CREATE UNIQUE INDEX turns_unfinished ON turns (conversation_id) WHERE outcome IS NULL;

CREATE TABLE steps (
    id              INTEGER PRIMARY KEY,
    turn_id         INTEGER NOT NULL REFERENCES turns(id),
    ordinal         INTEGER NOT NULL,
    cut             INTEGER,
    config_revision INTEGER NOT NULL,
    model           TEXT    NOT NULL,
    instructions    TEXT    NOT NULL,
    context         TEXT    NOT NULL,
    controls        TEXT    NOT NULL,
    tools           TEXT    NOT NULL,
    max_request_bytes INTEGER NOT NULL,
    UNIQUE (turn_id, ordinal)
);

CREATE TABLE attempts (
    id         INTEGER PRIMARY KEY,
    step_id    INTEGER NOT NULL REFERENCES steps(id),
    ordinal    INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    state      TEXT    NOT NULL,
    response   TEXT,
    UNIQUE (step_id, ordinal)
);

CREATE TABLE invocations (
    id             INTEGER PRIMARY KEY,
    step_id        INTEGER NOT NULL REFERENCES steps(id),
    entry_id       INTEGER NOT NULL REFERENCES entries(id),
    call_index     INTEGER NOT NULL,
    call_id        TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    arguments      TEXT    NOT NULL,
    implementation TEXT    NOT NULL,
    repeat_safe    INTEGER NOT NULL,
    generation     INTEGER NOT NULL,
    state          TEXT    NOT NULL,
    result         TEXT,
    message        TEXT,
    UNIQUE (entry_id, call_index)
);

CREATE INDEX invocations_by_step ON invocations (step_id, id);
";

pub(crate) fn initialize(connection: &Connection) -> Result<(), StoreError> {
    let existing: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if existing != 0 {
        return Err(StoreError::Rejected(Error::UnsupportedSchema {
            found: existing,
            expected: SCHEMA_VERSION,
        }));
    }
    connection.execute_batch(DDL)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub(crate) fn verify(connection: &Connection) -> Result<(), StoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(StoreError::Rejected(Error::UnsupportedSchema {
            found: version,
            expected: SCHEMA_VERSION,
        }));
    }
    Ok(())
}

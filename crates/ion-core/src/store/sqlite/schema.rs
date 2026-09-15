//! Schema and version handling for the per-session database.
//!
//! One database holds exactly one session. The schema is deliberately fresh:
//! older development databases are refused rather than migrated.

use rusqlite::Connection;

use super::StoreError;

/// Version 6 stores installed conversation configuration and its commit revision.
/// Older unreleased development databases are refused by `verify`.
pub(crate) const SCHEMA_VERSION: i64 = 6;

const DDL: &str = r"
CREATE TABLE session_meta (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    session_id        TEXT    NOT NULL,
    last_seq          INTEGER,
    last_commit       INTEGER,
    root_conversation INTEGER
);

CREATE TABLE conversations (
    id             INTEGER PRIMARY KEY,
    parent_id      INTEGER,
    parent_at      INTEGER,
    owner_task     INTEGER,
    foreground_turn INTEGER,
    turn_cancelled INTEGER NOT NULL DEFAULT 0,
    retired        INTEGER NOT NULL DEFAULT 0,
    config         TEXT,
    config_revision INTEGER
);

CREATE TABLE entries (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL,
    kind            TEXT    NOT NULL,
    data            TEXT    NOT NULL,
    projection      TEXT    NOT NULL,
    context         TEXT    NOT NULL
);

CREATE INDEX entries_by_conversation ON entries (conversation_id, id);

CREATE TABLE inputs (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL,
    request_key     TEXT    UNIQUE,
    sender          TEXT    NOT NULL,
    mode            TEXT    NOT NULL,
    body            TEXT    NOT NULL,
    disposition     TEXT    NOT NULL,
    commit_seq      INTEGER
);

CREATE INDEX inputs_by_conversation ON inputs (conversation_id, id);

CREATE TABLE tasks (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL,
    kind            TEXT    NOT NULL,
    schema_version  INTEGER NOT NULL,
    input           TEXT    NOT NULL,
    checkpoint      TEXT,
    turn            INTEGER,
    turn_closed_by  INTEGER,
    generation      INTEGER NOT NULL,
    invocation      TEXT,
    cancel_requested INTEGER NOT NULL,
    state           TEXT    NOT NULL CHECK (state IN ('pending', 'running', 'terminal')),
    outcome         TEXT,
    output          TEXT
);

CREATE INDEX tasks_by_conversation ON tasks (conversation_id, id);
CREATE INDEX tasks_by_state ON tasks (state);

CREATE TABLE task_dependencies (
    task_id    INTEGER NOT NULL,
    position   INTEGER NOT NULL,
    depends_on INTEGER NOT NULL,
    PRIMARY KEY (task_id, position)
);

CREATE TABLE task_ownership (
    task_id         INTEGER NOT NULL,
    position        INTEGER NOT NULL,
    conversation_id INTEGER NOT NULL,
    PRIMARY KEY (task_id, position)
);
";

pub(crate) fn initialize(connection: &Connection) -> Result<(), StoreError> {
    let existing: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if existing != 0 {
        return Err(StoreError::other(format!(
            "refusing to initialize a database that already has schema version {existing}"
        )));
    }
    connection.execute_batch(DDL)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub(crate) fn verify(connection: &Connection) -> Result<(), StoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(StoreError::other(format!(
            "unsupported session schema version {version}; this build writes version {SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

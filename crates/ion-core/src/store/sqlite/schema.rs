//! Fresh schema for the replacement coding-turn runtime.
//!
//! Version 1 belonged to the prototype turn runtime. R1 does not migrate it.

use rusqlite::{Connection, OptionalExtension};

use super::super::StoreError;
use crate::SessionId;

pub(crate) const SCHEMA_VERSION: i64 = 2;

const DDL: &str = r#"
CREATE TABLE session_meta (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    session_id           TEXT    NOT NULL,
    last_seq             INTEGER NOT NULL DEFAULT 0 CHECK (last_seq >= 0),
    last_commit          INTEGER,
    primary_conversation INTEGER,
    used_bytes           INTEGER NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    tombstoned           INTEGER NOT NULL DEFAULT 0 CHECK (tombstoned IN (0, 1))
);

CREATE TABLE conversations (
    id                      INTEGER PRIMARY KEY,
    history_parent           INTEGER REFERENCES conversations(id),
    history_parent_at        INTEGER,
    current_config_revision  INTEGER NOT NULL,
    retired                  INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
    CHECK ((history_parent IS NULL) = (history_parent_at IS NULL))
);
CREATE INDEX conversations_by_parent ON conversations (history_parent);

CREATE TABLE conversation_configs (
    conversation_id INTEGER NOT NULL REFERENCES conversations(id),
    revision        INTEGER NOT NULL,
    config          TEXT    NOT NULL,
    PRIMARY KEY (conversation_id, revision)
);

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
    request_key     TEXT,
    sender          TEXT    NOT NULL,
    mode            TEXT    NOT NULL,
    body            TEXT    NOT NULL,
    disposition     TEXT    NOT NULL
);
CREATE INDEX inputs_by_conversation ON inputs (conversation_id, id);
CREATE UNIQUE INDEX inputs_request_key
    ON inputs (conversation_id, request_key)
    WHERE request_key IS NOT NULL;

CREATE TABLE turns (
    id                      INTEGER PRIMARY KEY,
    conversation_id         INTEGER NOT NULL REFERENCES conversations(id),
    environment             TEXT    NOT NULL,
    settings_revision       INTEGER NOT NULL CHECK (settings_revision >= 0),
    settings                TEXT    NOT NULL,
    phase                   TEXT    NOT NULL,
    cancellation_generation INTEGER NOT NULL DEFAULT 0 CHECK (cancellation_generation >= 0),
    cancel_requested        INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    budget                  TEXT    NOT NULL,
    admitted_at             INTEGER NOT NULL,
    wall_deadline           INTEGER,
    outcome                 TEXT
);
CREATE UNIQUE INDEX turns_unfinished ON turns (conversation_id) WHERE outcome IS NULL;

CREATE TABLE model_steps (
    id          INTEGER PRIMARY KEY,
    turn_id     INTEGER NOT NULL REFERENCES turns(id),
    ordinal     INTEGER NOT NULL CHECK (ordinal >= 0),
    purpose     TEXT    NOT NULL,
    manifest    TEXT    NOT NULL,
    disposition TEXT    NOT NULL,
    UNIQUE (turn_id, ordinal)
);
CREATE INDEX model_steps_by_turn ON model_steps (turn_id, ordinal);

CREATE TABLE model_attempts (
    id         INTEGER PRIMARY KEY,
    step_id    INTEGER NOT NULL REFERENCES model_steps(id),
    ordinal    INTEGER NOT NULL CHECK (ordinal > 0),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    timing     TEXT    NOT NULL,
    cost_quote TEXT,
    state      TEXT    NOT NULL,
    UNIQUE (step_id, ordinal)
);
CREATE INDEX model_attempts_by_step ON model_attempts (step_id, ordinal);

CREATE TABLE tool_invocations (
    id                      INTEGER PRIMARY KEY,
    step_id                 INTEGER NOT NULL REFERENCES model_steps(id),
    assistant_entry         INTEGER NOT NULL REFERENCES entries(id),
    source_index            INTEGER NOT NULL CHECK (source_index >= 0),
    origin_provider_call_id TEXT,
    binding_id              TEXT    NOT NULL,
    prepared_action         TEXT    NOT NULL,
    approval                TEXT    NOT NULL,
    exchange_state          TEXT    NOT NULL,
    UNIQUE (assistant_entry, source_index)
);
CREATE INDEX tool_invocations_by_step ON tool_invocations (step_id, source_index);

CREATE TABLE tool_attempts (
    id            INTEGER PRIMARY KEY,
    invocation_id INTEGER NOT NULL REFERENCES tool_invocations(id),
    ordinal       INTEGER NOT NULL CHECK (ordinal > 0),
    generation    INTEGER NOT NULL CHECK (generation >= 0),
    executor      TEXT    NOT NULL,
    progress      TEXT,
    state         TEXT    NOT NULL,
    UNIQUE (invocation_id, ordinal)
);
CREATE INDEX tool_attempts_by_invocation ON tool_attempts (invocation_id, ordinal);

CREATE TABLE blobs (
    digest            TEXT PRIMARY KEY,
    length            INTEGER NOT NULL CHECK (length >= 0),
    media_type        TEXT,
    encoding          TEXT,
    semantic_required INTEGER NOT NULL CHECK (semantic_required IN (0, 1))
);
"#;

pub(super) fn initialize(connection: &Connection, session_id: SessionId) -> Result<(), StoreError> {
    let existing: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if existing != 0 {
        return Err(StoreError::UnsupportedSchema {
            found: existing,
            expected: SCHEMA_VERSION,
        });
    }
    connection.execute_batch(DDL)?;
    connection.execute(
        "INSERT INTO session_meta (id, session_id) VALUES (1, ?1)",
        [session_id.to_string()],
    )?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub(super) fn verify(connection: &Connection) -> Result<(), StoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: version,
            expected: SCHEMA_VERSION,
        });
    }
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM session_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(StoreError::Corrupt(
            "session_meta must contain exactly one id=1 row".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn read_session_id(connection: &Connection) -> Result<SessionId, StoreError> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT session_id FROM session_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let raw = raw.ok_or_else(|| StoreError::Corrupt("session metadata is missing".to_owned()))?;
    raw.parse::<SessionId>()
        .map_err(|error| StoreError::Corrupt(format!("invalid session id: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_schema_has_no_prototype_attempt_or_invocation_tables() {
        let connection = Connection::open_in_memory().expect("sqlite");
        initialize(&connection, SessionId::new()).expect("schema");

        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("statement");
        let names: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .expect("rows")
            .map(|row| row.expect("name"))
            .collect();

        assert!(names.contains(&"model_attempts".to_owned()));
        assert!(names.contains(&"tool_attempts".to_owned()));
        assert!(names.contains(&"tool_invocations".to_owned()));
        assert!(!names.contains(&"attempts".to_owned()));
        assert!(!names.contains(&"invocations".to_owned()));
    }

    #[test]
    fn old_schema_version_is_refused_not_migrated() {
        let connection = Connection::open_in_memory().expect("sqlite");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("version");
        let error = verify(&connection).expect_err("v1 must be refused");
        assert!(matches!(
            error,
            StoreError::UnsupportedSchema {
                found: 1,
                expected: SCHEMA_VERSION
            }
        ));
    }
}

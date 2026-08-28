use rusqlite::Connection;

use super::StoreError;

const SCHEMA_VERSION: i64 = 15;

/// What an existing database needs before the store can open it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SchemaPlan {
    /// No database yet: create the current schema.
    Fresh,
    /// Already at the current version.
    Current,
    /// Written by an older development build. The caller archives the
    /// files and reopens fresh; bytes are never migrated or
    /// reinterpreted.
    ArchiveOlder(i64),
}

pub(super) fn schema_version(connection: &Connection) -> Result<i64, StoreError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StoreError::from)
}

/// Schema gating. Ion is pre-1.0 with no durable compatibility promise yet:
/// a database from this build is used as-is, a missing database gets the
/// current schema, and an older development database is archived untouched so
/// a fresh one can be created. A database from a newer Ion is refused rather
/// than silently misread.
pub(super) fn classify(connection: &Connection) -> Result<SchemaPlan, StoreError> {
    let version = schema_version(connection)?;
    if version == SCHEMA_VERSION {
        return Ok(SchemaPlan::Current);
    }
    if version == 0
        && connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count == 0)?
    {
        return Ok(SchemaPlan::Fresh);
    }
    if version > 0 && version < SCHEMA_VERSION {
        return Ok(SchemaPlan::ArchiveOlder(version));
    }
    Err(StoreError::Sqlite(format!(
        "database schema version {version} is newer than this Ion ({SCHEMA_VERSION}); \
         refusing to open it with an older build — use a newer Ion or move the \
         database aside"
    )))
}

/// Create the current schema on a fresh (or freshly archived) database.
pub(super) fn create_fresh(connection: &mut Connection) -> Result<(), StoreError> {
    connection.execute_batch(SCHEMA)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL,
    step INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    recorded_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL,
    parent_session_id TEXT
);

CREATE TABLE IF NOT EXISTS entries (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    seq INTEGER NOT NULL,
    id TEXT NOT NULL UNIQUE,
    parent_id TEXT,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq),
    UNIQUE (session_id, id),
    FOREIGN KEY (session_id, parent_id) REFERENCES entries(session_id, id)
);

CREATE TABLE IF NOT EXISTS lanes (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    name TEXT NOT NULL,
    leaf_id TEXT,
    config TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, name),
    FOREIGN KEY (session_id, leaf_id) REFERENCES entries(session_id, id)
);

CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    kind TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    accepted_seq INTEGER NOT NULL
);

-- Direct latest-state projection used by recovery. The append-only revision
-- ledger below is populated atomically by triggers from every insert/update,
-- so the projection remains cheap while complete total states are retained.
CREATE TABLE IF NOT EXISTS operation_state (
    operation_id TEXT NOT NULL REFERENCES operations(id),
    state_seq INTEGER NOT NULL CHECK (state_seq >= 1),
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (operation_id)
);

CREATE TABLE IF NOT EXISTS operation_state_revisions (
    operation_id TEXT NOT NULL REFERENCES operations(id),
    state_seq INTEGER NOT NULL CHECK (state_seq >= 1),
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (operation_id, state_seq)
);

CREATE INDEX IF NOT EXISTS operation_state_revisions_latest
    ON operation_state_revisions (operation_id, state_seq DESC);

-- The ledger, not the mutable current projection, owns revision ordering.
-- This also works correctly with SQLite UPSERT: its BEFORE INSERT phase runs
-- before conflict resolution, whereas the ledger append happens only after the
-- chosen insert/update arm has produced the new current state.
CREATE TRIGGER IF NOT EXISTS operation_state_revision_order
BEFORE INSERT ON operation_state_revisions
WHEN NEW.state_seq != COALESCE(
    (SELECT MAX(state_seq) + 1
     FROM operation_state_revisions
     WHERE operation_id = NEW.operation_id),
    1
)
BEGIN
    SELECT RAISE(ABORT, 'operation state revisions must increment by one');
END;

CREATE TRIGGER IF NOT EXISTS operation_state_revision_on_insert
AFTER INSERT ON operation_state
BEGIN
    INSERT INTO operation_state_revisions (
        operation_id, state_seq, kind, payload, created_at
    ) VALUES (
        NEW.operation_id, NEW.state_seq, NEW.kind, NEW.payload, NEW.created_at
    );
END;

CREATE TRIGGER IF NOT EXISTS operation_state_revision_on_update
AFTER UPDATE ON operation_state
BEGIN
    INSERT INTO operation_state_revisions (
        operation_id, state_seq, kind, payload, created_at
    ) VALUES (
        NEW.operation_id, NEW.state_seq, NEW.kind, NEW.payload, NEW.created_at
    );
END;

CREATE TRIGGER IF NOT EXISTS operation_state_revisions_no_update
BEFORE UPDATE ON operation_state_revisions
BEGIN
    SELECT RAISE(ABORT, 'operation state revisions are append-only');
END;

CREATE TRIGGER IF NOT EXISTS operation_state_revisions_no_delete
BEFORE DELETE ON operation_state_revisions
BEGIN
    SELECT RAISE(ABORT, 'operation state revisions are append-only');
END;

CREATE TABLE IF NOT EXISTS inbox_items (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    status TEXT NOT NULL,
    accepted_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS effects (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL REFERENCES operations(id),
    kind TEXT NOT NULL,
    recovery_class TEXT NOT NULL,
    status TEXT NOT NULL,
    effective_input TEXT NOT NULL,
    settlement TEXT,
    created_at INTEGER NOT NULL,
    settled_at INTEGER,
    attempt INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS capability_snapshots (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS context_manifests (
    id TEXT PRIMARY KEY,
    capability_snapshot_id TEXT NOT NULL REFERENCES capability_snapshots(id),
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS model_steps (
    effect_id TEXT PRIMARY KEY REFERENCES effects(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    step INTEGER NOT NULL,
    model_ref TEXT NOT NULL,
    context_window INTEGER,
    capability_snapshot_id TEXT NOT NULL REFERENCES capability_snapshots(id),
    context_manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    capabilities TEXT NOT NULL,
    context_fingerprint TEXT NOT NULL,
    cache_expectation TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS assistant_frames (
    effect_id TEXT PRIMARY KEY REFERENCES effects(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    step INTEGER NOT NULL,
    frame_seq INTEGER NOT NULL,
    text TEXT NOT NULL,
    thinking TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_progress (
    effect_id TEXT PRIMARY KEY REFERENCES effects(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    call_id INTEGER NOT NULL,
    output TEXT NOT NULL,
    updated_at INTEGER NOT NULL
)
";

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("database");
        create_fresh(&mut connection).expect("schema");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        connection
            .execute(
                "INSERT INTO sessions (id, created_at, updated_at, cwd, title, parent_session_id)
                 VALUES ('session', 0, 0, '/tmp', '', NULL)",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation', 'session', 'run', 0, 1)",
                [],
            )
            .expect("operation");
        connection
    }

    #[test]
    fn operation_state_keeps_append_only_total_revisions() {
        let connection = seeded_connection();
        connection
            .execute(
                "INSERT INTO operation_state (operation_id, state_seq, kind, payload, created_at)
                 VALUES ('operation', 1, 'accepted', '{\"phase\":1}', 1)",
                [],
            )
            .expect("first revision");
        connection
            .execute(
                "INSERT INTO operation_state (operation_id, state_seq, kind, payload, created_at)
                 VALUES ('operation', 2, 'need_assistant', '{\"phase\":2}', 2)
                 ON CONFLICT(operation_id) DO UPDATE SET
                     state_seq = excluded.state_seq,
                     kind = excluded.kind,
                     payload = excluded.payload,
                     created_at = excluded.created_at",
                [],
            )
            .expect("second revision");

        let revisions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM operation_state_revisions WHERE operation_id = 'operation'",
                [],
                |row| row.get(0),
            )
            .expect("revision count");
        let latest: i64 = connection
            .query_row(
                "SELECT state_seq FROM operation_state WHERE operation_id = 'operation'",
                [],
                |row| row.get(0),
            )
            .expect("latest projection");
        assert_eq!(revisions, 2);
        assert_eq!(latest, 2);

        let skipped = connection.execute(
            "INSERT INTO operation_state (operation_id, state_seq, kind, payload, created_at)
             VALUES ('operation', 4, 'invalid', '{}', 4)
             ON CONFLICT(operation_id) DO UPDATE SET
                 state_seq = excluded.state_seq,
                 kind = excluded.kind,
                 payload = excluded.payload,
                 created_at = excluded.created_at",
            [],
        );
        assert!(skipped.is_err(), "revision gaps must fail");
        let revisions_after: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM operation_state_revisions WHERE operation_id = 'operation'",
                [],
                |row| row.get(0),
            )
            .expect("revision count after rejection");
        let latest_after: i64 = connection
            .query_row(
                "SELECT state_seq FROM operation_state WHERE operation_id = 'operation'",
                [],
                |row| row.get(0),
            )
            .expect("latest projection after rejection");
        assert_eq!(revisions_after, 2);
        assert_eq!(latest_after, 2);
    }

    #[test]
    fn operation_state_revision_rows_cannot_be_rewritten() {
        let connection = seeded_connection();
        connection
            .execute(
                "INSERT INTO operation_state (operation_id, state_seq, kind, payload, created_at)
                 VALUES ('operation', 1, 'accepted', '{}', 1)",
                [],
            )
            .expect("first revision");

        assert!(
            connection
                .execute(
                    "UPDATE operation_state_revisions SET kind = 'rewritten'
                     WHERE operation_id = 'operation' AND state_seq = 1",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM operation_state_revisions
                     WHERE operation_id = 'operation' AND state_seq = 1",
                    [],
                )
                .is_err()
        );
    }
}

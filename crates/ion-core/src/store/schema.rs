use rusqlite::Connection;

use super::StoreError;

const SCHEMA_VERSION: i64 = 23;

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
    control_parent_session_id TEXT REFERENCES sessions(id),
    fork_source_session_id TEXT REFERENCES sessions(id),
    fork_source_entry_id TEXT,
    FOREIGN KEY (fork_source_session_id, fork_source_entry_id)
        REFERENCES entries(session_id, id),
    CHECK (fork_source_entry_id IS NULL OR fork_source_session_id IS NOT NULL)
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
    current_operation_id TEXT,
    pending_entry_id TEXT,
    pending_prompt TEXT,
    pending_shell_entry_id TEXT,
    pending_shell_command TEXT NOT NULL DEFAULT '',
    pending_shell_excluded INTEGER NOT NULL DEFAULT 0,
    config TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, name),
    FOREIGN KEY (session_id, leaf_id) REFERENCES entries(session_id, id),
    FOREIGN KEY (session_id, current_operation_id) REFERENCES operations(session_id, id),
    CHECK (
        (pending_entry_id IS NULL AND pending_prompt IS NULL)
        OR
        (pending_entry_id IS NOT NULL AND pending_prompt IS NOT NULL)
    ),
    CHECK (
        (pending_shell_entry_id IS NULL AND pending_shell_command = '')
        OR
        (pending_shell_entry_id IS NOT NULL AND pending_shell_command <> '')
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS lanes_pending_entry_unique
    ON lanes (pending_entry_id) WHERE pending_entry_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    family_session_id TEXT NOT NULL REFERENCES sessions(id),
    control_parent_id TEXT REFERENCES agents(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    lane_name TEXT NOT NULL,
    history_kind TEXT NOT NULL,
    source_session_id TEXT REFERENCES sessions(id),
    source_entry_id TEXT,
    created_at INTEGER NOT NULL,
    UNIQUE (session_id, lane_name),
    FOREIGN KEY (session_id, lane_name) REFERENCES lanes(session_id, name),
    FOREIGN KEY (source_session_id, source_entry_id) REFERENCES entries(session_id, id),
    CHECK (
        (history_kind = 'root'
         AND control_parent_id IS NULL
         AND id = family_session_id
         AND session_id = family_session_id
         AND lane_name = 'main'
         AND source_session_id IS NULL
         AND source_entry_id IS NULL)
        OR
        (history_kind = 'lane'
         AND control_parent_id IS NOT NULL
         AND session_id = family_session_id
         AND source_session_id = family_session_id)
        OR
        (history_kind = 'fresh'
         AND control_parent_id IS NOT NULL
         AND id = session_id
         AND session_id != family_session_id
         AND lane_name = 'main'
         AND source_session_id IS NULL
         AND source_entry_id IS NULL)
        OR
        (history_kind = 'fork'
         AND control_parent_id IS NOT NULL
         AND id = session_id
         AND session_id != family_session_id
         AND lane_name = 'main'
         AND source_session_id IS NOT NULL)
    )
);

CREATE TRIGGER IF NOT EXISTS agents_no_update
BEFORE UPDATE ON agents
BEGIN
    SELECT RAISE(ABORT, 'agent topology is immutable');
END;

CREATE TRIGGER IF NOT EXISTS agents_no_delete
BEFORE DELETE ON agents
BEGIN
    SELECT RAISE(ABORT, 'agent topology is immutable');
END;

CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    kind TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    accepted_seq INTEGER NOT NULL,
    UNIQUE (session_id, id),
    UNIQUE (session_id, accepted_seq)
);

-- Immutable topology captured at operation acceptance. Keeping origin separate
-- from mutable operation state makes it impossible for recovery transitions to
-- accidentally rebind a run to another lane or history position.
CREATE TABLE IF NOT EXISTS operation_origins (
    operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    session_id TEXT NOT NULL,
    lane_name TEXT NOT NULL,
    source_leaf_id TEXT,
    FOREIGN KEY (session_id, lane_name) REFERENCES lanes(session_id, name),
    FOREIGN KEY (session_id, source_leaf_id) REFERENCES entries(session_id, id)
);

-- Operation origin is inserted explicitly by the same store transaction that
-- admits an operation. SQLite foreign keys validate the supplied lane and source
-- leaf; there is no trigger that guesses either fact from a hidden default lane.
CREATE TRIGGER IF NOT EXISTS operation_origins_no_update
BEFORE UPDATE ON operation_origins
BEGIN
    SELECT RAISE(ABORT, 'operation origin is immutable');
END;

CREATE TRIGGER IF NOT EXISTS operation_origins_no_delete
BEFORE DELETE ON operation_origins
BEGIN
    SELECT RAISE(ABORT, 'operation origin is immutable');
END;

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
                "INSERT INTO sessions (id, created_at, updated_at, cwd, title, control_parent_session_id)
                 VALUES ('session', 0, 0, '/tmp', '', NULL)",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO lanes (session_id, name, leaf_id, config, created_at, updated_at)
                 VALUES ('session', 'main', NULL, '{}', 0, 0)",
                [],
            )
            .expect("lane");
        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation', 'session', 'run', 0, 1)",
                [],
            )
            .expect("operation");
        connection
            .execute(
                "INSERT INTO operation_origins
                    (operation_id, session_id, lane_name, source_leaf_id)
                 VALUES ('operation', 'session', 'main', NULL)",
                [],
            )
            .expect("origin");
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

    #[test]
    fn operation_origin_is_captured_and_immutable() {
        let connection = seeded_connection();
        let (lane, source): (String, Option<String>) = connection
            .query_row(
                "SELECT lane_name, source_leaf_id FROM operation_origins
                 WHERE operation_id = 'operation'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("origin");
        assert_eq!(lane, "main");
        assert!(source.is_none());
        assert!(
            connection
                .execute(
                    "UPDATE operation_origins SET lane_name = 'other'
                     WHERE operation_id = 'operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM operation_origins WHERE operation_id = 'operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn operation_origin_captures_non_root_source_leaf() {
        let connection = seeded_connection();
        connection
            .execute(
                "INSERT INTO entries (session_id, seq, id, parent_id, kind, payload, created_at)
                 VALUES ('session', 1, 'entry-1', NULL, 'user_message', '{}', 1)",
                [],
            )
            .expect("entry");
        connection
            .execute(
                "UPDATE lanes SET leaf_id = 'entry-1' WHERE session_id = 'session' AND name = 'main'",
                [],
            )
            .expect("move lane");
        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation-2', 'session', 'run', 2, 2)",
                [],
            )
            .expect("second operation");
        connection
            .execute(
                "INSERT INTO operation_origins
                    (operation_id, session_id, lane_name, source_leaf_id)
                 VALUES ('operation-2', 'session', 'main', 'entry-1')",
                [],
            )
            .expect("second origin");
        let source: Option<String> = connection
            .query_row(
                "SELECT source_leaf_id FROM operation_origins WHERE operation_id = 'operation-2'",
                [],
                |row| row.get(0),
            )
            .expect("source");
        assert_eq!(source.as_deref(), Some("entry-1"));
    }

    #[test]
    fn operation_origin_requires_an_existing_lane() {
        let mut connection = Connection::open_in_memory().expect("database");
        create_fresh(&mut connection).expect("schema");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        connection
            .execute(
                "INSERT INTO sessions (id, created_at, updated_at, cwd, title, control_parent_session_id)
                 VALUES ('orphan', 0, 0, '/tmp', '', NULL)",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('orphan-operation', 'orphan', 'run', 0, 1)",
                [],
            )
            .expect("raw operation row");
        assert!(
            connection
                .execute(
                    "INSERT INTO operation_origins
                        (operation_id, session_id, lane_name, source_leaf_id)
                     VALUES ('orphan-operation', 'orphan', 'missing', NULL)",
                    [],
                )
                .is_err(),
            "an immutable origin cannot name a lane that does not exist"
        );
    }
}

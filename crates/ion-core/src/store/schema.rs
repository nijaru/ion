use rusqlite::Connection;

use super::StoreError;

const SCHEMA_VERSION: i64 = 14;

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

/// Schema gating (DESIGN.md §11.1, §33.12). Ion is v0 with no
/// compatibility guarantees: a database from this build is used as-is,
/// a missing database gets the current schema, and an older dev build's
/// database is archived untouched so a fresh one can be created. A
/// database from a NEWER Ion is refused: opening it would mean this
/// build silently misreading data it does not understand (§26.3).
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

CREATE TABLE IF NOT EXISTS operation_state (
    operation_id TEXT NOT NULL REFERENCES operations(id),
    state_seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (operation_id)
);

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

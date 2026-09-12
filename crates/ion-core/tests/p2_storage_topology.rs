use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use tempfile::TempDir;

const SESSION_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
CREATE TABLE session_meta (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL
);
CREATE TABLE effects (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    status TEXT NOT NULL
);
"#;

const CATALOG_SCHEMA: &str = r#"
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    path TEXT NOT NULL UNIQUE
);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredSession {
    id: String,
    title: String,
    created_at: i64,
    path: PathBuf,
}

struct StorageFixture {
    root: TempDir,
}

impl StorageFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary data root");
        fs::create_dir_all(root.path().join("sessions")).expect("session directory");
        Self { root }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.root()
            .join("sessions")
            .join(id)
            .join("session.sqlite")
    }

    fn create_session(&self, id: &str, title: &str, created_at: i64) -> PathBuf {
        let path = self.session_path(id);
        fs::create_dir_all(path.parent().expect("session parent")).expect("session parent directory");
        let connection = Connection::open(&path).expect("open session database");
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("enable foreign keys");
        connection
            .pragma_update(None, "synchronous", "FULL")
            .expect("full synchronous mode");
        connection
            .execute_batch(SESSION_SCHEMA)
            .expect("create session schema");
        connection
            .execute(
                "INSERT INTO session_meta (id, title, created_at) VALUES (?1, ?2, ?3)",
                params![id, title, created_at],
            )
            .expect("insert session metadata");
        path
    }

    fn catalog_path(&self) -> PathBuf {
        self.root().join("catalog.sqlite")
    }

    fn rebuild_catalog(&self) -> Vec<DiscoveredSession> {
        let discovered = discover_sessions(self.root()).expect("discover session stores");
        let catalog_path = self.catalog_path();
        if catalog_path.exists() {
            fs::remove_file(&catalog_path).expect("remove old catalog");
        }
        let connection = Connection::open(&catalog_path).expect("open catalog");
        connection
            .execute_batch(CATALOG_SCHEMA)
            .expect("create catalog schema");
        for session in &discovered {
            connection
                .execute(
                    "INSERT INTO sessions (id, title, created_at, path) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        session.id,
                        session.title,
                        session.created_at,
                        session.path.to_string_lossy()
                    ],
                )
                .expect("publish discovered session");
        }
        discovered
    }
}

fn discover_sessions(root: &Path) -> rusqlite::Result<Vec<DiscoveredSession>> {
    let sessions_root = root.join("sessions");
    let mut discovered = Vec::new();
    let entries = match fs::read_dir(&sessions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(discovered),
        Err(error) => panic!("read session directory: {error}"),
    };

    for entry in entries {
        let entry = entry.expect("session directory entry");
        let path = entry.path().join("session.sqlite");
        if !path.is_file() {
            continue;
        }
        let connection = Connection::open(&path)?;
        let metadata = connection
            .query_row(
                "SELECT id, title, created_at FROM session_meta LIMIT 1",
                [],
                |row| {
                    Ok(DiscoveredSession {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        created_at: row.get(2)?,
                        path: path.clone(),
                    })
                },
            )
            .optional()?;
        if let Some(metadata) = metadata {
            discovered.push(metadata);
        }
    }

    discovered.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(discovered)
}

#[test]
fn one_session_database_keeps_core_transition_atomic() {
    let fixture = StorageFixture::new();
    let path = fixture.create_session("session-a", "A", 1);
    let mut connection = Connection::open(path).expect("reopen session database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("enable foreign keys");

    {
        let transaction = connection.transaction().expect("begin transition");
        transaction
            .execute(
                "INSERT INTO tasks (id, status) VALUES ('task-a', 'running')",
                [],
            )
            .expect("insert task");
        let error = transaction
            .execute(
                "INSERT INTO effects (id, task_id, status) VALUES ('effect-a', 'missing-task', 'open')",
                [],
            )
            .expect_err("foreign-key violation must fail transition");
        assert!(error.to_string().contains("FOREIGN KEY"));
        transaction.rollback().expect("rollback failed transition");
    }

    let task_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("count tasks");
    let effect_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM effects", [], |row| row.get(0))
        .expect("count effects");
    assert_eq!(task_count, 0);
    assert_eq!(effect_count, 0);
}

#[test]
fn independent_session_files_have_independent_writer_locks() {
    let fixture = StorageFixture::new();
    let path_a = fixture.create_session("session-a", "A", 1);
    let path_b = fixture.create_session("session-b", "B", 2);

    let connection_a = Connection::open(&path_a).expect("open session A");
    connection_a
        .busy_timeout(std::time::Duration::ZERO)
        .expect("zero busy timeout A");
    connection_a
        .execute_batch(
            "BEGIN IMMEDIATE; INSERT INTO tasks (id, status) VALUES ('held-a', 'running');",
        )
        .expect("hold writer A");

    let competing_a = Connection::open(&path_a).expect("open competing session A writer");
    competing_a
        .busy_timeout(std::time::Duration::ZERO)
        .expect("zero busy timeout competing A");
    assert!(
        competing_a.execute_batch("BEGIN IMMEDIATE;").is_err(),
        "a second writer to the same session must contend"
    );

    let connection_b = Connection::open(&path_b).expect("open session B");
    connection_b
        .busy_timeout(std::time::Duration::ZERO)
        .expect("zero busy timeout B");
    connection_b
        .execute_batch(
            "BEGIN IMMEDIATE;\n\
             INSERT INTO tasks (id, status) VALUES ('independent-b', 'running');\n\
             COMMIT;",
        )
        .expect("session B must commit while session A writer is held");

    connection_a
        .execute_batch("ROLLBACK;")
        .expect("release session A writer");

    let count_b: i64 = connection_b
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = 'independent-b'",
            [],
            |row| row.get(0),
        )
        .expect("count session B task");
    assert_eq!(count_b, 1);
}

#[test]
fn catalog_can_be_rebuilt_from_authoritative_session_stores() {
    let fixture = StorageFixture::new();
    fixture.create_session("session-b", "Second", 20);
    fixture.create_session("session-a", "First", 10);

    let catalog = Connection::open(fixture.catalog_path()).expect("open initial catalog");
    catalog
        .execute_batch(CATALOG_SCHEMA)
        .expect("create initial catalog");
    catalog
        .execute(
            "INSERT INTO sessions (id, title, created_at, path) VALUES (?1, ?2, ?3, ?4)",
            params![
                "session-a",
                "stale title",
                0_i64,
                fixture.session_path("session-a").to_string_lossy()
            ],
        )
        .expect("insert deliberately stale catalog row");
    drop(catalog);

    let discovered = fixture.rebuild_catalog();
    assert_eq!(
        discovered
            .iter()
            .map(|session| (session.id.as_str(), session.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("session-a", "First"), ("session-b", "Second")]
    );

    let rebuilt = Connection::open(fixture.catalog_path()).expect("open rebuilt catalog");
    let rows = rebuilt
        .prepare("SELECT id, title FROM sessions ORDER BY id")
        .expect("prepare catalog query")
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .expect("query rebuilt catalog")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect rebuilt catalog");
    assert_eq!(
        rows,
        vec![
            ("session-a".to_owned(), "First".to_owned()),
            ("session-b".to_owned(), "Second".to_owned())
        ]
    );
}

#[test]
fn complete_session_store_can_exist_before_catalog_publication() {
    let fixture = StorageFixture::new();
    fixture.create_session("session-a", "Published later", 1);

    assert!(
        !fixture.catalog_path().exists(),
        "session publication must not require the catalog to be authoritative"
    );

    let discovered = discover_sessions(fixture.root()).expect("discover unpublished session");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, "session-a");

    let rebuilt = fixture.rebuild_catalog();
    assert_eq!(rebuilt, discovered);
}

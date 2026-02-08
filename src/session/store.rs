//! Session persistence with `SQLite`.
#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)] // SQLite uses i64 for rowids

use crate::provider::{ContentBlock, Message, Role};
use crate::session::Session;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const SCHEMA_VERSION: i32 = 2;
/// Max input history entries to keep
const INPUT_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),
}

/// Summary of a session for listing purposes.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub working_dir: String,
    pub model: String,
    pub updated_at: i64,
    pub first_user_message: Option<String>,
}

pub struct SessionStore {
    db: Connection,
}

impl SessionStore {
    /// Open or create a session store at the given path.
    pub fn open(path: &Path) -> Result<Self, SessionStoreError> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                SessionStoreError::InvalidData(format!(
                    "Failed to create session directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let db = Connection::open(path)?;

        // Enable WAL mode for better concurrent access and performance
        db.execute_batch("PRAGMA journal_mode=WAL;")?;

        let store = Self { db };
        store.init_schema()?;

        Ok(store)
    }

    fn init_schema(&self) -> Result<(), SessionStoreError> {
        let version: i32 = self
            .db
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        // Migration v0 -> v1: Initial schema
        if version < 1 {
            self.db.execute_batch(
                r"
                CREATE TABLE IF NOT EXISTS sessions (
                    id          TEXT PRIMARY KEY,
                    working_dir TEXT NOT NULL,
                    model       TEXT NOT NULL,
                    created_at  INTEGER NOT NULL,
                    updated_at  INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                    position    INTEGER NOT NULL,
                    role        TEXT NOT NULL,
                    content     TEXT NOT NULL,
                    created_at  INTEGER NOT NULL,
                    UNIQUE(session_id, position)
                );

                CREATE INDEX IF NOT EXISTS idx_messages_session_position
                    ON messages(session_id, position);

                CREATE INDEX IF NOT EXISTS idx_sessions_updated
                    ON sessions(updated_at DESC);

                PRAGMA user_version = 1;
                ",
            )?;
        }

        // Migration v1 -> v2: Add input history table
        if version < SCHEMA_VERSION {
            self.db.execute_batch(
                r"
                CREATE TABLE IF NOT EXISTS input_history (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    content     TEXT NOT NULL,
                    created_at  INTEGER NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_input_history_created
                    ON input_history(created_at DESC);

                PRAGMA user_version = 2;
                ",
            )?;
        }

        // Enable foreign key enforcement
        self.db.execute("PRAGMA foreign_keys = ON", [])?;

        Ok(())
    }

    /// Save a session. Upserts session metadata and appends new messages.
    pub fn save(&self, session: &Session) -> Result<(), SessionStoreError> {
        if !session.messages.iter().any(|m| m.role != Role::System) {
            return Ok(());
        }

        let now = chrono::Utc::now().timestamp();
        let working_dir = session.working_dir.display().to_string();

        // Begin transaction for atomicity
        self.db.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| {
            // Upsert session metadata
            self.db.execute(
                r"
                INSERT INTO sessions (id, working_dir, model, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?4)
                ON CONFLICT(id) DO UPDATE SET
                    working_dir = excluded.working_dir,
                    model = excluded.model,
                    updated_at = excluded.updated_at
                ",
                params![session.id, working_dir, session.model, now],
            )?;

            // Get the current max position for this session
            let max_position: Option<i64> = self.db.query_row(
                "SELECT MAX(position) FROM messages WHERE session_id = ?1",
                params![session.id],
                |row| row.get(0),
            )?;

            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            // SQLite rowid fits in usize
            let start_position = max_position.map_or(0, |p| p + 1) as usize;

            // Only insert messages beyond the last saved position
            for (i, msg) in session.messages.iter().enumerate().skip(start_position) {
                let role_str = role_to_str(msg.role);
                let content_json = serde_json::to_string(&*msg.content)?;

                self.db.execute(
                    r"
                    INSERT INTO messages (session_id, position, role, content, created_at)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    ",
                    params![session.id, i as i64, role_str, content_json, now],
                )?;
            }

            Ok::<(), SessionStoreError>(())
        })();

        match result {
            Ok(()) => {
                self.db.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.db.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Delete sessions without any user messages (empty/aborted runs).
    pub fn prune_empty_sessions(&self) -> Result<usize, SessionStoreError> {
        let deleted = self.db.execute(
            r"
            DELETE FROM sessions
            WHERE id NOT IN (
                SELECT DISTINCT session_id FROM messages WHERE role = 'user'
            )
            ",
            [],
        )?;
        Ok(deleted)
    }

    /// Delete sessions older than the given number of days.
    /// Returns the number of sessions deleted.
    pub fn cleanup_old_sessions(&self, retention_days: u32) -> Result<usize, SessionStoreError> {
        if retention_days == 0 {
            return Ok(0);
        }
        let cutoff = chrono::Utc::now().timestamp() - i64::from(retention_days) * 86400;
        let deleted = self.db.execute(
            "DELETE FROM sessions WHERE updated_at < ?1",
            params![cutoff],
        )?;
        Ok(deleted)
    }

    /// Load a session by ID.
    pub fn load(&self, id: &str) -> Result<Session, SessionStoreError> {
        // Load session metadata
        let (working_dir, model): (String, String) = self
            .db
            .query_row(
                "SELECT working_dir, model FROM sessions WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => SessionStoreError::NotFound(id.to_string()),
                _ => SessionStoreError::Database(e),
            })?;

        // Load messages in order
        let mut stmt = self.db.prepare(
            "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY position",
        )?;

        let messages: Result<Vec<Message>, SessionStoreError> = stmt
            .query_map(params![id], |row| {
                let role_str: String = row.get(0)?;
                let content_json: String = row.get(1)?;
                Ok((role_str, content_json))
            })?
            .map(|r| {
                let (role_str, content_json) = r?;
                let role = str_to_role(&role_str)?;
                let content: Vec<ContentBlock> = serde_json::from_str(&content_json)?;
                Ok(Message {
                    role,
                    content: Arc::new(content),
                })
            })
            .collect();

        Ok(Session {
            id: id.to_string(),
            working_dir: working_dir.into(),
            model,
            messages: messages?,
            abort_token: CancellationToken::new(),
            no_sandbox: false, // Default to sandboxed for restored sessions
        })
    }

    /// List recent sessions, ordered by most recently updated.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<SessionSummary>, SessionStoreError> {
        let mut stmt = self.db.prepare(
            r"
            WITH first_user_messages AS (
                SELECT session_id, content, ROW_NUMBER() OVER (
                    PARTITION BY session_id ORDER BY position
                ) as rn
                FROM messages
                WHERE role = 'user'
            )
            SELECT
                s.id,
                s.working_dir,
                s.model,
                s.updated_at,
                fum.content as first_user_message
            FROM sessions s
            INNER JOIN first_user_messages fum ON fum.session_id = s.id AND fum.rn = 1
            ORDER BY s.updated_at DESC
            LIMIT ?1
            ",
        )?;

        let summaries: Result<Vec<SessionSummary>, SessionStoreError> = stmt
            .query_map(params![limit as i64], |row| {
                let id: String = row.get(0)?;
                let working_dir: String = row.get(1)?;
                let model: String = row.get(2)?;
                let updated_at: i64 = row.get(3)?;
                let first_user_message: Option<String> = row.get(4)?;
                Ok((id, working_dir, model, updated_at, first_user_message))
            })?
            .map(|r| {
                let (id, working_dir, model, updated_at, first_user_message_json) = r?;

                // Extract text from first user message JSON
                let first_user_message =
                    first_user_message_json.and_then(|json| extract_first_text_from_content(&json));

                Ok(SessionSummary {
                    id,
                    working_dir,
                    model,
                    updated_at,
                    first_user_message,
                })
            })
            .collect();

        summaries
    }

    /// Delete a session and all its messages.
    pub fn delete(&self, id: &str) -> Result<(), SessionStoreError> {
        let affected = self
            .db
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;

        if affected == 0 {
            return Err(SessionStoreError::NotFound(id.to_string()));
        }

        Ok(())
    }

    /// Add an input to history.
    pub fn add_input_history(&self, content: &str) -> Result<(), SessionStoreError> {
        let now = chrono::Utc::now().timestamp();

        // Use transaction for atomicity
        self.db.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| {
            // Insert new entry
            self.db.execute(
                "INSERT INTO input_history (content, created_at) VALUES (?1, ?2)",
                params![content, now],
            )?;

            // Prune old entries beyond limit
            self.db.execute(
                r"
                DELETE FROM input_history
                WHERE id NOT IN (
                    SELECT id FROM input_history
                    ORDER BY created_at DESC
                    LIMIT ?1
                )
                ",
                params![INPUT_HISTORY_LIMIT as i64],
            )?;

            Ok::<(), SessionStoreError>(())
        })();

        match result {
            Ok(()) => {
                self.db.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.db.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Load input history (most recent last).
    pub fn load_input_history(&self) -> Result<Vec<String>, SessionStoreError> {
        let mut stmt = self
            .db
            .prepare("SELECT content FROM input_history ORDER BY created_at ASC")?;

        let history: Result<Vec<String>, _> = stmt.query_map([], |row| row.get(0))?.collect();

        Ok(history?)
    }
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::ToolResult => "tool_result",
    }
}

fn str_to_role(s: &str) -> Result<Role, SessionStoreError> {
    match s {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool_result" => Ok(Role::ToolResult),
        _ => Err(SessionStoreError::InvalidData(format!("Unknown role: {s}"))),
    }
}

/// Extract the first text content from a JSON-serialized Vec<ContentBlock>.
fn extract_first_text_from_content(json: &str) -> Option<String> {
    let blocks: Vec<ContentBlock> = serde_json::from_str(json).ok()?;
    for block in blocks {
        if let ContentBlock::Text { text } = block {
            // Truncate for display
            let truncated = if text.chars().count() > 100 {
                format!("{}...", text.chars().take(100).collect::<String>())
            } else {
                text
            };
            return Some(truncated);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_test_session() -> Session {
        let mut session = Session::new("/test/dir".into(), "test-model".to_string());
        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "Hello, world!".to_string(),
            }]),
        });
        session.messages.push(Message {
            role: Role::Assistant,
            content: Arc::new(vec![ContentBlock::Text {
                text: "Hi there!".to_string(),
            }]),
        });
        session
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        let session = make_test_session();
        let id = session.id.clone();

        store.save(&session).unwrap();

        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.messages.len(), 2);

        if let ContentBlock::Text { text } = &loaded.messages[0].content[0] {
            assert_eq!(text, "Hello, world!");
        } else {
            panic!("Expected Text block");
        }
    }

    #[test]
    fn test_incremental_save() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        let mut session = make_test_session();
        let id = session.id.clone();

        // First save
        store.save(&session).unwrap();

        // Add more messages
        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "Follow-up question".to_string(),
            }]),
        });

        // Second save (should only append new message)
        store.save(&session).unwrap();

        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded.messages.len(), 3);
    }

    #[test]
    fn test_list_recent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        // Create multiple sessions
        let mut session_ids = Vec::new();
        for i in 0..5 {
            let mut session = Session::new(format!("/test/dir{}", i).into(), "model".to_string());
            session.messages.push(Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: format!("Session {} message", i),
                }]),
            });
            session_ids.push(session.id.clone());
            store.save(&session).unwrap();
        }

        let recent = store.list_recent(3).unwrap();
        assert_eq!(recent.len(), 3);

        // Verify summaries have proper data
        for summary in &recent {
            assert!(summary.first_user_message.is_some());
            assert!(
                summary
                    .first_user_message
                    .as_ref()
                    .unwrap()
                    .contains("Session")
            );
            assert!(summary.model == "model");
        }

        // All returned sessions should be from our created set
        for summary in &recent {
            assert!(session_ids.contains(&summary.id));
        }
    }

    #[test]
    fn test_delete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        let session = make_test_session();
        let id = session.id.clone();

        store.save(&session).unwrap();
        assert!(store.load(&id).is_ok());

        store.delete(&id).unwrap();
        assert!(matches!(
            store.load(&id),
            Err(SessionStoreError::NotFound(_))
        ));
    }

    #[test]
    fn test_not_found() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        assert!(matches!(
            store.load("nonexistent"),
            Err(SessionStoreError::NotFound(_))
        ));
    }

    #[test]
    fn test_cleanup_old_sessions() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        // Create a session
        let session = make_test_session();
        let id = session.id.clone();
        store.save(&session).unwrap();

        // Cleanup with 90 days — should keep the session (it was just created)
        let deleted = store.cleanup_old_sessions(90).unwrap();
        assert_eq!(deleted, 0);
        assert!(store.load(&id).is_ok());

        // Manually backdate the session to 100 days ago
        let old_ts = chrono::Utc::now().timestamp() - 100 * 86400;
        store
            .db
            .execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![old_ts, id],
            )
            .unwrap();

        // Cleanup with 90 days — should delete the old session
        let deleted = store.cleanup_old_sessions(90).unwrap();
        assert_eq!(deleted, 1);
        assert!(matches!(
            store.load(&id),
            Err(SessionStoreError::NotFound(_))
        ));
    }

    #[test]
    fn test_cleanup_zero_days_noop() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        let session = make_test_session();
        store.save(&session).unwrap();

        // 0 days = disabled
        let deleted = store.cleanup_old_sessions(0).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_complex_content_blocks() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let store = SessionStore::open(&db_path).unwrap();

        let mut session = Session::new("/test".into(), "model".to_string());
        session.messages.push(Message {
            role: Role::Assistant,
            content: Arc::new(vec![
                ContentBlock::Thinking {
                    thinking: "Let me think...".to_string(),
                },
                ContentBlock::Text {
                    text: "Here's my answer".to_string(),
                },
                ContentBlock::ToolCall {
                    id: "call_123".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/foo.txt"}),
                },
            ]),
        });
        session.messages.push(Message {
            role: Role::ToolResult,
            content: Arc::new(vec![ContentBlock::ToolResult {
                tool_call_id: "call_123".to_string(),
                content: "file contents here".to_string(),
                is_error: false,
            }]),
        });

        let id = session.id.clone();
        store.save(&session).unwrap();

        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded.messages.len(), 2);

        // Verify complex content blocks survived round-trip
        if let ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } = &loaded.messages[0].content[2]
        {
            assert_eq!(id, "call_123");
            assert_eq!(name, "read_file");
            assert_eq!(arguments["path"], "/foo.txt");
        } else {
            panic!("Expected ToolCall block");
        }
    }
}

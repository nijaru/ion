# Session Persistence Schema

**Task**: tk-ixus
**Date**: 2026-01-15

## Data Model

### Current Rust Types

```rust
// session.rs
pub struct Session {
    pub id: String,           // UUID
    pub working_dir: PathBuf,
    pub model: String,
    pub messages: Vec<Message>,
    pub abort_token: CancellationToken,  // runtime-only
}

// provider/mod.rs
pub struct Message {
    pub role: Role,  // System, User, Assistant, ToolResult
    pub content: Arc<Vec<ContentBlock>>,
}

pub enum ContentBlock {
    Text { text },
    Thinking { thinking },
    ToolCall { id, name, arguments },
    ToolResult { tool_call_id, content, is_error },
    Image { media_type, data },
}
```

## Schema

```sql
-- Schema version for migrations
PRAGMA user_version = 1;

CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,
    working_dir TEXT NOT NULL,
    model       TEXT NOT NULL,
    created_at  INTEGER NOT NULL,  -- Unix timestamp
    updated_at  INTEGER NOT NULL   -- Unix timestamp
);

CREATE TABLE messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,  -- Order within session (0-indexed)
    role        TEXT NOT NULL,     -- 'system', 'user', 'assistant', 'tool_result'
    content     TEXT NOT NULL,     -- JSON array of ContentBlock
    created_at  INTEGER NOT NULL,  -- Unix timestamp

    UNIQUE(session_id, position)
);

-- Index for loading session messages in order
CREATE INDEX idx_messages_session_position ON messages(session_id, position);

-- Index for listing recent sessions
CREATE INDEX idx_sessions_updated ON sessions(updated_at DESC);
```

## Design Decisions

| Decision                    | Rationale                                                                                                                           |
| --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| **JSON for content**        | ContentBlock already serializes via serde. We always load full messages. Avoids complex joins. Schema-flexible for new block types. |
| **Position not timestamp**  | Messages in same turn share timestamps. Position is deterministic and allows explicit ordering.                                     |
| **Separate messages table** | Enables partial loads (pagination) and efficient appends without rewriting session blob.                                            |
| **No title column**         | Derive from first user message at display time. Avoids sync issues.                                                                 |
| **Unix timestamps**         | SQLite has no native datetime. Integers are sortable and portable.                                                                  |
| **CASCADE DELETE**          | Deleting session removes all messages atomically.                                                                                   |

## Not Stored

| Field         | Reason                              |
| ------------- | ----------------------------------- |
| `abort_token` | Runtime-only (CancellationToken)    |
| System prompt | Global config, not session-specific |
| Usage stats   | Can add later if needed             |

## Operations

### Save Session

```sql
-- Upsert session metadata
INSERT INTO sessions (id, working_dir, model, created_at, updated_at)
VALUES (?, ?, ?, ?, ?)
ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at;

-- Append new messages (only those after last saved position)
INSERT INTO messages (session_id, position, role, content, created_at)
VALUES (?, ?, ?, ?, ?);
```

### Load Session

```sql
SELECT id, working_dir, model, created_at, updated_at FROM sessions WHERE id = ?;

SELECT role, content FROM messages
WHERE session_id = ?
ORDER BY position;
```

### List Recent Sessions

```sql
SELECT s.id, s.working_dir, s.model, s.updated_at,
       (SELECT content FROM messages m
        WHERE m.session_id = s.id AND m.role = 'user'
        ORDER BY m.position LIMIT 1) as first_user_message
FROM sessions s
ORDER BY s.updated_at DESC
LIMIT ?;
```

## File Location

```
~/.local/share/ion/sessions.db
```

Single database file for all sessions (not one file per session). Simpler to manage, query across sessions, and backup.

## Migration Strategy

Use `PRAGMA user_version` to track schema version. On open:

1. Check `user_version`
2. Run migrations for versions < current
3. Set `user_version` to current

## Implementation

New module: `src/session/store.rs`

```rust
pub struct SessionStore {
    db: rusqlite::Connection,
}

impl SessionStore {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn save(&self, session: &Session) -> Result<()>;
    pub fn load(&self, id: &str) -> Result<Session>;
    pub fn list_recent(&self, limit: usize) -> Result<Vec<SessionSummary>>;
    pub fn delete(&self, id: &str) -> Result<()>;
}
```

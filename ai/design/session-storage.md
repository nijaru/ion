# Session Storage Design

## Overview

JSONL-based session storage with per-directory organization and SQLite indexes.

## Directory Structure

```
~/.ion/
  sessions/
    -home-nick-projects-ion/          # Encoded cwd (slashes → dashes)
      input.db                         # Per-directory input history
      index.db                         # Session metadata for picker
      1706000000-a7b3.jsonl           # Session files
      1706100000-c9d5.jsonl
    -home-nick-other-project/
      input.db
      index.db
      1706050000-d2e6.jsonl
```

## Path Encoding

Convert `/home/nick/projects/ion` → `-home-nick-projects-ion`

```rust
fn encode_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "-")
        .replace('\\', "-")  // Windows
}

fn decode_path(encoded: &str) -> PathBuf {
    // First char is always "-" (from leading /)
    PathBuf::from(encoded.replacen('-', "/", 1).replace('-', "/"))
}
```

## Session File Format (JSONL)

Each line is a typed JSON object. Append-only.

```jsonl
{"type":"meta","id":"1706000000-a7b3","cwd":"/home/nick/ion","model":"claude-sonnet-4-20250514","branch":"main","created_at":1706000000}
{"type":"user","content":"Add authentication to the API","ts":1706000001}
{"type":"assistant","content":[{"type":"text","text":"I'll add auth..."}],"ts":1706000002}
{"type":"tool_use","id":"call_123","name":"read","input":{"file_path":"/src/api.rs"},"ts":1706000003}
{"type":"tool_result","tool_use_id":"call_123","content":"pub fn handler()...","is_error":false,"ts":1706000004}
{"type":"assistant","content":[{"type":"text","text":"Now editing..."}],"ts":1706000005}
{"type":"tool_use","id":"call_124","name":"edit","input":{"file_path":"/src/api.rs","old_string":"...","new_string":"..."},"ts":1706000006}
{"type":"tool_result","tool_use_id":"call_124","content":"OK","is_error":false,"ts":1706000007}
```

### Event Types

| Type          | Fields                             | Notes                 |
| ------------- | ---------------------------------- | --------------------- |
| `meta`        | id, cwd, model, branch, created_at | First line, required  |
| `user`        | content (string), ts               | User message          |
| `assistant`   | content (array of blocks), ts      | Model response        |
| `tool_use`    | id, name, input, ts                | Tool call             |
| `tool_result` | tool_use_id, content, is_error, ts | Tool response         |
| `system`      | content, ts                        | System message (rare) |

### Content Block Types (in assistant content array)

```json
{"type":"text","text":"..."}
{"type":"thinking","thinking":"..."}
```

## Session Naming

Format: `{unix_timestamp}-{short_id}.jsonl`

- `unix_timestamp`: Seconds since epoch (session creation time)
- `short_id`: First 8 chars of UUID v4 (hex, lowercase)

Example: `1706000000-a7b3c4d5.jsonl`

Benefits:

- Sortable by creation time (filesystem `ls` works)
- Unique (timestamp + random)
- Short enough to read
- No ambiguity

## Index Schema (index.db)

Per-directory SQLite database for fast picker queries.

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,              -- "1706000000-a7b3c4d5"
    file_name TEXT NOT NULL,          -- "1706000000-a7b3c4d5.jsonl"
    model TEXT,
    branch TEXT,                      -- Git branch at session start
    name TEXT,                        -- User-given name (future: rename)
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,      -- Last message timestamp
    message_count INTEGER NOT NULL,
    last_preview TEXT                 -- Truncated last user message
);

CREATE INDEX idx_sessions_updated ON sessions(updated_at DESC);
```

### Index Updates

On session save:

1. Append new events to JSONL
2. Upsert index row with updated `updated_at`, `message_count`, `last_preview`

On index corruption:

1. Delete index.db
2. Rebuild by scanning all JSONL files (read meta + count lines + last user line)

## Input History Schema (input.db)

Per-directory SQLite database.

```sql
CREATE TABLE inputs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_inputs_created ON inputs(created_at DESC);
```

Limit: 100 entries per directory (prune oldest on insert).

## Resume Picker

Query: `SELECT * FROM sessions ORDER BY updated_at DESC`

Display format:

```
❯ Add authentication to the API...        (+3 other sessions)
  35 seconds ago · 41 messages · main
```

Fields:

- Preview: `last_preview` (truncated to ~60 chars)
- Time: Relative time from `updated_at`
- Count: `message_count`
- Branch: `branch` (if available)

## CLI Flags

| Flag            | Behavior                                        |
| --------------- | ----------------------------------------------- |
| `--continue`    | Load most recent session from current directory |
| `--resume`      | Open picker for current directory               |
| `--resume <id>` | Load specific session by ID                     |

## Migration

From current SQLite (`~/.ion/data/sessions.db`):

1. For each session in old DB:
   - Create directory for `working_dir`
   - Write JSONL file with meta + messages
   - Update index.db
2. Migrate input_history to per-directory input.db (by working_dir if available, else global)
3. Keep old DB as backup for 30 days

## Future Considerations

- **Session naming**: `name` field in schema, Ctrl+R in picker
- **Git branch in status line**: Read from session meta or live `git branch`
- **Cross-directory search**: Global index or scan all directories
- **Session export**: `ion export <id>` → stdout or file
- **Session import**: `ion import <file>` → copy + index
- **Session sharing**: Generate shareable format (sanitize paths?)
- **Compression**: `.jsonl.gz` for old sessions

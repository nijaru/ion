# Tool Pass Plan

## Overview

Two tasks: bash improvements (tk-d7jh) and grep enhancements (tk-rxsz).

## Task 1: Bash — directory param + read-mode safe commands (tk-d7jh)

### 1a. Add `directory` parameter

**File:** `src/tool/builtin/bash.rs`

Add optional `directory` param to schema (lines 34-44):

```json
"directory": {
    "type": "string",
    "description": "Working directory for this command (default: project root)"
}
```

In `execute()` (line 86), resolve and validate:

```rust
let dir = args.get("directory")
    .and_then(|v| v.as_str())
    .map(|d| ctx.working_dir.join(d))
    .unwrap_or_else(|| ctx.working_dir.clone());
let dir = ctx.check_sandbox(&dir).map_err(ToolError::PermissionDenied)?;
// ...
.current_dir(&dir)
```

**Display:** `message_list.rs` `extract_key_arg` should show directory when present:
`• bash(cargo test, dir=backend/)`

### 1b. Read-mode safe commands

**File:** `src/tool/permissions.rs`

Change `check_command_permission` read-mode branch (line 60-62) from blanket deny to safe-command check:

```rust
ToolMode::Read => {
    if is_safe_command(command) {
        PermissionStatus::Allowed
    } else {
        PermissionStatus::Denied("Command has side effects; blocked in Read mode".into())
    }
}
```

**File:** `src/tool/builtin/guard.rs`

Add `is_safe_command()` function. Allowlist approach — match first token:

```rust
const SAFE_COMMANDS: &[&str] = &[
    // Navigation/listing
    "ls", "find", "tree", "file", "stat", "du", "df", "wc",
    // Reading
    "cat", "head", "tail", "less", "bat",
    // Search
    "grep", "rg", "ag", "fd", "fzf",
    // Git (read-only)
    "git status", "git log", "git diff", "git show", "git branch",
    "git tag", "git remote", "git rev-parse", "git describe",
    // Build info
    "cargo --version", "rustc --version", "node --version",
    "python --version", "go version",
    // Project tools (read-only)
    "cargo check", "cargo clippy", "cargo test", "cargo bench",
    "npm test", "pytest", "go test",
    // Task tracking
    "tk",
    // System info
    "uname", "whoami", "hostname", "date", "env", "printenv", "which", "type",
];
```

Parse first token(s) from command, check against allowlist. Pipe chains: check each segment. `&&`/`||` chains: check each segment. If ANY segment is unsafe, deny the whole command.

**Tests:** Add test cases for safe commands in read mode, unsafe commands blocked, pipe/chain handling.

## Task 2: Grep — context lines + output modes (tk-rxsz)

### 2a. Add parameters

**File:** `src/tool/builtin/grep.rs`

Extend schema (lines 27-41):

```json
"context_before": {
    "type": "integer",
    "description": "Lines of context before each match (like grep -B)"
},
"context_after": {
    "type": "integer",
    "description": "Lines of context after each match (like grep -A)"
},
"output_mode": {
    "type": "string",
    "enum": ["content", "files", "count"],
    "description": "Output format: content (default, matching lines), files (file paths only), count (match counts per file)"
}
```

### 2b. Implement context lines

**File:** `src/tool/builtin/grep.rs`

`grep-searcher` supports context natively via `SearcherBuilder`:

```rust
let mut builder = SearcherBuilder::new();
if context_before > 0 {
    builder.before_context(context_before);
}
if context_after > 0 {
    builder.after_context(context_after);
}
let mut searcher = builder.build();
```

Context lines come through the sink as `ContextKind::Before`/`After`. Need to handle the `--` separator between match groups.

### 2c. Implement output modes

In `search_with_grep`, branch on mode:

- **content** (default): Current behavior — `file:line: text`
- **files**: Collect unique file paths only, stop searching file after first match
- **count**: Collect `(file_path, count)` pairs, format as `file: N matches`

For **files** mode, use `file_count.fetch_add(1)` per file (not per match) for the limit.

### 2d. Update display

**File:** `src/tui/message_list.rs`

Grep with `output_mode=files` should show "N files" not "N matches". Check metadata or tool name context.

## Order of Implementation

1. Bash `directory` param (small, isolated change)
2. Read-mode safe commands (guard.rs + permissions.rs)
3. Grep `output_mode` (schema + branching)
4. Grep context lines (searcher config + sink handling)

## Files to Modify

| File                        | Changes                             |
| --------------------------- | ----------------------------------- |
| `src/tool/builtin/bash.rs`  | Add `directory` param, resolve path |
| `src/tool/builtin/guard.rs` | Add `is_safe_command()`             |
| `src/tool/permissions.rs`   | Read mode: safe command check       |
| `src/tool/builtin/grep.rs`  | Context lines, output modes         |
| `src/tui/message_list.rs`   | Display: bash dir, grep mode units  |

## Verification

```bash
cargo test --lib
cargo clippy
```

Manual: test bash with directory param, test read mode allows safe commands, test grep context/modes.

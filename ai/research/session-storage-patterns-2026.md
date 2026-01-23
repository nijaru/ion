# CLI Coding Agent Session Storage Patterns

Research on how major CLI coding agents handle conversation persistence.

## Summary Table

| Agent       | Format        | Location                           | Naming                      | Resume                        | Export                   |
| ----------- | ------------- | ---------------------------------- | --------------------------- | ----------------------------- | ------------------------ |
| Claude Code | JSONL         | `~/.claude/projects/{path}/`       | `{uuid}.jsonl`              | `--continue`, `--resume <id>` | Third-party tools        |
| Codex CLI   | JSONL         | `~/.codex/sessions/`               | `rollout-{ts}-{uuid}.jsonl` | `codex resume`                | Built-in JSON            |
| Gemini CLI  | JSON          | `~/.gemini/tmp/{hash}/chats/`      | `checkpoint-{tag}.json`     | `--resume`                    | `/chat share`, `/export` |
| OpenCode    | JSON          | `~/.local/share/opencode/storage/` | `{sessionID}.json`          | Built-in                      | Session sharing          |
| Amp         | Server-synced | ampcode.com                        | Thread ID                   | `amp threads continue`        | URL sharing              |
| Aider       | Markdown      | `.aider.chat.history.md`           | Per-project file            | `--restore-chat-history`      | Copy markdown            |

## Claude Code (Anthropic)

### Storage Structure

```
~/.claude/
  history.jsonl              # Index of all sessions (metadata)
  projects/
    -home-user-myproject/    # Path encoded (/ -> -)
      {uuid}.jsonl           # Full conversation
      agent-{uuid}.jsonl     # Agent subprocesses
  session-env/               # Environment snapshots
  todos/                     # Todo lists per session
```

### File Format

JSONL with message types:

```json
{"type":"summary","summary":"Project Development","leafUuid":"..."}
{"type":"user","message":{"role":"user","content":"..."},"uuid":"...","timestamp":"..."}
{"type":"assistant","message":{"role":"assistant","content":[...]}}
```

Content blocks include:

- `text` - Plain text responses
- `thinking` - Extended thinking (when enabled)
- `tool_use` - Tool invocations with `id`, `name`, `input`
- `tool_result` - Tool outputs with `tool_use_id`, `content`

### Resume

- `claude --continue` - Most recent session
- `claude --resume <id>` - Specific session by ID
- Sessions listed via `claude --resume` (interactive picker)

### Export

No built-in export. Third-party tools exist:

- [claude-conversation-extractor](https://github.com/ZeroSumQuant/claude-conversation-extractor)
- [claude-code-log](https://github.com/daaain/claude-code-log)

### Sources

- [Claude Code Conversation History](https://kentgigger.com/posts/claude-code-conversation-history)
- [Migrate Sessions](https://www.vincentschmalbach.com/migrate-claude-code-sessions-to-a-new-computer/)

---

## OpenAI Codex CLI

### Storage Structure

```
~/.codex/
  sessions/
    YYYY/MM/DD/
      rollout-{timestamp}-{uuid}.jsonl
  archived_sessions/
  history.jsonl              # Optional global log
```

### File Format

JSONL "rollout" files with typed events:

```json
{"type":"session_meta","id":"...","model":"...","timestamp":"..."}
{"type":"user_turn","content":"..."}
{"type":"assistant_turn","content":"...","reasoning":"..."}
{"type":"tool_call","name":"...","input":"..."}
{"type":"tool_result","output":"..."}
{"type":"turn_complete","token_usage":{...}}
```

### Resume

- `codex resume` - Interactive picker
- `codex resume --last` - Most recent
- `codex resume <uuid>` - Specific session

### Features

- Session forking at specific turns
- Automatic compaction when file exceeds `history.max_bytes`
- Token usage tracking per turn

### Sources

- [Codex Session Management](https://deepwiki.com/openai/codex/3.3-session-management-and-persistence)
- [Rollout Items PR](https://github.com/openai/codex/pull/3380)

---

## Google Gemini CLI

### Storage Structure

```
~/.gemini/
  tmp/{project_hash}/
    chats/
      checkpoint-{tag}.json
    checkpoints/
      {timestamp}-{filename}-{tool}.json
  history/{project_hash}/    # Shadow git repo for snapshots
```

### File Format

JSON with conversation array:

```json
{
  "messages": [
    {"role": "user", "parts": [{"text": "..."}]},
    {"role": "model", "parts": [{"text": "..."}]}
  ],
  "tool_calls": [...],
  "token_usage": {"input": N, "output": N, "cached": N},
  "thoughts": [...]
}
```

### Resume

- `gemini --resume` - Resume last session
- `/chat save <tag>` - Save checkpoint
- `/chat load <tag>` - Load checkpoint

### Export

- `/chat share file.md` or `/chat share file.json`
- `/export markdown` or `/export jsonl`

### Features

- Project-specific sessions (by directory)
- Shadow git repository for file snapshots
- Checkpoint includes git state + conversation

### Sources

- [Gemini CLI Checkpointing](https://google-gemini.github.io/gemini-cli/docs/cli/checkpointing.html)
- [Session Management](https://geminicli.com/docs/cli/session-management/)

---

## OpenCode (SST)

### Storage Structure

```
~/.local/share/opencode/
  storage/
    session/{projectHash}/
      {sessionID}.json
    message/{sessionID}/
      msg_{messageID}.json
    part/{messageID}/
      {partID}.json
    share/{sessionID}.json
    session_diff/{sessionID}.json
```

### File Format

JSON with hierarchical storage keys:

```json
// Session
{
  "id": "ses_abc123",
  "projectID": "...",
  "directory": "/path/to/project",
  "parentID": null,
  "title": "Feature implementation",
  "version": "0.5.0",
  "time": {"created": "...", "updated": "...", "compacting": null, "archived": null},
  "summary": {"additions": N, "deletions": N, "files": N},
  "summaryMessageID": "msg_xyz"
}
```

### Resume

- Built-in session picker
- Session summarization for context compaction
- Parent/child session relationships

### Features

- SQLite option for storage
- Atomic updates with file locking
- Session sharing via public URLs
- No automatic cleanup (manual recommended)

### Sources

- [OpenCode Session Management](https://deepwiki.com/sst/opencode/2.1-session-management)
- [Session Sharing](https://deepwiki.com/sst/opencode/6.11-session-sharing)

---

## Amp (Sourcegraph)

### Storage

- **Primary**: Server-synced to ampcode.com
- **Local config**: `~/.config/amp/settings.json`
- **OAuth tokens**: `~/.amp/oauth/`
- **Custom data dir**: `AMP_DATA_HOME` env var

### Thread Model

Threads are cloud-first:

- Sync across devices via ampcode.com/threads
- Visibility: public, unlisted, workspace, group
- Searchable by keyword, file, repo, author, date

### Resume

- `amp threads continue [threadId]`
- `amp threads list`
- `amp threads fork [threadId]`

### Export

- Share via URL
- Thread reference in code reviews
- No local file export

### Limitations

- No local-first storage option
- Free tier uses data for model training
- Privacy concerns for proprietary code

### Sources

- [Amp Manual](https://ampcode.com/manual)
- [Amp Security](https://ampcode.com/security)

---

## Aider

### Storage Structure

```
{project}/
  .aider.chat.history.md     # Conversation log (markdown)
  .aider.input.history       # Command history (plain text)
```

Or global with `AIDER_CHAT_HISTORY_FILE`.

### File Format

Markdown with level-4 headings:

```markdown
#### user

Implement the login feature

#### assistant

I'll help you implement that...

#### user

/add src/auth.py
```

### Resume

- `--restore-chat-history` - Reload previous conversation
- Summarizes if history exceeds token limit
- Does NOT auto-restore on restart

### Features

- Git-based context (repo map)
- Per-project or global history
- Token limit: 8k for chat history
- `/clear` to reset context

### Limitations

- No structured format (just markdown)
- Large history files cause performance issues
- No built-in session management

### Sources

- [Aider Options](https://aider.chat/docs/config/options.html)
- [Chat History Issues](https://github.com/Aider-AI/aider/issues/2684)

---

## Recommendations for Ion

### Storage Format

**JSONL** is the clear winner:

- Used by Claude Code, Codex CLI
- Append-only, crash-safe
- Line-by-line streaming reads
- Easy to filter/transform

### Message Schema

Adopt typed events similar to Codex:

```json
{"type":"session_start","id":"...","model":"...","timestamp":"...","cwd":"..."}
{"type":"user","content":"...","timestamp":"..."}
{"type":"assistant","content":[...],"token_usage":{...}}
{"type":"tool_use","id":"...","name":"...","input":{...}}
{"type":"tool_result","tool_use_id":"...","content":"...","is_error":false}
{"type":"session_end","summary":"..."}
```

### Directory Structure

```
~/.ion/
  sessions/
    {project_hash}/
      {uuid}.jsonl           # One file per session
  index.jsonl                # Session metadata for fast listing
```

### Resume Features

- `ion --continue` - Most recent in current project
- `ion --resume <id>` - Specific session
- `ion sessions` - Interactive picker

### Export

- `ion export <id> --format md|json|jsonl`
- `/share` command for markdown output

### Key Differentiators

1. **Local-first** (unlike Amp)
2. **Structured format** (unlike Aider)
3. **Project-scoped** (like Claude Code, Gemini)
4. **Built-in export** (unlike Claude Code)

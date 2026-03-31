# Codebase Review: GLM-5.1 (2026-03-28)

Independent review of ion by GLM-5.1. Covers concurrency, storage, security, and lifecycle.

## ERROR

### E1. Storage writes block the Bubble Tea Update loop
**Files:** `internal/app/events.go:206,224,303,322,354,382,477`  
**Description:** Every `m.storage.Append(context.Background(), ...)` call in `handleSessionEvent` runs synchronously on the Bubble Tea goroutine. `handleSessionEvent` is called from `Update`, which Bubble Tea runs on its main goroutine. If SQLite is busy (lock contention, disk I/O, WAL checkpoint), the TUI freezes entirely — no renders, no key processing, no spinner ticks.  
**Impact:** User-visible UI stalls during storage contention. On slow disks or with concurrent store access, this could hang for seconds.  
**Fix:** Move storage writes to async tea.Cmds. Emit a Cmd that does the write in a goroutine and returns a result Msg. Only the in-memory model state needs to update synchronously.

### E2. translateEvents loses final events on turn completion
**File:** `internal/backend/canto/backend.go:433-459,461-575`  
**Description:** `SubmitTurn` starts two goroutines: (1) `translateEvents` reading from `evCh` with `select { case <-ctx.Done(): return; case ev := <-evCh: ... }`, and (2) `SendStream` with `defer cancel()`. When `SendStream` completes, it calls `cancel()` which closes `turnCtx.Done()`. If `evCh` still has pending events (ToolCompleted, TurnCompleted), Go's `select` non-deterministically picks between the ready cases. There is a ~50% chance the context-done branch wins and the final events are dropped.  
**Impact:** Lost ToolCompleted/TurnCompleted events mean the TUI never transitions out of the "Working..." state for some turns, requiring manual cancellation.  
**Fix:** Don't tie translateEvents' lifetime to the turn context. Use a separate signal: have translateEvents drain evCh until the channel closes (runner closes it), and use turnCtx only for the SendStream call. Or add a small sleep/rendezvous before cancel() to let translateEvents drain.

### E3. Tool file operations have no path traversal protection
**Files:** `internal/backend/canto/tools/file.go:63,119,175,268,347`  
**Description:** `filepath.Join(r.cwd, input.FilePath)` does NOT reject `..` components. A request with `file_path: "../../etc/shadow"` resolves outside the workspace. This applies to Read, Write, Edit, MultiEdit, and List tools.  
**Impact:** An LLM agent can read or write arbitrary files on the system. In WRITE mode with auto-approve for read tools, a model could exfiltrate secrets without user approval.  
**Fix:** After `filepath.Join`, resolve with `filepath.Abs` and verify the result starts with the workspace root. Reject paths that escape. Example:
```go
abs, _ := filepath.Abs(filepath.Join(cwd, input.FilePath))
if !strings.HasPrefix(abs, cwd+string(filepath.Separator)) && abs != cwd {
    return "", fmt.Errorf("path escapes workspace: %s", input.FilePath)
}
```

### E4. ACP ReadTextFile/WriteTextFile have no path restriction
**File:** `internal/backend/acp/session.go:348-368`  
**Description:** `os.ReadFile(p.Path)` and `os.WriteFile(p.Path, ...)` use the raw path from the ACP request with no validation. An external agent process can read or write any file on the host.  
**Impact:** A compromised or misbehaving ACP agent (e.g., claude CLI) has unrestricted filesystem access.  
**Fix:** Validate that the requested path is within the session's CWD before reading/writing.

### E5. ACP terminal processes orphaned on session close
**File:** `internal/backend/acp/session.go:142-149,410-414`  
**Description:** `Session.Close()` cancels the context and closes the events channel, but never iterates `s.terminals` to kill running processes. Any terminal created via `CreateTerminal` that hasn't been explicitly released keeps running as an orphan.  
**Impact:** Zombie processes accumulate if the session ends while terminals are running.  
**Fix:** In `Close()`, iterate `s.terminals`, kill each process, and clear the map. Add the cleanup inside `closeOnce.Do`.

### E6. ApprovalManager leaks channels for cancelled requests
**File:** `internal/backend/canto/tools/approver.go:25-30`  
**Description:** `Request(id)` creates a buffered channel and stores it in the map. If the turn context is cancelled while waiting for approval (backend.go:264), the hook returns without calling `Approve`. The channel and map entry are never cleaned up. Over multiple cancelled turns, the `requests` map grows indefinitely.  
**Impact:** Memory leak in long-running sessions with frequent cancellations.  
**Fix:** Add a `Cleanup(id)` method or use a finalizer. Better: in the hook's `<-ctx.Done()` path, delete the map entry. Or use a context-aware pattern where the channel is garbage-collected when both sender and receiver give up.

## WARN

### W1. cantoStore shares one SQLite file across three connections without coordinated pragmas
**File:** `internal/storage/canto_store.go:32-45`  
**Description:** `session.NewSQLiteStore(dbPath)`, `memory.NewCoreStore(dbPath)`, and `sql.Open("sqlite", dbPath+"?_pragma=...")` all open the same `sessions.db`. Only the third connection sets `busy_timeout`. The other two may use default SQLite settings (no busy timeout), meaning concurrent writes could immediately fail with "database is locked" rather than retrying.  
**Fix:** Ensure all three connections use WAL mode and busy_timeout. Pass the pragma through each constructor or configure globally.

### W2. cantoStore has no Close — SQLite connections leak
**File:** `internal/storage/canto_store.go`  
**Description:** `cantoStore` holds `*sql.DB`, `*session.SQLiteStore`, and `*memory.CoreStore` but has no `Close()` method. These are never cleaned up.  
**Fix:** Add a `Close()` method that closes the db, canto store, and memory store. Call it on shutdown.

### W3. fileStore accumulates SQLite connections without cleanup
**File:** `internal/storage/file_store.go:24-27,247-318`  
**Description:** `openIndexDB` and `openInputDB` create a new SQLite connection per directory and cache it in the `dbs`/`inputs` maps. There is no eviction or Close. Long-lived ion sessions with many working directories accumulate open file descriptors.  
**Fix:** Add a Close method. Consider LRU eviction for rarely-used connections.

### W4. cantoSession.toolNameForUseID loads entire session history per ToolResult
**File:** `internal/storage/canto_store.go:368-393`  
**Description:** On every ToolResult append, `toolNameForUseID` calls `s.store.canto.Load(ctx, s.id)` which loads all events, then does a reverse linear scan. For a session with N events, this is O(N) per tool result, making it O(N^2) total.  
**Fix:** Cache tool names in a local map within the session. Or maintain a side index in SQLite. (Already tracked as `tk-oyzb`.)

### W5. cantoSession.Append calls UpdateSession on every event
**File:** `internal/storage/canto_store.go:365`  
**Description:** After every persisted event (User, Agent, ToolUse, ToolResult, Status, TokenUsage), `s.store.UpdateSession(ctx, ...)` runs a SQL UPDATE. Token usage events fire frequently during streaming.  
**Fix:** Batch the updated_at/preview updates. Only write on User/Agent events (meaningful preview changes). Skip for TokenUsage and Status.

### W6. Bash tool has no output size limit
**File:** `internal/backend/canto/tools/bash.go:61-101`  
**Description:** `var output strings.Builder` grows unbounded. A command producing massive output (`cat /dev/urandom`, `find /`) causes OOM. The policy engine doesn't restrict arguments to safe commands.  
**Fix:** Add a max output size (e.g., 1MB). After the limit, truncate and append a marker. Check output length in the pipe reader loop.

### W7. Bash tool ignores stdout/stderr pipe creation errors
**File:** `internal/backend/canto/tools/bash.go:54-55`  
**Description:** `cmd.StdoutPipe()` and `cmd.StderrPipe()` errors are discarded with `_`. If pipe creation fails, the command runs but output is silently lost.  
**Fix:** Check and return errors from pipe creation before starting the command.

### W8. IsSafeBashCommand bypasses via safe-listed commands
**File:** `internal/backend/bash_policy.go`  
**Description:** Commands in the safe list can be used with arbitrary arguments. `cat /etc/shadow`, `printenv SECRET_KEY`, `find / -name "*.pem"` all pass the safety check. The safe-prefix approach doesn't restrict arguments.  
**Fix:** For the read-only safe list, also validate arguments don't escape the workspace. Or remove `cat`/`printenv` from the safe list and require approval for them in READ mode too.

### W9. IsSafeBashCommand doesn't handle quoted strings
**File:** `internal/backend/bash_policy.go:124-138`  
**Description:** `splitCommandChain` splits on `&&`, `||`, `;`, `|` naively without handling quoted delimiters. A command like `echo "hello; rm -rf /"` would be split at the semicolon inside quotes. However, the subshell check (`$(`, backticks) catches some injection vectors, and the safe-prefix check on the resulting segments provides a second layer.  
**Fix:** Use a proper shell tokenizer (e.g., `shellescape`) or at minimum skip delimiters inside quotes.

### W10. FileTagProcessor reads arbitrary files via @file tags
**File:** `internal/backend/canto/processors.go:49-58`  
**Description:** The `@file` resolution reads any path without workspace restriction. An agent sending `@/etc/shadow` in a message would have that file's contents included in the LLM prompt.  
**Fix:** Restrict @file resolution to paths within the workspace CWD.

### W11. KillTerminalCommand only sends SIGINT
**File:** `internal/backend/acp/session.go:477`  
**Description:** Only `os.Interrupt` (SIGINT) is sent. Some processes ignore SIGINT. There's no escalation to SIGKILL after a timeout.  
**Fix:** After SIGINT, start a timer. If the process hasn't exited after N seconds, send SIGKILL.

### W12. MultiEdit is not atomic despite doc claim
**File:** `internal/backend/canto/tools/file.go:222,298-303`  
**Description:** The spec says "atomic operation" but files are written one at a time in the second pass. If a write fails partway, earlier files are already modified with no rollback.  
**Fix:** Either remove the "atomic" claim, or write to temp files first and rename all at once.

### W13. Verify tool captures all output in memory
**File:** `internal/backend/canto/tools/verify.go:46`  
**Description:** `cmd.CombinedOutput()` buffers all output. A command producing massive output causes OOM.  
**Fix:** Stream output with a size cap, same as recommended for the Bash tool.

## NIT

### N1. fileStore.Append: index update errors silently swallowed
**File:** `internal/storage/file_store.go:370-372`  
**Description:** `s.store.updateIndex()` errors are ignored (not even logged). The index can silently desync from the JSONL file.  
**Fix:** Log the error at minimum. Consider returning it to the caller.

### N2. cantoSession.Close is a no-op
**File:** `internal/storage/canto_store.go:489-491`  
**Description:** Returns nil without cleanup. No resources are released.  
**Fix:** Close any open handles if applicable.

### N3. handlePickerKey/handleSessionPickerKey use *Model receiver inconsistently
**Files:** `internal/app/commands.go:317`, `internal/app/session_picker.go`  
**Description:** These methods use pointer receivers while handleKey uses a value receiver. Go auto-takes the address, which works but is inconsistent with the rest of the codebase and makes the value/pointer semantics harder to reason about.  
**Fix:** Standardize on value receivers for all methods that return (Model, tea.Cmd), matching Bubble Tea conventions.

### N4. shortID duplicated between storage and session packages
**Files:** `internal/storage/file_store.go:502-508`, `internal/session/util.go:9-14`  
**Description:** Two identical `shortID`/`ShortID` functions exist in different packages.  
**Fix:** Use the one in `session` package everywhere, or extract to a shared utility.

### N5. UnconfiguredSession events channel has capacity 1, can drop errors
**File:** `internal/backend/unconfigured.go:97-100`  
**Description:** `select { case s.events <- ...: default: }` silently drops the error if the channel is full (already has one pending error).  
**Fix:** Increase channel capacity or block (this is a rarely-hit edge case).

### N6. Scanner.hashFile reads entire files into memory for hashing
**File:** `internal/storage/scanner.go:96-108`  
**Description:** Uses `io.Copy(h, f)` which is fine (streams), but the Scanner.Scan itself loads all FileInfo entries into memory. For huge workspaces, this could be significant.  
**Fix:** Stream results via a callback or channel instead of accumulating a slice.

## Summary

| Severity | Count | Key Themes |
|----------|-------|------------|
| ERROR    | 6     | Blocking storage in UI loop, lost events on turn end, path traversal in tools and ACP, orphaned processes, approval channel leaks |
| WARN     | 13    | SQLite connection management, unbounded output, safe command bypasses, non-atomic multi-edit, missing SIGKILL escalation |
| NIT      | 6     | Swallowed errors, duplicated code, inconsistent receivers |

**Highest-priority fixes:** E1 (storage blocks UI), E3+E4 (path traversal), E2 (lost events). These affect correctness and security. The WARN items around SQLite connection lifecycle (W1-W3) should be addressed next to prevent long-running session degradation.

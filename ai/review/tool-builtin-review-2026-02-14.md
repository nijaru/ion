# Built-in Tool Review

Date: 2026-02-14
Scope: All files in `src/tool/builtin/`, `src/tool/types.rs`, `src/tool/mod.rs`, `src/tool/permissions.rs`

## Summary

38/38 tests pass. Overall the tool implementations are solid -- good schemas, proper sandbox checks on file tools, streaming with size limits, cancellation support. Three findings are notable: a security gap in Read-mode safe command checking, a truncation bug in web_fetch, and missing result limits in the list tool.

---

## Critical

### [ERROR] guard.rs - `env` in safe prefix list enables arbitrary command execution in Read mode

**File:** `src/tool/builtin/guard.rs:188`

`env` is listed as a safe command prefix. Since `is_safe_command` only checks the first word of each pipe/chain segment, `env rm -rf /` or `env bash -c "malicious"` will pass the safe check and be allowed in Read mode.

Similarly, `echo evil > /tmp/hacked` passes because shell redirections (`>`, `>>`) are not split operators -- the entire string is treated as one segment starting with `echo`.

```
Bypasses confirmed:
  "env rm -rf /"           -> safe=true
  "env bash -c evil"       -> safe=true
  "echo evil > /tmp/file"  -> safe=true
  "echo $(rm -rf /)"       -> safe=true (subshell)
  "cat /dev/urandom > big" -> safe=true
```

**Fix:** Remove `env` from `SAFE_PREFIXES`. For `echo`/`cat` with redirections, add a check: if any segment contains `>` or `>>` (outside quotes), reject it. Or strip redirections and check the remaining command.

```rust
// Minimal fix for env:
// Remove "env" from SAFE_PREFIXES
// Replace with "printenv" (already listed) for reading env vars

// For redirections, add to is_safe_command:
if segment.contains('>') { return false; }
```

---

## Important

### [WARN] web_fetch.rs:264 - Truncation uses char count instead of byte index

**File:** `src/tool/builtin/web_fetch.rs:264`

```rust
let truncate_at = processed_text
    .char_indices()
    .take_while(|(i, _)| *i < max_length)
    .last()
    .map_or(processed_text.len(), |(i, c)| i + c.len_utf8());
let truncated_text: String = processed_text.chars().take(truncate_at).collect();
```

`truncate_at` is a **byte offset** (e.g., 50000), but `.chars().take(truncate_at)` takes `truncate_at` **characters**. For multibyte UTF-8 text (CJK, emoji), this takes far more bytes than intended. Also allocates a new String unnecessarily.

Compare with the correct pattern used in `write.rs:111-116`, `edit.rs:175-180`, `bash.rs:145-149`:

```rust
content.truncate(truncate_at);
```

**Fix:**

```rust
let mut truncated_text = processed_text;
truncated_text.truncate(truncate_at);
```

---

### [WARN] list.rs - No result count limit

**File:** `src/tool/builtin/list.rs`

Unlike glob (MAX_RESULTS=1000) and grep (MAX_RESULTS=500), the list tool has no upper bound on results. With `depth: 100` on a large repo, it could return tens of thousands of entries, bloating context.

**Fix:** Add a `MAX_RESULTS` constant (e.g., 2000) and truncate with a message, matching the pattern in glob.rs.

---

### [WARN] guard.rs - `is_safe_command` does not handle subshells or process substitution

**File:** `src/tool/builtin/guard.rs:206-214`

The command chain splitter only splits on `&&`, `||`, `;`, `|`. It does not handle:

- Subshells: `$(rm -rf /)`
- Backticks: `` `rm -rf /` ``
- Process substitution: `<(cmd)`, `>(cmd)`
- Grouping: `{ cmd; }`

Any of these embedded in a "safe" command bypass the allowlist. This is inherent to prefix-based checking without shell parsing, but detecting `$(`, backticks, `<(`, `>(` in the raw string and rejecting would close common cases.

**Fix:** Add a check at the top of `is_safe_command`:

```rust
// Reject commands with subshell/substitution syntax
if command.contains("$(") || command.contains('`')
   || command.contains("<(") || command.contains(">(") {
    return false;
}
```

---

## Minor

### [NIT] read.rs:190-193 - Comment says "skip UTF-8 decode" but still decodes

**File:** `src/tool/builtin/read.rs:189-193`

```rust
} else if i >= start + count {
    // After collecting needed lines, just count remaining (skip UTF-8 decode)
    let line = line_result?;
    drop(line);
}
```

`BufReader::lines()` always performs UTF-8 validation/decoding. The `drop(line)` is a no-op. The comment is misleading. For large files this wastes CPU on UTF-8 decoding of lines we only need to count. In practice, the 1MB `MAX_FILE_SIZE` limit on full reads means offset/limit reads are bounded, so impact is low.

**Fix:** Either remove the misleading comment, or switch to `read_line` with a `Vec<u8>` buffer after collecting needed lines to actually skip UTF-8 decode.

---

### [NIT] types.rs:110-112 - `requires_sandbox` is dead code

**File:** `src/tool/types.rs:110-112`

The `requires_sandbox` method on the `Tool` trait is defined but never called anywhere in the codebase. Each tool handles sandbox checks internally via `ctx.check_sandbox()`.

**Fix:** Remove it, or document its intended purpose if planned for future use.

---

### [NIT] grep.rs - Sequential walker uses unnecessary Mutex

**File:** `src/tool/builtin/grep.rs:164-175`

The grep search function uses `Mutex<Vec<String>>` and `AtomicUsize`/`AtomicBool` for thread safety, but the walker is sequential (`.build()` not `.build_parallel()`). The atomics and mutex add minor overhead for no benefit.

**Fix:** Remove the Mutex/atomics and use plain `Vec` and `bool` since the walker is sequential. Or switch to a parallel walker for performance.

---

### [NIT] glob.rs - No `path` parameter for search root

**File:** `src/tool/builtin/glob.rs:24-35`

The glob tool always searches from the working directory. Unlike grep which has a `path` parameter, glob does not allow specifying a subdirectory to search. Models sometimes want to glob in a specific subdirectory (e.g., "find all tests in src/tool/").

The workaround is to include the path in the pattern (`src/tool/**/*test*`), which works but is less discoverable.

---

### [NIT] bash.rs:88 - Emoji in blocked command message

**File:** `src/tool/builtin/bash.rs:88`

Uses an emoji in the blocked message. Per project conventions (AGENTS.md: "No emoji unless requested"), this should be plain text.

---

## Ideas

### [IDEA] read.rs - Add line numbers to output

The read tool returns raw content without line numbers. Adding optional line numbers (like `cat -n`) would help the model reference specific lines in edit operations. Many competing agents include line numbers by default.

### [IDEA] bash.rs - Timeout parameter

The bash tool has no timeout -- it relies on user Ctrl+C via cancellation token. Adding an optional `timeout` parameter (default: 120s) would prevent runaway commands from blocking indefinitely.

### [IDEA] guard.rs - `analyze_command` should detect `sudo`

While `sudo` is not in the safe prefix list (so it's blocked in Read mode), in Write mode `sudo rm -rf /` is not flagged as dangerous by `analyze_command` because the check looks for `rm` at the command start, not after `sudo`. Consider stripping `sudo` prefix before analysis.

### [IDEA] glob.rs - Hidden files behavior inconsistency

`glob.rs` always hides hidden files (`.hidden`). `list.rs` has a `hidden` parameter. The glob tool should consider a `hidden` parameter for consistency.

---

## Per-tool Quality Summary

| Tool           | Schema | Edge Cases                     | Errors | Sandbox  | Output         | Perf                      |
| -------------- | ------ | ------------------------------ | ------ | -------- | -------------- | ------------------------- |
| read           | Good   | Good (offset/limit, size cap)  | Good   | Yes      | Good           | Minor (UTF-8 decode)      |
| write          | Good   | Good (dir creation, diff)      | Good   | Yes      | Good (diff)    | Good                      |
| edit           | Good   | Excellent (7 tests)            | Good   | Yes      | Good (diff)    | Good                      |
| bash           | Good   | Good (cancel, truncate)        | Good   | Dir only | Good           | Good                      |
| glob           | Good   | Good (parallel, truncate)      | Good   | Implicit | Good           | Good                      |
| grep           | Good   | Good (3 modes, context)        | Good   | Yes      | Good           | Minor (unnecessary Mutex) |
| list           | Good   | Missing limit                  | Good   | Yes      | Good           | Unbounded results         |
| compact        | Good   | N/A (sentinel)                 | N/A    | N/A      | N/A            | N/A                       |
| web_fetch      | Good   | Good (SSRF, streaming)         | Good   | N/A      | Truncation bug | Good                      |
| web_search     | Good   | Good (CAPTCHA, ads)            | Good   | N/A      | Good           | Good                      |
| spawn_subagent | Good   | Good                           | Good   | N/A      | Good           | Good                      |
| mcp_tools      | Good   | Good                           | Good   | N/A      | Good           | Good                      |
| guard          | Good   | Missing: env bypass, redirects | N/A    | N/A      | N/A            | Good                      |

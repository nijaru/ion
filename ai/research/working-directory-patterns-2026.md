# Working Directory Handling in TUI Coding Agents (February 2026)

**Research Date**: 2026-02-05
**Purpose**: How competing agents handle `cd`, working directory persistence, and directory tracking across tool calls
**Scope**: Claude Code, Codex CLI, Gemini CLI, opencode, aider, goose

---

## Summary Table

| Agent           | cd Persists? | Tracking Mechanism            | Dedicated cd Tool? | Shell Execution Model       |
| --------------- | ------------ | ----------------------------- | ------------------ | --------------------------- |
| **Claude Code** | Yes (cwd)    | Persistent bash session       | No                 | Long-lived bash process     |
| **Codex CLI**   | No           | `workdir` parameter on tool   | No (`--cd` flag)   | Per-call `bash -c`          |
| **Gemini CLI**  | No           | `directory` parameter on tool | No                 | Per-call `bash -c`          |
| **opencode**    | No           | `Instance.directory` as cwd   | No (requested)     | Per-call with fixed cwd     |
| **aider**       | N/A          | No bash tool (editor-focused) | No                 | User runs commands manually |
| **goose**       | No           | Extension-level working dir   | No                 | Per-call subprocess         |
| **ion (ours)**  | No           | `ToolContext.working_dir`     | No                 | Per-call `bash -c`          |

---

## 1. Claude Code (Anthropic)

### API-Level Bash Tool (bash_20250124)

The Anthropic API provides a built-in bash tool that maintains a **persistent bash session**:

- A long-lived `/bin/bash` process is spawned at session start
- Commands are written to stdin, output captured from stdout/stderr
- **Working directory persists** between commands (`cd /tmp` in call 1, `pwd` in call 2 shows `/tmp`)
- **Environment variables do NOT persist** between commands (each command gets a fresh env)
- A `restart` parameter allows resetting the session
- The `CLAUDE_ENV_FILE` env var can source a file before each command
- `CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR=1` resets cwd to project root after each command

### Known Issues

This is one of Claude Code's most-reported bugs:

- **Issue #1669** (65+ upvotes): "Claude Code frequently loses track of which directory it is in"
- **Issue #7442**: Claude uses `cd dir && command` but then tries to read files as if still in parent dir
- **Issue #6326** (marked COMPLETED): "Claude should be able to change directories and persist"
- The model (not the tool) loses track of cwd state, causing file access errors and in one case 60 hours of data loss from `git reset --hard` in the wrong directory

### Architecture

```
Session Start -> Spawn persistent /bin/bash process
Tool Call 1: write "cd /tmp\n" to stdin -> read output
Tool Call 2: write "ls\n" to stdin -> output shows /tmp contents (cwd persisted)
```

The tool itself tracks cwd correctly. The problem is the **model** not maintaining awareness of where it is.

**Source**: [Anthropic Bash Tool Docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/bash-tool), GitHub issues #1669, #2508, #7442

---

## 2. Codex CLI (OpenAI)

### Tool Schema

The `shell_command` (formerly `shell`) tool has an explicit `workdir` parameter:

```
shell_command:
  command: string (required) - The command to execute
  workdir: string (optional) - Working directory for the command
```

### Working Directory Model

- Each tool call is an **independent subprocess** (`bash -lc "<command>"`)
- No persistent shell session - cwd resets every call
- The model is instructed: "Always set the `workdir` param. Do not use `cd` unless absolutely necessary"
- CLI flag `--cd, -C` sets the initial workspace directory
- In `full-auto` mode, writes are confined to the workdir via OS-level sandbox (macOS Seatbelt / Linux Landlock)

### Known Issues

- **Issue #7344**: Model prepends `cd $(pwd)/... &&` to commands despite instructions not to. Closed as model training issue.
- **Issue #7761**: `workdir` metadata gets appended to command string, breaking execution
- The sandbox inspects the `workdir` parameter for scope; in-command `cd` triggers permission warnings

### Architecture

```
Tool Call -> Extract command + workdir
          -> spawn("bash", ["-lc", command], cwd=workdir)
          -> Capture output, return result
          -> Process dies, no state carries over
```

**Source**: [Codex CLI docs](https://developers.openai.com/codex/cli/reference/), GitHub issues #7344, #7761, DeepWiki analysis

---

## 3. Gemini CLI (Google)

### Tool Schema

The `run_shell_command` tool has an explicit `directory` parameter:

```
run_shell_command:
  command: string (required) - The exact shell command to execute
  description: string (optional) - Brief description of command purpose
  directory: string (optional) - Directory relative to project root to execute in
```

### Working Directory Model

- Each call is an **independent subprocess** (Unix: `bash -c`, Windows: `cmd.exe /c`)
- When `directory` is omitted, commands run in the **project root**
- The `directory` parameter is relative to project root, not absolute
- No persistent shell state between calls
- Sets `GEMINI_CLI=1` environment variable in spawned processes
- Supports background processes via `&` in command

### Architecture

```
Tool Call -> Extract command + directory
          -> resolved_dir = project_root / directory (or project_root if absent)
          -> spawn("bash", ["-c", command], cwd=resolved_dir)
          -> Return stdout, stderr, exit_code, directory
```

The return value explicitly includes the directory the command ran in, providing the model with ground truth about execution context.

**Source**: [Gemini CLI Shell Tool Docs](https://google-gemini.github.io/gemini-cli/docs/tools/shell.html)

---

## 4. opencode (Anomaly)

### Working Directory Model

- Commands execute with `cwd` set to `Instance.directory` (the project root)
- No persistent shell - each call is a subprocess
- The system prompt instructs: "Try to maintain your current working directory throughout the session by using absolute paths and avoiding usage of cd"
- All file tool paths must be **absolute** (relative paths joined with `process.cwd()`)

### Known Issues

- **Issue #4982**: "agent always using `cd [cwd] && ....` even when cd isn't needed" - same model behavior problem as Codex
- **Issue #2177**: "Allow explicitly changing working directory" - requests `/cd` command and ability to modify outside cwd. No resolution yet. The dev had a literal note on his desk: "Make agent more aware of cwd"

### Architecture

```
Tool Call -> command runs with cwd=Instance.directory
          -> Subprocess completes, state discarded
          -> No cd tracking, model must use absolute paths
```

**Source**: GitHub issues #2177, #4982, [OpenCode deep dive](https://cefboud.com/posts/coding-agents-internals-opencode-deepdive/)

---

## 5. aider

aider does **not have a bash/shell tool**. It is focused purely on code editing:

- 4 core modes: whole file, diff, editor-diff, architect
- Commands run by the **user** via `/run` or `!` prefix
- Working directory is wherever aider was launched
- No directory change capability within the tool
- Issue #968 requested shell execution capability; response: "aider is a code editor, not a shell"

**Source**: [aider FAQ](https://aider.chat/docs/faq.html), GitHub issue #968

---

## 6. goose (Block)

### Tool Schema

The Developer extension's `shell` tool executes commands:

```
shell:
  command: string (required) - Shell command to execute
```

### Working Directory Model

- Commands run via subprocess with cwd set to the project/session working directory
- The `-w, --working_dir` CLI flag filters sessions by working directory
- goose tracks "Projects" that associate working directories with sessions
- No persistent shell between calls
- MCP-based architecture: the Developer extension is an MCP server providing the `shell` tool

### Architecture

```
User message -> Agent decides tool call
            -> Developer extension receives shell request
            -> Subprocess with cwd=project_dir
            -> Return output
```

**Source**: [Goose Developer Extension](https://block.github.io/goose/docs/mcp/developer-mcp/), [Goose CLI Commands](https://block.github.io/goose/docs/guides/goose-cli-commands/)

---

## 7. ion (Current State)

### Current Implementation

ion uses per-call subprocess execution, identical to the Codex/Gemini pattern:

```rust
// src/tool/builtin/bash.rs:83-86
let child = Command::new("bash")
    .arg("-c")
    .arg(command_str)
    .current_dir(&ctx.working_dir)   // Always project root
```

- `ToolContext.working_dir` is set once from `Session.working_dir` at session creation
- Never changes during a session
- No `directory`/`workdir` parameter on the bash tool
- No persistent shell process

---

## Design Patterns Comparison

### Pattern A: Persistent Shell (Claude Code API)

```
Pros:
+ cd naturally persists
+ Environment modifications carry over (with workarounds)
+ Feels like a real terminal
+ No parameter overhead

Cons:
- Model loses track of cwd (major bug source)
- Process lifecycle management complexity
- Harder to sandbox (long-lived process)
- Shell state can accumulate cruft
- Environment variables DON'T actually persist (each command sourced fresh)
```

### Pattern B: Per-Call with Directory Parameter (Codex, Gemini)

```
Pros:
+ Clean slate each call (no accumulated state)
+ Explicit directory in tool schema (model must specify)
+ Easier sandboxing (each subprocess scoped)
+ Directory visible in tool call for approval UI

Cons:
- cd doesn't persist (confuses models anyway)
- Extra parameter adds token cost
- Models still try to use cd despite instructions
- Multi-step shell workflows require chaining in one call
```

### Pattern C: Fixed CWD Subprocess (opencode, goose, ion)

```
Pros:
+ Simplest implementation
+ Always runs from known location
+ No ambiguity about working directory

Cons:
- Cannot work in subdirectories without cd prefix
- Model develops cd-prepending habit
- No way to change context mid-session
```

---

## Recommendations for ion

### Short-term: Add `directory` parameter (Pattern B)

Follow Gemini CLI's approach -- add an optional `directory` parameter to the bash tool:

```rust
fn parameters(&self) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "The command to execute"
            },
            "directory": {
                "type": "string",
                "description": "Working directory for this command, relative to project root. Defaults to project root if omitted."
            }
        },
        "required": ["command"]
    })
}
```

Resolve: `ctx.working_dir.join(directory)` with sandbox check.

This is the cleanest pattern because:

1. Each call is explicit about where it runs
2. The model must declare intent (visible in approval UI)
3. Sandbox validation is straightforward (check resolved path)
4. Matches what Codex and Gemini do (models are trained for this pattern)

### Medium-term: Return cwd in result metadata

Include the actual execution directory in tool results:

```rust
metadata: Some(json!({
    "exit_code": output.status.code(),
    "directory": resolved_dir.display().to_string(),
    "truncated": truncated,
}))
```

This gives the model ground truth about where the command ran, reducing confusion.

### Not recommended: Persistent shell

Claude Code's persistent shell is the source of their most-reported bug category. The complexity of tracking shell state, handling process lifecycle, and keeping the model aware of accumulated state changes outweighs the benefit of natural `cd` persistence. Models are already trained for the per-call pattern via Claude API and Codex/Gemini usage.

---

## References

- Anthropic Bash Tool: https://platform.claude.com/docs/en/agents-and-tools/tool-use/bash-tool
- Claude Code Issues: #1669, #2508, #6326, #7442
- Codex CLI: https://developers.openai.com/codex/cli/reference/
- Codex Issues: #7344, #7761
- Gemini CLI Shell Tool: https://google-gemini.github.io/gemini-cli/docs/tools/shell.html
- opencode Issues: #2177, #4982
- Goose Developer Extension: https://block.github.io/goose/docs/mcp/developer-mcp/
- aider FAQ: https://aider.chat/docs/faq.html

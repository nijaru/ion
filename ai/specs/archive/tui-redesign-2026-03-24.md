# TUI Redesign — 2026-03-24

## Goal

Rewrite `internal/app/` to use Bubbletea v2 inline mode correctly: completed chat entries
commit to real terminal scrollback via `tea.Printf`; `View()` returns only the dynamic
bottom area. Matches the rendering model of Claude Code and the Rust-era ion design.

---

## Rendering Model

### Two Planes

**Plane A — Terminal Scrollback (permanent)**

Committed chat history printed via `tea.Printf`. Each entry lands in native terminal
scrollback and is never touched again. Supports Cmd+F search, text selection, and
scroll arbitrarily far back. No in-app scroll management needed.

Entries committed here:

- User messages (on submit)
- Completed assistant messages (on TurnFinished)
- Tool results: name, args, output (on ToolResult)
- Diffs for write/edit tools (after tool completes, post-approval if required)
- Sub-agent results (on ChildCompleted/ChildFailed)
- System notices (approval decision, model change, etc.)

**Plane B — Ephemeral Dynamic Area (in View)**

Everything in-flight. Rendered fresh each frame. Cleared entry-by-entry as items
commit to scrollback.

Contents:

- Streaming assistant text (token by token as AssistantDelta arrives)
- Active tool call: name, args, live output (as ToolOutputDelta streams)
- Approval prompt when ApprovalRequest arrives (y/n question)
- Sub-agent status (ChildRequested → ChildDelta)
- Thinking/reasoning text (ThinkingDelta, dimmed, shown while generating)

Note: diffs are not available until ToolResult arrives. There is no pre-result diff
preview in Plane B. The diff is rendered to scrollback on ToolResult.

### Layout

```
[terminal scrollback — committed history]
[empty line]

  • streaming assistant text...

  • bash("go test ./...")
    FAIL: TestFoo (0.12s)
    FAIL

  • write(internal/app/model.go)
    + func New(b backend.Backend, ...) Model {
    + ...

  Approve write to internal/app/model.go? (y/n)

⠿ Ionizing...
──────────────────────────────────────────────────────
 › cursor here (auto-expanding, 1–10 lines)
──────────────────────────────────────────────────────
[READ] · claude-sonnet-4-5 · 12k/200k · $0.04 · ./ion · main
```

### Commit Rules

| Event              | Plane B action          | Scrollback action                        |
| ------------------ | ----------------------- | ---------------------------------------- |
| User submit        | —                       | `tea.Printf` user message                |
| TurnStarted        | show streaming area     | —                                        |
| AssistantDelta     | append to stream buf    | —                                        |
| AssistantMessage   | clear stream buf        | `tea.Printf` completed assistant message |
| TurnFinished       | update progress → Ready | —                                        |
| ToolCallStarted    | show tool placeholder   | —                                        |
| ToolOutputDelta    | append to tool buf      | —                                        |
| ApprovalRequest    | show y/n prompt         | —                                        |
| Approval given     | clear prompt            | `tea.Printf` "Approved/Denied: ..."      |
| ToolResult         | clear tool entry        | `tea.Printf` tool result + diff          |
| VerificationResult | —                       | `tea.Printf` pass/fail result            |
| ChildRequested     | show agent status       | —                                        |
| ChildDelta         | append to agent buf     | —                                        |
| ChildCompleted     | clear agent entry       | `tea.Printf` agent result                |
| ChildFailed        | clear agent entry       | `tea.Printf` agent error                 |
| ChildStarted       | update agent label      | —                                        |
| Error              | clear all Plane B       | progress line shows error                |

---

## Progress Line

Single line between Plane B and the top separator. Always compact — one line, no tool
details. Purpose: make it immediately clear whether the agent is working, done, or
errored. Tool details (name, output) live in Plane B above.

| State              | Display               |
| ------------------ | --------------------- |
| Idle               | `· Ready`             |
| Waiting on backend | `⠿ Ionizing...`       |
| Streaming text     | `⠿ Streaming...`      |
| Tool running       | `⠿ Working...`        |
| Awaiting approval  | `⚠ Approval required` |
| Cancelled          | `· Cancelled`         |
| Error              | `✗ Error: <msg>`      |

Spinner animates on all `⠿` states (`m.thinking == true`). States derive from the
most recent session event — e.g. `TurnStarted` → Ionizing, `AssistantDelta` →
Streaming, `ToolCallStarted` → Working, `TurnFinished` → Ready.

---

## Status Line

Left to right, mode first:

```
[READ] · openrouter · claude-sonnet-4-5 · 12k/200k (6%) · $0.042 · ./ion · main
```

- `[READ]` / `[WRITE]` — tool mode, left-aligned, color-coded (cyan/yellow)
- `provider` — shown separately from model; dim; omit if empty
- `model` — shown separately; dim
- `Xk/Yk (N%)` — used/limit token counts + percentage; drop limit if unknown
- `$0.042` — cumulative cost, shown only when > 0
- `./dir` — `filepath.Base(workdir)` prefixed with `./`, hidden on narrow terminals
- `branch` — git branch name, cyan, hidden on narrow terminals

Width-responsive: drop rightmost segments when terminal narrows below 80/60 cols.

---

## Composer

- `textarea` bubble (charm.land/bubbles/v2)
- Auto-expands from 1 to 10 lines as content grows
- `Enter` sends, `Shift+Enter` inserts newline
- `Ctrl+C` double-tap quits; single clears input
- `Esc` double-tap clears input; single cancels in-flight turn
- Readline keybindings: `Ctrl+A/E`, `Ctrl+W/U/K`, `Alt+B/F`
- Up/Down: input history navigation when cursor is on first/last line

---

## Keybindings

### Input Mode

| Key           | Action                                   |
| ------------- | ---------------------------------------- |
| `Enter`       | Send message                             |
| `Shift+Enter` | Insert newline                           |
| `Ctrl+C`      | Single: clear input. Double: quit        |
| `Esc`         | Single: cancel in-flight. Double: clear  |
| `Shift+Tab`   | Toggle READ/WRITE tool mode              |
| `Up`          | History prev (when on first line)        |
| `Down`        | History next (when on last line)         |
| `PageUp`      | — (terminal scroll, not intercepted)     |
| `y` / `n`     | Approve/deny when ApprovalRequest active |

### Readline (inside composer)

| Key      | Action               |
| -------- | -------------------- |
| `Ctrl+A` | Line start           |
| `Ctrl+E` | Line end             |
| `Ctrl+W` | Delete word backward |
| `Ctrl+U` | Delete to line start |
| `Ctrl+K` | Delete to line end   |
| `Alt+B`  | Word left            |
| `Alt+F`  | Word right           |

---

## Slash Commands

Handled by `commands.go`:

| Command            | Action                          |
| ------------------ | ------------------------------- |
| `/model <name>`    | Switch model, save to config    |
| `/provider <name>` | Switch provider, save to config |
| `/mcp add <cmd>`   | Register MCP server             |
| `/exit`, `/quit`   | Quit                            |

System message committed to scrollback on success.

---

## Diff Rendering

For `write` and `edit` tool results, render a unified diff inline using `go-udiff`
(already in go.mod). Format:

```
• write(internal/app/model.go)
  @@ -12,6 +12,8 @@
  -func old() {
  +func new() {
  +    // improved
   }
```

Added lines: green. Removed lines: red. Context lines: dim.
Diff commits to scrollback only after `ToolResult` arrives (post-approval if guarded).

---

## Tool Output Rendering

Default truncation: 10 lines with `... (N more lines)` indicator.
Full output is stored in `m.pending.Content` during streaming.
On commit: truncated version goes to scrollback.

Tool display format:

```
• tool_name(key_arg)    ← cyan, bold label
  output line 1         ← dim, indented 2
  output line 2
  ... (47 more lines)
```

For error results: label shows `✗ tool_name(...)` in red.

---

## File Structure

```
internal/app/
  model.go     — Model struct, New(), Init(), top-level Update() dispatch
  events.go    — handleKey(), handleSessionEvent()
  render.go    — View(), renderEntry(), renderPlaneB(), progressLine(), statusLine(), layout()
  commands.go  — handleCommand() for slash commands
  styles.go    — all lipgloss.Style definitions, color palette
```

### model.go responsibilities

- `Model` struct (all state fields)
- `New()` constructor
- `Init()` — start event loop, ticker, focus composer
- `Update()` — top-level dispatch: routes `tea.KeyPressMsg` → `handleKey`,
  session events → `handleSessionEvent`
- `awaitSessionEvent()` command

### events.go responsibilities

- `handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd)`
- `handleSessionEvent(ev session.Event) (Model, tea.Cmd)` — `session.Event` is the
  existing interface in `internal/session/event.go`; no new type needed
- Input history state and navigation

### render.go responsibilities

- `View() tea.View`
- `renderPlaneB() string` — streaming text, active tools, approval prompt
- `renderEntry(entry session.Entry) string` — for scrollback commits
- `progressLine() string`
- `statusLine() string`
- `layout()` — compute composer and viewport dimensions

### commands.go responsibilities

- `handleCommand(input string) tea.Cmd`
- One case per slash command

### styles.go responsibilities

- All `lipgloss.Style` fields moved out of Model
- Single `styles` struct initialized once in `New()`
- Color palette constants

---

## State Changes vs Current

| Current                    | New                                     |
| -------------------------- | --------------------------------------- |
| `viewport.Model`           | removed                                 |
| `appendHistory(string)`    | `tea.Printf(renderEntry(entry))`        |
| styles on Model fields     | `styles` sub-struct in `styles.go`      |
| `headerRows`, `footerRows` | computed dynamically in `layout()`      |
| No input history           | `[]string` history + index on Model     |
| No readline keys           | handled in `events.go`                  |
| No diff rendering          | `go-udiff` in `events.go` / `render.go` |
| Single `model.go`          | 5 files                                 |

---

## Non-Goals (this iteration)

- `@file` path completer
- `/command` autocomplete popup
- Ctrl+R history search
- Model/provider picker UI
- Image display (iTerm/Kitty)
- Syntax highlighting in tool output

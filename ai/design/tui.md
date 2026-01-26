# Design Spec: TUI Interface

## 1. Overview

A professional, high-readability terminal interface built with `ratatui`, designed for developers who live in the CLI.

## 2. Layout & Components

- **History**: Terminal scrollback (inline viewport; no app-managed chat box).
- **Ionizing Line**: Shows during execution: `⠼ Ionizing... (14s · ↑ 4.1k · ↓ 2.1k)`
- **Input Area**: Multi-line editor with history recall and word-level navigation.
- **Status Line**: `model · 56% (112k/200k)` on the left, `? help` on the right.
- **Selector UI**: Bottom-anchored takeover for provider/model selection (shared shell).

## 3. Interaction

### 3.1 Keybindings

| Key           | Action                                        |
| ------------- | --------------------------------------------- |
| `Enter`       | Send message / Select                         |
| `Shift+Enter` | Newline                                       |
| `Shift+Tab`   | Cycle Tool Mode (Read → Write → Agi)          |
| `Ctrl+M`      | Model Selector (page)                         |
| `Ctrl+P`      | Provider Selector (page)                      |
| `Ctrl+T`      | Cycle thinking level (off → low → med → high) |
| `Ctrl+G`      | Open input in external editor (tk-otmx)       |
| `Ctrl+C`      | Clear input; double-tap cancel/quit when empty |
| `Ctrl+H`      | Help overlay                                  |
| `PageUp/Down` | Terminal scrollback (native)                  |
| `Up/Down`     | History recall / cursor movement              |
| `Up` (running, input empty) | Pull queued messages into editor |

### 3.2 Slash Commands

- System commands use a single `/` (reserved).
- Custom skills use `//` so users can name skills freely without colliding with system commands.
- Custom commands are standardized on agent skills (no bespoke command handlers).
- `/model`, `/models`: Open model selector
- `/provider`, `/providers`: Open provider selector
- `/clear`: Reset history and session state
- `/help`, `/?`: Show interactive help
- `/index [path]`: Index codebase for memory
- `/quit`, `/exit`, `/q`: Exit

## 4. Visual Polish

### 4.1 Implemented

- **ANSI Colors**: Tool output preserves terminal colors (git, ls, etc.)
- **Markdown**: Code blocks, lists, bold text via tui-markdown
- **Message Indicators**: `>` prefix on first line of user messages; user text dim cyan; dimmed bracketed system notices

### 4.2 Planned

- **Diff Highlighting** (tk-smqs): See [diff-highlighting.md](diff-highlighting.md)
  - Green for additions, red for deletions
  - Word-level highlighting for specific changes

- **Syntax Highlighting**: tree-sitter for code blocks

### 4.3 Tool Execution Visibility (tk-arh6)

Current: Progress line shows "Ionizing..." for all states
Planned: Show tool name during execution: `⏺ Running bash...`

## 5. External Editor (tk-otmx)

Ctrl+G opens input in $EDITOR:

```rust
fn open_in_editor(&mut self) {
    let temp = tempfile::NamedTempFile::new()?;
    std::fs::write(temp.path(), &self.input)?;

    let editor = std::env::var("EDITOR").unwrap_or("vim".into());
    std::process::Command::new(&editor)
        .arg(temp.path())
        .status()?;

    self.input = std::fs::read_to_string(temp.path())?;
    self.cursor_pos = self.input.len();
}
```

## 6. Selector Handling

Escape closes the selector and returns to input. If onboarding requires a selection, Esc returns from Model → Provider during setup.
When no selector is open, Escape cancels a running task but never quits.

## 7. Debugging & Testing

- **TestBackend**: Programmatic rendering for unit testing
- **Snapshotting**: Ctrl+S dumps buffer to `ai/tmp/tui_snapshot.txt`

## Related Specs

- [diff-highlighting.md](diff-highlighting.md) - Detailed diff rendering design
- [interrupt-handling.md](interrupt-handling.md) - Ctrl+C behavior spec

## Progress Bar Enhancements (ref: Claude Code)

**Current:** `Ionizing... ↑1.2k ↓3.4k tokens (Esc to cancel)`

**Target:**
- Add elapsed timer: `· Ionizing… (Esc to interrupt · 6m 57s · ↑1.2k ↓3.4k tokens)`
- Add thinking indicator: `· thinking` appears during thinking blocks
- After thinking: `thought for 5s`

**Thinking blocks:**
- Don't render thinking content in chat
- Show placeholder: "thinking" (active) or "thought for Xs" (done)
- Keep thinking content internal only

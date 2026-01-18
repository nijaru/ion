# Design Spec: TUI Interface

## 1. Overview

A professional, high-readability terminal interface built with `ratatui`, designed for developers who live in the CLI.

## 2. Layout & Components

- **Message List**: Scrollable history with color-coded senders (Cyan: User, Green: Agent, Magenta: Tool).
- **Progress Line**: Shows during execution: `⠼ Ionizing... (14s · ↑ 4.1k · ↓ 2.1k)`
- **Input Area**: Multi-line editor with history recall and word-level navigation.
- **Status Line**: `model · 56% (112k/200k) · [branch] · cwd`
- **Modals**: Full-screen or centered overlays for Provider and Model selection.

## 3. Interaction

### 3.1 Keybindings

| Key           | Action                                        |
| ------------- | --------------------------------------------- |
| `Enter`       | Send message / Select                         |
| `Shift+Enter` | Newline                                       |
| `Shift+Tab`   | Cycle Tool Mode (Read → Write → Agi)          |
| `Ctrl+M`      | Model Picker                                  |
| `Ctrl+P`      | Provider Picker                               |
| `Ctrl+T`      | Cycle thinking level (off → low → med → high) |
| `Ctrl+G`      | Open input in external editor (tk-otmx)       |
| `Ctrl+C`      | Cancel running task / Clear input / Quit      |
| `Ctrl+H`      | Help overlay                                  |
| `PageUp/Down` | Scroll chat history                           |
| `Up/Down`     | History recall / cursor movement              |

### 3.2 Slash Commands

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
- **Message Indicators**: ↑ You, ↓ model-name, ⏺ tool

### 4.2 Planned

- **Diff Highlighting** (tk-smqs): See [diff-highlighting.md](diff-highlighting.md)
  - Green for additions, red for deletions
  - Word-level highlighting for specific changes

- **Syntax Highlighting**: tree-sitter for code blocks

- **Git Stats in Status Line** (tk-whde): `+12 -5 3 files`

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

## 6. Modal Handling (tk-o4uo)

Escape should always close modals, even during setup. If selection required, re-open after Esc.

## 7. Debugging & Testing

- **TestBackend**: Programmatic rendering for unit testing
- **Snapshotting**: Ctrl+S dumps buffer to `ai/tmp/tui_snapshot.txt`

## Related Specs

- [diff-highlighting.md](diff-highlighting.md) - Detailed diff rendering design
- [interrupt-handling.md](interrupt-handling.md) - Ctrl+C behavior spec

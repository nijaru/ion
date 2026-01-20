# Design Spec: TUI Interface

## 1. Overview
A professional, high-readability terminal interface built with `ratatui`, designed for developers who live in the CLI.

## 2. Layout & Components
- **Message List**: Scrollable history with color-coded senders (Cyan: User, Green: Agent, Magenta: Tool).
- **Input Area**: Multi-line editor with history recall and word-level navigation.
- **Status Line**: Context window usage %, Model ID, and Tool Mode.
- **Modals**: Full-screen or centered overlays for Provider and Model selection.

## 3. Interaction

### 3.1 Keybindings
- `Enter`: Send message / Select.
- `Shift+Enter`: Newline.
- `Tab`: Cycle Tool Mode (Read -> Write -> Agi).
- `Ctrl+M`: Model Picker.
- `Ctrl+P`: Provider Picker.
- `PageUp/Down`: Scroll chat history.
- `Double-ESC`: Cancel running task.

### 3.2 Slash Commands
- `/models`: Open model selector.
- `/providers`: Open provider selector.
- `/clear`: Reset history and session state.
- `/help`: Show interactive help.
- `/snapshot`: Save UI debug view.

## 4. Visual Polish
- **Markdown**: Support for code blocks, lists, and bold text.
- **Syntax Highlighting**: Using `syntect` or similar for code block readability.
- **Diff View**: Git-style unified diffs for file modifications using the `similar` crate.

## 5. Debugging & Testing
- **TestBackend**: Programmatic rendering of the UI to strings for unit testing.
- **Snapshotting**: `/snapshot` dumps the current buffer to `ai/tmp/tui_snapshot.txt`.

# ion TUI Specification

**Vision**: A high-performance, minimalist terminal agent that feels like a native system utility.

## Layout Hierarchy

1. **History Buffer** (Top 90%):
   - Scrollable area for messages.
   - **User Message**: Prefix `❯`, color `Text`.
   - **Delimiter**: A single thin line `──────────────────` (color `Surface0`).
   - **Agent Message**: Prefix `▼`, color `Lavender`.
   - **Thinking blocks**: Collapsible `Overlay1` text with a spinner.

2. **Command/Edit View**:
   - **Read/Search**: Single-line status logs.
   - **Edits**: Inline unified diffs using the `similar` crate.
   - Red (`Maroon`) for removals, Green (`Green`) for additions.

3. **Statusline** (Bottom):
   - Format: `{Model} · {Context}% ({Used}/{Limit}) | [{Branch}] · {Cwd}`
   - Theme: Catppuccin Mocha (Default).

## Interaction Model

- **Scrollback**: PageUp/PageDown and Mouse Wheel support. Virtualized rendering for large buffers.
- **Completion**: Tab-cycle for file paths in the prompt.
- **Interrupt**: `Ctrl-C` to stop the agent's current turn without closing the TUI.

## Theming (Catppuccin Mocha)

| Element    | Color    | Hex     |
| ---------- | -------- | ------- |
| Background | Base     | #1e1e2e |
| User Text  | Text     | #cdd6f4 |
| Agent Text | Lavender | #b4befe |
| Delimiter  | Surface0 | #313244 |
| Diff Add   | Green    | #a6e3a1 |
| Diff Rem   | Maroon   | #eba0ac |
| Thinking   | Overlay1 | #7f849c |

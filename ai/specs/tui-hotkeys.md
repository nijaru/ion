# TUI Hotkeys

Source of truth for inline TUI key semantics.

## Composer
- `Enter`: send message
- `Shift+Enter`: insert newline
- `Alt+Enter`: insert newline
- `Up` / `Down`: history only when the cursor is at the start/end boundary where recall makes sense

## Safety
- `Esc`: if a turn is running, cancel it. If idle with non-empty input, double-tap to clear input.
- `Ctrl+C`: if input is non-empty, clear it. If idle and input is empty, double-tap to quit.
- `Ctrl+D`: if idle and input is empty, double-tap to quit. Never clear input.
- `Ctrl+C` and `Ctrl+D` do not cancel running turns.

## Pickers
- `Ctrl+P`: open provider picker
- `Ctrl+M`: open model picker
- `Tab`: swap provider/model picker when a picker is open
- `Esc`, `Ctrl+C`, `Ctrl+D`: close an open picker

## Mode
- `Shift+Tab`: toggle read/write mode

## Transcript / Plane B
- Startup header prints once to native scrollback.
- Committed rows print to native scrollback.
- Plane B contains only live streaming content, one blank spacer, progress line, textarea, and status line.

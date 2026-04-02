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
- `Ctrl+P`: toggle the primary/fast model preset
- `Ctrl+T`: open thinking controls
- `Tab`: cycle picker scope when a picker is open
- `Shift+Tab`: cycle picker scope in reverse when a picker is open
- `Esc`, `Ctrl+C`, `Ctrl+D`: close an open picker

## Slash Commands
- `/...`: built-in ion command surface for stateful actions
- `//...`: user-defined command or skill aliases
- `/primary`: switch to the primary model preset
- `/fast`: switch to the fast model preset
- `/model`: fuzzy model selection and preset management
- `/provider`: fuzzy provider selection
- `/thinking`: explicit thinking budget control
- Prefer slash commands for explicit configuration, switching, and other actions that benefit from text discoverability

## Binding principles
- Avoid function keys as primary bindings. They are available in many terminals, but they are a weak default on macOS laptops and less reliable through terminal/multiplexer stacks.
- Avoid relying on `Ctrl+Shift+<letter>` as a primary binding family. Some terminals can distinguish it, but legacy terminal handling often collapses it with the plain `Ctrl+<letter>` chord.
- Avoid using `Option` / `Alt` as the primary modifier family. Claude Code uses it, but only after terminal-specific setup; that is too fragile for ion's default cross-platform TUI path.
- Avoid adding more global `Ctrl+<letter>` bindings unless the action is truly core. The composer intentionally preserves common terminal editing keys.
- Desired state:
  - `primary` and `fast` are the only UI-exposed model presets
  - `summary` stays config-only
  - `Ctrl+P` is the fast model swap accelerator
  - fuzzy slash commands own the full model/provider selection surface

## Mode
- `Shift+Tab`: toggle read/write mode

## Transcript / Plane B
- Startup header prints once to native scrollback.
- Committed rows print to native scrollback.
- Plane B contains only live streaming content, one blank spacer, progress line, textarea, and status line.

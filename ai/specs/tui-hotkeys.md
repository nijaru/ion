# TUI Hotkeys

Source of truth for inline TUI key semantics.

## Composer
- `Enter`: send message
- `Shift+Enter`: insert newline
- `Alt+Enter`: insert newline
- `Up` / `Down`: history only when the cursor is at the start/end boundary where recall makes sense
- `Ctrl+P` / `Ctrl+N`: history with the same boundary-aware behavior as `Up` / `Down`

## Safety
- `Esc`: cancel a running turn. Otherwise it does nothing.
- `Ctrl+C`: if input is non-empty, clear it. If idle and input is empty, double-tap to quit.
- `Ctrl+D`: if idle and input is empty, double-tap to quit. Never clear input.
- `Ctrl+C` and `Ctrl+D` do not cancel running turns.

## Pickers
- `Ctrl+M`: toggle the primary/fast model preset; when the model picker is open, it switches which preset slot will be updated
- `Ctrl+T`: open thinking controls
- `Tab`: swap between the provider picker and the model picker
- `PgUp` / `PgDn`: page through long picker lists
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
  - `Ctrl+M` toggles the active preset slot
  - `Ctrl+T` opens thinking controls
  - the model picker keeps Configured presets at the top of the list
  - `Tab` swaps provider/model pickers
  - `PgUp` / `PgDn` page through long picker lists
  - fuzzy slash commands own the full model/provider selection surface

## Mode
- `Shift+Tab`: toggle READ/EDIT mode. AUTO requires an explicit slash command or startup flag.

## Transcript / Plane B
- Startup header prints once to native scrollback.
- Committed rows print to native scrollback.
- Plane B contains only live streaming content, one blank spacer, progress line, textarea, and status line.

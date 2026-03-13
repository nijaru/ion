# Keybindings

## Current State

### Input Mode

| Binding       | Action                               |
| ------------- | ------------------------------------ |
| `Enter`       | Send message                         |
| `Shift+Enter` | Insert newline                       |
| `Esc`         | Cancel task / double-tap clear input |
| `Ctrl+C`      | Clear input / double-tap quit        |
| `Ctrl+D`      | Double-tap quit (when empty)         |
| `Ctrl+G`      | Open external editor                 |
| `Ctrl+H`      | Help overlay                         |
| `Ctrl+M`      | Model selector                       |
| `Ctrl+P`      | Provider selector                    |
| `Ctrl+T`      | Cycle thinking level                 |
| `Shift+Tab`   | Cycle tool mode                      |
| `?`           | Help (when empty)                    |
| `Up/Down`     | History / cursor movement            |
| `PageUp/Down` | Scroll chat                          |

### Line Editing (Readline-compatible)

| Binding           | Action                        |
| ----------------- | ----------------------------- |
| `Ctrl+A`          | Line start                    |
| `Ctrl+E`          | Line end                      |
| `Ctrl+W`          | Delete word backward          |
| `Ctrl+U`          | Delete to line start          |
| `Ctrl+K`          | Delete to line end            |
| `Alt+B`           | Word left                     |
| `Alt+F`           | Word right                    |
| `Ctrl+Left/Right` | Word movement                 |
| `Cmd+Left/Right`  | Visual line start/end (macOS) |

### Selector Mode

| Binding     | Action                               |
| ----------- | ------------------------------------ |
| `Up/Down`   | Navigate                             |
| `Enter`     | Select                               |
| `Esc`       | Cancel                               |
| `Tab`       | Switch pages (provider/model)        |
| `Backspace` | Back to provider (when filter empty) |
| Type        | Filter items                         |

## Proposed Changes

### Phase 1: Readline History

| Binding  | Current           | Proposed                        |
| -------- | ----------------- | ------------------------------- |
| `Ctrl+P` | Provider selector | Previous history                |
| `Ctrl+N` | (unused)          | Next history                    |
| `Ctrl+M` | Model selector    | Unified provider/model selector |
| `Ctrl+H` | Help overlay      | Remove (use `?` / `/help`)      |

**Ctrl+M behavior:**

- No provider → Opens provider picker
- Has provider → Opens model picker
- Tab switches pages within selector

### Phase 2: Optional Readline (Low Priority)

| Binding  | Action     |
| -------- | ---------- |
| `Ctrl+B` | Char left  |
| `Ctrl+F` | Char right |

### Undecided

| Binding  | Possible Use         | Notes                                |
| -------- | -------------------- | ------------------------------------ |
| `Ctrl+R` | Input history search | Mini-selector with fuzzy filter      |
| `Ctrl+L` | Clear/redraw screen  | Conflicts conceptually with `/clear` |

## Readline Bindings We Skip

| Binding  | Readline Use        | Why Skip                  |
| -------- | ------------------- | ------------------------- |
| `Ctrl+D` | Delete char         | We use for quit           |
| `Ctrl+T` | Transpose           | We use for thinking level |
| `Ctrl+G` | Abort               | We use for editor         |
| `Ctrl+Y` | Yank                | Kill ring is complex      |
| `Alt+D`  | Delete word forward | Less common               |

## Design Principles

1. **Readline where practical** - Ctrl+P/N/A/E/U/K/W are muscle memory
2. **TUI conventions** - Ctrl+G for editor, Shift+Tab for mode cycling
3. **Simple over complete** - No kill ring, no transpose
4. **Discoverable** - `/help` and `?` for help, slash commands for actions

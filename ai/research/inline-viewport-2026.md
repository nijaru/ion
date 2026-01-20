# Inline Viewport Mode for Terminal TUI Agents

Research on how terminal coding agents implement inline viewport rendering without alternate screen buffer.

**Date:** 2026-01-20

## Summary

| Tool         | Framework   | Approach                  | Key Feature                      |
| ------------ | ----------- | ------------------------- | -------------------------------- |
| Claude Code  | Ink (React) | Incremental line update   | `incrementalRendering: true`     |
| OpenAI Codex | Ink (React) | Debug mode or incremental | Line-by-line rewrite             |
| Gemini CLI   | Ink (React) | Same as above             | Shared pattern                   |
| pi-mono      | Custom TS   | Direct ANSI escape codes  | Cursor positioning + DECSTBM     |
| ratatui      | Native Rust | `Viewport::Inline(n)`     | `insert_before()` for scrollback |

**Recommendation:** Use ratatui's native `Viewport::Inline` - it provides exactly what we need with proper Rust integration.

## ratatui Inline Viewport (Recommended)

ratatui has first-class support for inline viewport rendering since v0.23.0.

### How It Works

```rust
use ratatui::{Terminal, TerminalOptions, Viewport};

fn main() -> Result<()> {
    // Create terminal with inline viewport of 8 lines
    let mut terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(8),  // Fixed height at bottom
    });

    // Normal drawing works within the viewport area
    terminal.draw(|frame| {
        // Renders within the 8-line viewport
        frame.render_widget(my_widget, frame.area());
    })?;

    // Push content to terminal scrollback
    terminal.insert_before(1, |buf| {
        Paragraph::new("This becomes scrollback history")
            .render(buf.area, buf);
    })?;

    ratatui::restore();
    Ok(())
}
```

### Viewport Options

```rust
pub enum Viewport {
    Fullscreen,           // Uses alternate screen (default)
    Inline(u16),          // Inline mode with fixed height
    Fixed(Rect),          // Fixed area anywhere on screen
}
```

### Key Methods

| Method                         | Purpose                                     |
| ------------------------------ | ------------------------------------------- |
| `Viewport::Inline(height)`     | Create viewport at current cursor position  |
| `terminal.insert_before(n, f)` | Push n lines above viewport into scrollback |
| `terminal.autoresize()`        | Handle terminal resize in inline mode       |

### Example: Download Progress (Official)

From ratatui examples, showing inline viewport with `insert_before`:

```rust
// Initialize inline viewport
let mut terminal = ratatui::init_with_options(TerminalOptions {
    viewport: Viewport::Inline(8),
});

// When a download completes, push to scrollback
terminal.insert_before(1, |buf| {
    Paragraph::new(Line::from(vec![
        Span::from("Finished "),
        Span::styled(
            format!("download {download_id}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::from(format!(" in {}ms", elapsed)),
    ]))
    .render(buf.area, buf);
})?;
```

### Escape Sequences Used Internally

ratatui's inline viewport uses these terminal escape sequences:

| Sequence | Code                      | Purpose           |
| -------- | ------------------------- | ----------------- |
| DECSTBM  | `\x1b[{top};{bottom}r`    | Set scroll region |
| CUP      | `\x1b[{row};{col}H`       | Position cursor   |
| ED       | `\x1b[J`                  | Erase in display  |
| SU/SD    | `\x1b[{n}S` / `\x1b[{n}T` | Scroll up/down    |

### Pros and Cons

**Pros:**

- Native Rust, no FFI or external dependencies
- Built-in scrollback management with `insert_before()`
- Works with existing ratatui widgets
- Handles resize properly
- Text selection works (terminal's native selection)
- Maintains terminal history

**Cons:**

- Viewport height is fixed at creation (can recreate to resize)
- Some widgets designed for fullscreen may need adjustment
- Limited to rectangular regions

## Ink (Claude Code, Codex, Gemini CLI)

Ink is a React renderer for CLIs used by all major AI coding agents.

### Render Modes

```typescript
import { render } from 'ink';

// Default mode - replaces previous output
const instance = render(<App />);

// Debug mode - each update is separate output (scrollback preserved)
const instance = render(<App />, {
    debug: true,  // Separate outputs, no replacement
});

// Incremental rendering (Ink 6+) - only updates changed lines
const instance = render(<App />, {
    incrementalRendering: true,  // Reduces flicker
    maxFps: 30,                  // Throttle updates
});
```

### Key Options

```typescript
type RenderOptions = {
  stdout?: NodeJS.WriteStream;
  stdin?: NodeJS.ReadStream;
  debug?: boolean; // Don't replace previous output
  exitOnCtrlC?: boolean;
  patchConsole?: boolean; // Prevent console.log mixing
  maxFps?: number; // Default: 30
  incrementalRendering?: boolean; // Only update changed lines
};
```

### How Ink Achieves Inline Feel

1. **Cursor management**: Moves cursor up to redraw previous lines
2. **Clear and rewrite**: Uses `\x1b[{n}A` (cursor up) and `\x1b[J` (clear to end)
3. **No alternate screen**: Never enters alternate buffer by default
4. **Incremental mode**: Calculates diff and updates only changed lines

### Example Pattern from Codex

```typescript
// From codex-cli/src/components/chat/terminal-chat.tsx pattern
function TerminalChat({ items, onSubmit }) {
    return (
        <Box flexDirection="column">
            {/* Message history - grows with scrollback */}
            <Static items={items.filter(i => i.finalized)}>
                {item => <MessageItem key={item.id} item={item} />}
            </Static>

            {/* Active/streaming content */}
            {items.filter(i => !i.finalized).map(item => (
                <MessageItem key={item.id} item={item} />
            ))}

            {/* Input area - stays at bottom */}
            <InputArea onSubmit={onSubmit} />
        </Box>
    );
}
```

The `<Static>` component is key - it marks content that shouldn't be re-rendered, effectively becoming scrollback.

### Pros and Cons

**Pros:**

- Declarative React model
- Rich ecosystem of components
- Used by major AI tools (battle-tested)
- Good text selection support
- Incremental rendering reduces flicker

**Cons:**

- JavaScript/TypeScript only
- Higher memory usage than native solutions
- FPS capped (30 default, can increase)
- React overhead for simple UIs

## pi-mono TUI

Custom TypeScript TUI library with low-level terminal control.

### Architecture

```
packages/tui/src/
  tui.ts         - Core TUI class, rendering loop
  terminal.ts    - Terminal abstraction (stdin/stdout)
  components/    - UI components (Box, Text, Editor, etc.)
  keys.ts        - Keyboard input handling (Kitty protocol)
```

### Key Concepts

```typescript
// From tui.ts exports
export {
  TUI, // Main TUI class
  Container, // Component container
  CURSOR_MARKER, // Cursor positioning marker
  Component, // Base component type
  Focusable, // Focus management
  OverlayHandle, // Overlay/popup support
};
```

### Rendering Approach

pi-mono uses direct ANSI escape sequences:

1. **Cursor positioning**: `\x1b[{row};{col}H` to place cursor
2. **Line clearing**: `\x1b[K` to clear line content
3. **Scroll regions**: `\x1b[{top};{bottom}r` (DECSTBM) for fixed regions
4. **Content writing**: Direct stdout writes with styling

### Input Handling

Supports Kitty keyboard protocol for enhanced key events:

```typescript
// From keys.ts
export {
  Key,
  parseKey,
  matchesKey,
  isKeyRepeat,
  isKeyRelease,
  isKittyProtocolActive,
};
```

### Pros and Cons

**Pros:**

- Full control over terminal
- Minimal dependencies
- Efficient direct rendering
- Kitty protocol support

**Cons:**

- TypeScript only
- Must handle many edge cases
- Less portable than abstraction libraries
- More code to maintain

## Terminal Escape Sequences Reference

### Scroll Region (DECSTBM)

```
CSI Pt ; Pb r
```

- `Pt` = top line (1-based)
- `Pb` = bottom line (defaults to screen height)

**Behavior:**

- Content outside scroll region is not affected by scrolling
- Cursor moves to (1,1) after setting region
- Reset with `\x1b[r` (full screen)

### Usage Pattern for Inline UI

```
1. Query terminal size
2. Calculate viewport position (e.g., bottom N lines)
3. Set scroll region: \x1b[1;{screen_height - N}r
4. Write scrolling content in scroll region
5. Move cursor below scroll region for fixed UI
6. Reset scroll region before exit: \x1b[r
```

### Common Sequences

| Purpose                | Sequence        | Notes            |
| ---------------------- | --------------- | ---------------- |
| Set scroll region      | `\x1b[{t};{b}r` | DECSTBM          |
| Reset scroll region    | `\x1b[r`        | Full screen      |
| Cursor position        | `\x1b[{r};{c}H` | CUP              |
| Cursor up              | `\x1b[{n}A`     | CUU              |
| Cursor down            | `\x1b[{n}B`     | CUD              |
| Scroll up              | `\x1b[{n}S`     | SU               |
| Scroll down            | `\x1b[{n}T`     | SD               |
| Clear to end of screen | `\x1b[J`        | ED               |
| Clear line             | `\x1b[K`        | EL               |
| Save cursor            | `\x1b[s`        | DECSC            |
| Restore cursor         | `\x1b[u`        | DECRC            |
| Alternate screen ON    | `\x1b[?1049h`   | Avoid for inline |
| Alternate screen OFF   | `\x1b[?1049l`   | -                |

## Implementation Recommendation for ion

### Option 1: Use ratatui Viewport::Inline (Recommended)

```rust
// src/tui/mod.rs
use ratatui::{Terminal, TerminalOptions, Viewport};
use crossterm::terminal;

pub struct InlineTui {
    terminal: Terminal<CrosstermBackend<Stderr>>,
    viewport_height: u16,
}

impl InlineTui {
    pub fn new(height: u16) -> Result<Self> {
        // Don't enter alternate screen
        terminal::enable_raw_mode()?;

        let backend = CrosstermBackend::new(io::stderr());
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;

        Ok(Self { terminal, viewport_height: height })
    }

    /// Push content to scrollback (above viewport)
    pub fn push_to_history(&mut self, content: impl Widget) -> Result<()> {
        self.terminal.insert_before(1, |buf| {
            content.render(buf.area, buf);
        })
    }

    /// Draw in the viewport area
    pub fn draw(&mut self, f: impl FnOnce(&mut Frame)) -> Result<()> {
        self.terminal.draw(f)?;
        Ok(())
    }
}

impl Drop for InlineTui {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}
```

### Option 2: Hybrid Approach

Keep alternate screen for fullscreen mode, add inline mode option:

```rust
pub enum TuiMode {
    Fullscreen,           // Current behavior
    Inline { height: u16 }, // New inline mode
}

impl Tui {
    pub fn new(mode: TuiMode) -> Result<Self> {
        match mode {
            TuiMode::Fullscreen => {
                execute!(stderr(), EnterAlternateScreen)?;
                // ... existing setup
            }
            TuiMode::Inline { height } => {
                // No alternate screen
                let options = TerminalOptions {
                    viewport: Viewport::Inline(height),
                };
                // ... inline setup
            }
        }
    }
}
```

### Key Design Decisions

1. **Use stderr for output** - Allows stdout for piping
2. **No alternate screen in inline mode** - Preserves scrollback
3. **Fixed viewport height** - Simplifies layout, can be configurable
4. **`insert_before` for history** - Messages flow up naturally
5. **Input area at bottom** - Familiar chat interface pattern

### Text Selection

With inline viewport:

- Terminal's native text selection works
- No special handling needed
- Mouse capture optional (can disable for selection)

## References

- [ratatui Inline Example](https://ratatui.rs/examples/apps/inline/)
- [ratatui Viewport docs](https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html)
- [Ink GitHub](https://github.com/vadimdemedes/ink)
- [pi-mono TUI](https://github.com/badlogic/pi-mono/tree/main/packages/tui)
- [DECSTBM Reference](https://vt100.net/docs/vt510-rm/DECSTBM.html)
- [Ghostty Terminal Docs](https://ghostty.org/docs/vt/csi/decstbm)
- [XTerm Control Sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)

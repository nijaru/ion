# Terminal User Interface State of the Art 2026

**Research Date:** 2026-01-26
**Purpose:** Comprehensive survey of TUI technologies for chat/agent applications
**Focus:** Rust crates, inline viewport rendering, terminal rendering techniques

---

## Executive Summary

| Aspect               | Current State                                             |
| -------------------- | --------------------------------------------------------- |
| Dominant Rust TUI    | ratatui (de facto standard, successor to tui-rs)          |
| Inline viewport      | Partially solved; resize handling remains problematic     |
| Best reference       | Codex CLI (Rust/ratatui with custom terminal management)  |
| Synchronized output  | Widely supported (Ghostty, Kitty, Windows Terminal, Warp) |
| pi-mono architecture | Sound but TypeScript-specific; not using Zig FFI          |

**Key finding:** There is no purpose-built library for "inline viewport with scrollback." The problem must be solved at the application level through scroll regions (DECSTBM) and custom viewport management.

---

## 1. Rust TUI Crates Landscape

### Primary Choices

| Crate         | Style          | Inline Support     | Notes                               |
| ------------- | -------------- | ------------------ | ----------------------------------- |
| **ratatui**   | Immediate mode | `Viewport::Inline` | Most popular, active development    |
| **cursive**   | Declarative    | Limited            | Event-loop managed, less flexible   |
| **iocraft**   | React-like     | Yes                | Newer, declarative API              |
| **termwiz**   | Low-level      | Full control       | Powers WezTerm, backend for ratatui |
| **crossterm** | Raw terminal   | Full control       | Backend used by ratatui             |

### Ratatui (Primary Choice)

[github.com/ratatui/ratatui](https://github.com/ratatui/ratatui)

The successor to tui-rs, ratatui is the dominant Rust TUI framework. Key features:

- **Immediate mode rendering** - Application controls when/what to render
- **Multiple backends** - crossterm (default), termion, termwiz
- **Rich widget ecosystem** - Tables, lists, charts, sparklines, canvas
- **Inline viewport** - `Viewport::Inline(height)` for non-fullscreen rendering
- **`scrolling-regions` feature** - Flicker-free `insert_before()` for inline mode

**Inline Viewport Limitations** (as of v0.29):

- Fixed height at creation (`Viewport::Inline(8)`)
- [PR #1964](https://github.com/ratatui/ratatui/pull/1964) adds `set_viewport_height()` but not merged
- [Issue #2086](https://github.com/ratatui/ratatui/issues/2086) documents horizontal resize problems
- Content pushed to scrollback on terminal reflow during resize

### Cursive (Alternative)

[github.com/gyscos/cursive](https://github.com/gyscos/cursive)

Declarative TUI library with event-loop management:

- **Pros**: Easier to reason about, automatic event handling
- **Cons**: Less control, not ideal for inline rendering
- **Best for**: Form-based UIs, dialog applications

### iocraft (Newer Alternative)

[github.com/ccbrown/iocraft](https://github.com/ccbrown/iocraft)

React-like declarative TUI in Rust:

- **Style**: Component-based like React/Ink
- **Layout**: Flexbox via taffy
- **Ideal for**: Developers familiar with React patterns
- **Maturity**: Newer, smaller ecosystem

### Termwiz (Low-Level)

[github.com/wez/wezterm/tree/main/termwiz](https://github.com/wez/wezterm/tree/main/termwiz)

Terminal abstraction library from WezTerm:

- Powers WezTerm terminal emulator
- Available as ratatui backend (`ratatui-termwiz`)
- Deep terminal feature support (sixel, hyperlinks, true color)
- Full control over escape sequences
- Best for: Custom terminal behavior, advanced features

### Which is Best for Inline Rendering?

**Raw crossterm** offers the most control for inline viewport with scrollback. Applications like Codex CLI use ratatui for widgets but bypass `Viewport::Inline` with custom terminal management.

---

## 2. Notable Chat/Agent TUIs

### Aider (Python)

[github.com/Aider-AI/aider](https://github.com/Aider-AI/aider)

Python-based AI pair programmer:

- **TUI Approach**: Uses prompt_toolkit (implied from Python CLI patterns)
- **Interface**: Command prompt with git integration
- **Rendering**: Standard terminal output, no fullscreen TUI
- **Scrollback**: Native terminal scrollback (simple approach)

**Key insight**: Aider succeeds with minimal TUI complexity - just prompts and text output. No custom viewport management.

### Goose (Rust) - Block

[github.com/block/goose](https://github.com/block/goose)

Rust-based AI agent framework:

- **Architecture**: CLI + Electron desktop
- **Crate structure**: goose-cli, goose-server (goosed), goose-mcp
- **TUI**: Not specified, likely minimal CLI output
- **MCP**: Native support as core feature

### AIChat (Rust)

[github.com/sigoden/aichat](https://github.com/sigoden/aichat)

Popular Rust CLI for LLM chat:

- **Features**: 20+ providers, Shell Assistant, Chat-REPL, RAG
- **TUI**: Tab autocompletion, multi-line input, history search
- **Rendering**: Likely prompt_toolkit-style REPL, not fullscreen TUI
- **Theming**: Custom dark/light themes

### Codex CLI (Rust) - OpenAI

[github.com/openai/codex](https://github.com/openai/codex)

**The most relevant reference for ion.** See [codex-tui-analysis.md](./codex-tui-analysis.md).

Key architecture decisions:

- **Framework**: ratatui + crossterm
- **Mode**: Inline viewport (primary), alternate screen (overlays only)
- **Scrollback**: `insert_history_lines()` with scroll regions (DECSTBM)
- **Key feature**: `scrolling-regions` ratatui feature for flicker-free insertion
- **TUI2 experiment**: Abandoned fullscreen approach - "terminal-native" won

**Why TUI2 was removed** (PR #9640):

> "terminal functionality that works uniformly everywhere trumps sophisticated but environment-specific features"

### Cursor CLI

[cursor.com/cli](https://cursor.com/cli)

Released August 2025, terminal interface for Cursor Agent:

- **Connection**: Links to Cursor IDE ecosystem
- **Features**: Share rules + MCP with IDE
- **Interface**: Standard CLI prompts, not fullscreen TUI
- **Status**: Beta

### Continue CLI (cn)

[docs.continue.dev/guides/cli](https://docs.continue.dev/guides/cli)

TypeScript-based CLI coding agent:

- **Features**: Session management, pause/resume, git branch display
- **Headless mode**: Works without TTY for IDE integration
- **Rendering**: Standard CLI output with tips system
- **Architecture**: Separate from IDE extension

### Claude Code (Anthropic)

TypeScript with custom React renderer (originally Ink-based):

- **Rendering**: Incremental rendering, `<Static>` for scrollback content
- **Known issues**: Terminal resize causes content loss (#18493)
- **Approach**: Inline with cursor repositioning for dynamic content

### Gemini CLI (Google)

[github.com/google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli)

- **Default**: Alternate screen buffer
- **Exit**: Prints transcript to scrollback
- **Philosophy**: Clean TUI during use, history preserved on exit

---

## 3. Terminal Rendering Best Practices

### Synchronized Output (CSI ? 2026)

Prevents tearing by buffering output until frame complete:

```
\x1b[?2026h    # Begin synchronized update
... rendering ...
\x1b[?2026l    # End synchronized update
```

**Terminal Support (2025-2026):**
| Terminal | Version | Status |
|----------|---------|--------|
| Ghostty | 1.0.0+ | Supported |
| Kitty | Recent | Supported |
| Windows Terminal | 2025-12+ | Supported |
| Warp | 0.2025.01+ | Supported |
| Contour | Current | Supported |
| iTerm2 | Current | Supported |

**Feature detection:**

```
CSI ? 2026 $ p  # Query mode status
```

Response `CSI ? 2026 ; 0 $ y` = not supported.

[Terminal Spec: Synchronized Output](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)

### Scroll Regions (DECSTBM)

Essential for inline viewport with scrollback insertion:

```
\x1b[{top};{bottom}r    # Set scroll region (1-indexed, inclusive)
\x1b[r                  # Reset to full screen
```

[Ghostty DECSTBM Docs](https://ghostty.org/docs/vt/csi/decstbm)

**How Codex uses scroll regions:**

1. Set scroll region to area above viewport
2. Move cursor to region
3. Insert lines (region scrolls up)
4. Reset scroll region
5. Restore cursor to viewport

```rust
// From Codex insert_history_lines()
queue!(writer, SetScrollRegion(1..area.top()))?;
queue!(writer, MoveTo(0, cursor_top))?;
// ... write lines ...
queue!(writer, ResetScrollRegion)?;
queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
```

### Frame Diffing Techniques

**Ratatui approach:**

- Double-buffered rendering (current vs next frame)
- Cell-by-cell comparison
- Only emit ANSI for changed cells
- Cursor movement optimization (skip unchanged regions)

**OpenTUI approach (Zig):**

- Cell array comparison in native code
- Run-length encoding for adjacent identical styles
- ANSI sequence batching
- Claims sub-millisecond frame times

**Best practices:**

1. Track previous frame content
2. Find first changed line for partial updates
3. Use `Clear::UntilNewLine` instead of full screen clear
4. Batch terminal write operations
5. Use synchronized output to prevent tearing

### Width Calculation

Critical for avoiding overflow and line wrapping issues:

```rust
// Unicode-aware width (not byte length)
let width = unicode_width::UnicodeWidthStr::width(text);

// Handle ANSI escape sequences
let visible_width = strip_ansi_escapes(text).width();

// Grapheme clusters (emoji, CJK)
use unicode_segmentation::UnicodeSegmentation;
let graphemes = text.graphemes(true);
```

---

## 4. Pi-Mono Analysis

[github.com/badlogic/pi-mono](https://github.com/badlogic/pi-mono)

### Architecture

**Pure TypeScript** - contrary to initial assumption, pi-mono does NOT use Zig FFI. The TUI is implemented entirely in TypeScript.

| Package                         | Purpose                        |
| ------------------------------- | ------------------------------ |
| `@mariozechner/pi-tui`          | Terminal rendering, components |
| `@mariozechner/pi-ai`           | LLM provider abstraction       |
| `@mariozechner/pi-agent`        | Agent loop, tool execution     |
| `@mariozechner/pi-coding-agent` | CLI application                |

### Why Pi-Mono is Notable

1. **Scrollback-native inline rendering** - Not alternate screen
2. **Differential rendering** - Three-strategy approach
3. **Content-first height** - Components return line arrays
4. **No fixed viewport** - Content determines rendered area
5. **Synchronized output** - Uses CSI 2026

### Rendering Strategies

```
Strategy 1 (Initial):      Output all lines, no clear
Strategy 2 (Width Change): \x1b[3J\x1b[2J\x1b[H + full repaint
Strategy 3 (Incremental):  Move to first changed line, clear to EOS, render diff
```

### Key Limitation

> "If the first changed line is above the visible viewport (user scrolled up), we have to do a full clear and re-render. The terminal doesn't let you write to the scrollback buffer above the viewport."

### Tradeoffs vs Other Approaches

| Aspect         | Pi-Mono        | Codex (ratatui) | OpenTUI (Zig) |
| -------------- | -------------- | --------------- | ------------- |
| Language       | TypeScript     | Rust            | TS + Zig      |
| Inline mode    | Primary        | Primary         | Secondary     |
| Fixed viewport | No             | Custom          | Yes           |
| Performance    | Good           | Excellent       | Excellent     |
| Ecosystem      | Node.js        | Rust crates     | Custom        |
| Portability    | Cross-platform | Cross-platform  | Limited       |

### Is TypeScript+Zig FFI Common?

**No, this is unusual.** OpenTUI (SST) uses it, but pi-mono does not. The pattern exists but is rare:

- **OpenTUI**: TypeScript API + Zig rendering core via Bun.dlopen()
- **Most projects**: Single-language implementation
- **Tradeoff**: Performance vs build complexity

---

## 5. Inline Viewport with Scrollback Solutions

### The Problem

Applications need:

1. Dynamic content area (streaming responses, tool output)
2. Fixed input area at bottom
3. Completed content pushed to terminal scrollback
4. Native scroll/search in scrollback
5. Proper resize handling

No library solves this completely. It requires application-level implementation.

### Approaches

#### Option A: Alternate Screen with Exit Transcript (Gemini CLI)

```rust
// Use fullscreen during session
enter_alternate_screen();

// ... TUI operations ...

// Print transcript on exit
leave_alternate_screen();
for msg in transcript {
    println!("{}", format_message(msg));
}
```

**Pros**: Clean implementation, no resize issues
**Cons**: No scrollback search during session

#### Option B: Custom Terminal Management (Codex CLI - Recommended)

```rust
// Enable scrolling-regions feature
ratatui = { features = ["scrolling-regions"] }

// Custom terminal wrapper (not Viewport::Inline)
let mut terminal = CustomTerminal::new();
terminal.set_viewport_area(Rect { ... });

// Insert to scrollback using scroll regions
insert_history_lines(&mut terminal, completed_lines);
```

**Pros**: Native scrollback, flicker-free insertion
**Cons**: More complex, resize heuristics needed

#### Option C: Raw Crossterm + Ratatui Widgets

Bypass ratatui's viewport entirely:

```rust
// Chat history: print directly
for line in chat_lines {
    println!("{}", line);
}

// Status area: render to buffer, position manually
let mut buf = Buffer::empty(status_rect);
render_widgets(&mut buf);
execute!(stdout, MoveTo(0, term_height - status_height))?;
output_buffer(&buf);
```

**Pros**: Full control, pi-mono-like flexibility
**Cons**: Lose viewport abstraction

#### Option D: Accept Fixed Viewport Limitations

```rust
let terminal = ratatui::init_with_options(TerminalOptions {
    viewport: Viewport::Inline(8), // Small fixed size
});

// Accept some visual imperfection on resize
// Use insert_before() for completed content
```

**Pros**: Simplest implementation
**Cons**: Gaps during resize, height not dynamic

### Recommendation for Ion

**Option B (Custom Terminal Management)** based on Codex CLI approach:

1. Enable `scrolling-regions` ratatui feature
2. Create custom terminal wrapper bypassing `Viewport::Inline`
3. Implement `insert_history_lines()` with DECSTBM
4. Use synchronized output (`CSI ? 2026`) for flicker prevention
5. Track cursor position for resize heuristics

---

## Key Takeaways

1. **No magic library exists** - Inline viewport with scrollback requires custom implementation

2. **Codex CLI is the reference** - Their approach (scroll regions + custom viewport) is proven

3. **Synchronized output is widely supported** - Use CSI 2026 for flicker prevention

4. **Pi-mono's architecture is sound** - But TypeScript-specific, not using Zig FFI

5. **Ratatui's inline viewport has limits** - Fixed height, resize issues not fully solved

6. **Terminal-native approach wins** - Codex abandoned fullscreen TUI2 for terminal-native behavior

7. **Simple approaches work** - Aider succeeds with minimal TUI complexity

---

## Sources

### Crates and Libraries

- [ratatui](https://github.com/ratatui/ratatui) - Primary Rust TUI framework
- [iocraft](https://github.com/ccbrown/iocraft) - React-like Rust TUI
- [cursive](https://github.com/gyscos/cursive) - Declarative Rust TUI
- [termwiz](https://github.com/wez/wezterm/tree/main/termwiz) - WezTerm's terminal library
- [OpenTUI](https://github.com/sst/opentui) - TypeScript + Zig TUI (SST)

### Agent TUIs

- [Codex CLI](https://github.com/openai/codex) - OpenAI's Rust agent
- [Pi-Mono](https://github.com/badlogic/pi-mono) - Mario Zechner's TypeScript agent
- [Aider](https://github.com/Aider-AI/aider) - Python AI pair programmer
- [AIChat](https://github.com/sigoden/aichat) - Rust LLM CLI
- [Goose](https://github.com/block/goose) - Block's Rust agent
- [Gemini CLI](https://github.com/google-gemini/gemini-cli) - Google's CLI

### Terminal Standards

- [Synchronized Output Spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)
- [DECSTBM Docs (Ghostty)](https://ghostty.org/docs/vt/csi/decstbm)
- [VT Extensions](https://github.com/contour-terminal/vt-extensions)

### Prior Research

- [codex-tui-analysis.md](./codex-tui-analysis.md) - Codex approach details
- [pi-mono-tui-analysis.md](./pi-mono-tui-analysis.md) - Pi-mono implementation
- [inline-viewport-scrollback-2026.md](./inline-viewport-scrollback-2026.md) - Problem analysis
- [tui-agents-comparison-2026.md](./tui-agents-comparison-2026.md) - Agent comparison

# Input Library Evaluation

**Date:** 2026-01-26
**Task:** tk-svho (Spike: Evaluate input libraries for TUI)

## Summary

| Library         | Multi-line          | Concurrent Output         | Status          |
| --------------- | ------------------- | ------------------------- | --------------- |
| rustyline-async | **NO**              | SharedWriter (production) | Not viable      |
| reedline        | **YES** (Validator) | ExternalPrinter (stable)  | **Recommended** |

## rustyline-async

**Verdict: NOT VIABLE** - Single-line only.

From the source code:

> "The user entering the line can edit their input using the key bindings listed for navigation within the current input line (singular)."

We need multi-line for user prompts.

## reedline

**Verdict: RECOMMENDED** - Has both features we need.

### Multi-line Support

Via `Validator` trait:

```rust
struct MultiLineValidator;

impl Validator for MultiLineValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if line.ends_with('\\') || line.ends_with('{') {
            ValidationResult::Incomplete  // Continue to next line
        } else {
            ValidationResult::Complete
        }
    }
}
```

### Concurrent Output

Via `ExternalPrinter`:

```rust
let printer = ExternalPrinter::default();
let mut editor = Reedline::create()
    .with_external_printer(printer.clone());

// From another thread/task:
printer.print("Message appears above input".to_string())?;
```

### Additional Features

- History with search (Ctrl+R)
- Syntax highlighting (Highlighter trait)
- Fish-style suggestions (Hinter trait)
- Vi and Emacs keybindings
- Clipboard integration (with feature)

## Architecture Question

**What reedline provides:**

- Multi-line input editing
- History management
- Concurrent output above input line
- Customizable prompt

**What reedline doesn't provide:**

- ratatui widget integration
- Our selector UI (model picker, etc.)
- Progress spinners, token counts display
- Status line below input

### Options for Status/Selectors

**Option A: Prompt-based status**

- Put model name, token count in the prompt itself
- Simple text menus for selectors
- Minimal change to reedline model

```
[gpt-4o · 1.2k tokens] >
```

**Option B: Hybrid reedline + raw crossterm**

- Use reedline for input/output
- Use raw crossterm to draw status line below
- May have coordination issues

**Option C: Hybrid reedline + ratatui overlays**

- Use reedline for main interaction
- Switch to ratatui fullscreen for selectors
- Clean separation but mode switching

**Option D: Stay with custom ratatui**

- Keep current approach but fix viewport issues
- Codex-style custom terminal wrapper
- More work but full control

## Recommendation

**Start with Option A (prompt-based status)** because:

1. Simplest integration
2. reedline's prompt is quite customizable
3. Can add complexity later if needed
4. Selectors can be text-based (like fzf)

If prompt-based doesn't work, fall back to **Option D** (custom ratatui with scroll regions).

## Spike Examples

Created in `examples/`:

- `rustyline_spike.rs` - Demonstrates rustyline-async (single-line only)
- `reedline_spike.rs` - Demonstrates reedline multi-line + external_printer

Run with:

```bash
cargo run --example reedline_spike
```

## Next Steps

1. Test reedline spike manually
2. Evaluate prompt customization for status display
3. Prototype selector UI (text-based or fullscreen overlay)
4. If viable, plan migration from current ratatui TUI

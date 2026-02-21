# TUI Render Layout Review — 2026-02-20

Scope: `src/tui/render/selector.rs`, `src/tui/render/bottom_ui.rs`
Focus: column layout budgets, status line width arithmetic, border fills, cursor/scroll math.

---

## Correctness/Safety Issues

### [ERROR] bottom_ui.rs:158–187 — `calculate_input_height` uses different wrap width than rendering

`calculate_input_height` computes line count with:

```rust
let text_width = viewport_width
    .saturating_sub(BORDER_OVERHEAD)   // -2 (labeled "top and bottom borders" — wrong axis)
    .saturating_sub(LEFT_MARGIN + RIGHT_MARGIN) as usize;  // -4
// → text_width = viewport_width - 6
```

`input_lines_for_height` renders with:

```rust
let content_width = width.saturating_sub(INPUT_MARGIN) as usize;
// INPUT_MARGIN = 3 → content_width = width - 3
```

The two use different widths: `width - 6` vs `width - 3`. Because `width - 6 < width - 3`, `calculate_input_height` wraps text earlier, sees more visual lines, and returns a taller input box than the rendering actually needs. On a narrow terminal (e.g., 40 columns), the discrepancy is 3 columns, which can produce 1–2 extra rows in the height estimate. The box is oversized; blank content lines appear at the bottom.

The `BORDER_OVERHEAD = 2` subtracted from width is a category error: the comment says "top and bottom borders" but those are row overhead, not column overhead. The content rows have no side borders (`│`), so no column subtraction for borders is needed.

Fix — align `calculate_input_height` to use the same margin as rendering:

```rust
// Before
const BORDER_OVERHEAD: u16 = 2;
const LEFT_MARGIN: u16 = 3;
const RIGHT_MARGIN: u16 = 1;
let text_width = viewport_width
    .saturating_sub(BORDER_OVERHEAD)
    .saturating_sub(LEFT_MARGIN + RIGHT_MARGIN) as usize;

// After — match INPUT_MARGIN used in input_lines_for_height
use crate::tui::render::INPUT_MARGIN;
let text_width = viewport_width.saturating_sub(INPUT_MARGIN) as usize;
```

Or if the right-margin protection is intentional:

```rust
let text_width = viewport_width.saturating_sub(INPUT_MARGIN + 1) as usize;
// (INPUT_MARGIN=3 already includes 1 right-pad; +1 is the explicit RIGHT_MARGIN)
```

Either way, remove the BORDER_OVERHEAD (= 2) width subtraction.

---

### [WARN] selector.rs:283 — column header padding uses format-width (char count) not display width

```rust
let max_label_width = data.items.iter()
    .map(|item| display_width(&item.label))
    .max()
    .unwrap_or(0);

// column header row:
let padded = format!("{label_h:<max_label_width$}");
push_clipped_span(&mut spans, &padded, &mut remaining, None, false, true);
```

`max_label_width` is in display cells (from `display_width()`). `format!("{:<N$}", s)` pads by _character count_ (Rust's format width counts Unicode scalar values). For ASCII-only labels (current usage: model names, provider names) these are equal. If labels ever contain multi-byte chars, the header column will misalign.

Item rows correctly compute padding as `" ".repeat(max_label_width - label_w)` using display-width subtraction — this is consistent. The header is the odd one out.

Fix:

```rust
// Before
let padded = format!("{label_h:<max_label_width$}");

// After
let header_w = display_width(label_h);
let header_pad = if header_w < max_label_width {
    " ".repeat(max_label_width - header_w)
} else {
    String::new()
};
let padded = format!("{label_h}{header_pad}");
```

---

### [WARN] bottom_ui.rs:354–371 — status line segment widths use `.len()` (bytes) not `display_width()`

All segment width variables (`mode_w`, `think_w`, `model_seg`, `think_seg`, `pct_seg`, `detail_extra`, `cost_seg`, `branch_extra`, `diff_extra`, `proj_seg`) are computed with `.len()`:

```rust
let mode_w = mode_label.len() + 3;
let model_seg = 3 + model_name.len() + think_w;
let proj_seg = 3 + project.len();
let branch_extra = branch.map_or(0, |b| 3 + b.len());
// etc.
```

For ASCII-only values this is correct (`.len()` == display cells). Current values are all ASCII in practice: mode labels ("READ", "WRITE"), token percentages, cost strings, model IDs, branch names. However:

- `project` is a filesystem directory name — can be non-ASCII (e.g. `~/проекты/ion`)
- `branch` is a git branch name — can be non-ASCII
- `model_name` from `session.model.split('/').next_back()` — unlikely but possible

If a non-ASCII value appears, the width estimate is wrong (too small for multi-byte, too large for wide chars), causing wrong drop-level decisions. The status line is already guarded by `paint_row_spans` truncation, so no crash — just wrong drop behavior.

Given these fields come from user-controlled data (working directory, git branch), this is a real hazard. For the drop-level arithmetic to be correct, replace `.len()` with `display_width()` for project, branch, and model_name:

```rust
let model_seg = 3 + display_width(model_name) + think_w;
let proj_seg = 3 + display_width(project);
let branch_extra = branch.map_or(0, |b| 3 + display_width(b));
```

---

## Quality/Refactoring Issues

### [WARN] bottom_ui.rs:354 — misleading constant name and comment

```rust
// Before
const BORDER_OVERHEAD: u16 = 2; // Top and bottom borders
// Used as: viewport_width.saturating_sub(BORDER_OVERHEAD)

// After (if the -2 is kept with correct intent, e.g. right margin):
const COLUMN_MARGIN: u16 = 4; // PROMPT_WIDTH(2) + right_pad(1) + safety(1)
let text_width = viewport_width.saturating_sub(COLUMN_MARGIN) as usize;
```

The existing constant name conflates row overhead with column margin, making the width formula opaque.

---

### [NIT] selector.rs:100–107 — `push_clipped_span` early return on empty clip

```rust
fn push_clipped_span(...) {
    if *remaining == 0 {
        return;
    }
    let clipped = truncate_to_display_width(text, *remaining);
    if clipped.is_empty() {
        return;   // ← exits without decrementing remaining
    }
    *remaining = remaining.saturating_sub(display_width(&clipped));
    ...
}
```

When `clipped` is empty (input was empty string or all zero-width chars), the function returns early without touching `*remaining`. This is correct — nothing was emitted, nothing to deduct. But the empty-text case comes up on separator spans like `"  "` being clipped to nothing (e.g. `remaining = 1` and input is two spaces). The first space would be emitted with `remaining` going to 0, then the second `push_clipped_span(" ")` hits the `remaining == 0` guard cleanly. No issue, just worth noting that the two early-return paths are intentionally different.

---

### [NIT] bottom_ui.rs:494–500 — `scroll_to_cursor` order dependency on stale `cursor_pos`

```rust
if content_width > 0 {
    self.input_state.calculate_cursor_pos_with(..., content_width);
}
let total_lines = ...;
self.input_state.scroll_to_cursor(visible_height, total_lines);
```

`scroll_to_cursor` reads `self.cursor_pos.1` which was set by `calculate_cursor_pos_with`. If `content_width == 0`, the cursor position is not updated before scrolling. On a width-0 terminal `total_lines` is 1 and `scroll_to_cursor` clamps offset to 0 anyway, so this is benign in practice. A guard or comment noting the dependency would prevent future confusion:

```rust
if content_width > 0 {
    self.input_state.calculate_cursor_pos_with(..., content_width);
    let total_lines = ...;
    self.input_state.scroll_to_cursor(visible_height, total_lines);
}
```

---

## Summary

| ID  | Severity | File             | Description                                                                        |
| --- | -------- | ---------------- | ---------------------------------------------------------------------------------- |
| 1   | ERROR    | bottom_ui.rs:158 | Wrap width mismatch: height calc uses `width-6`, render uses `width-3`             |
| 2   | WARN     | selector.rs:283  | Header padding uses format char-count, items use display_width                     |
| 3   | WARN     | bottom_ui.rs:354 | Status width arithmetic uses `.len()` for user-controlled fields (project, branch) |
| 4   | WARN     | bottom_ui.rs:158 | Misleading `BORDER_OVERHEAD` constant name and axis                                |
| 5   | NIT      | bottom_ui.rs:191 | `scroll_to_cursor` depends on cursor_pos set by preceding call                     |

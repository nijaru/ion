# Chat Rendering Enhancements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add strikethrough text, task list checkboxes, OSC 8 clickable hyperlinks, and a visual token bar to ion's chat and status rendering; fix `display_width` consistency in `table.rs` and `direct.rs`.

**Architecture:** All chat output goes through `StyledSpan` → `StyledLine` → `write_to_width`. Adding new attributes means extending `StyledSpan` and updating `to_rnk_span` or `write_to`. Markdown features go in `src/tui/highlight/markdown.rs` using pulldown-cmark events. The token bar is a new helper in `util.rs` used by `bottom_ui.rs`'s status line.

**Tech Stack:** Rust 2024 edition, pulldown-cmark (event-based), crossterm (terminal I/O), rnk (styled-string formatter), unicode_width.

**Key files:**

- `src/tui/terminal.rs` — `TextStyle`, `StyledSpan`, `StyledLine`, `to_rnk_span`, `write_to`/`write_to_width`
- `src/tui/highlight/markdown.rs` — `render_markdown_with_width` (big match over pulldown-cmark events)
- `src/tui/table.rs` — `measure_width`, `wrap_text`, `pad_cell`
- `src/tui/render/direct.rs` — `selector_data()` Provider and Model arms
- `src/tui/render/bottom_ui.rs` — `status_line_spans()`
- `src/tui/util.rs` — helpers; add `render_token_bar` here

**Critical invariant:** Every `StyledLine` must occupy exactly 1 terminal row when written. New features must not cause line wraps. Links, strikethrough, and task list markers don't change line count — they only change how a single line is styled or prefixed.

---

## Task 1: Strikethrough in markdown (tk-s2xv)

`TextStyle.crossed_out` already exists and `to_rnk_span` already maps it to `.strikethrough()`. Only the builder method and markdown event handler are missing.

**Files:**

- Modify: `src/tui/terminal.rs`
- Modify: `src/tui/highlight/markdown.rs`

### Step 1: Add `StyledSpan::with_strikethrough()` builder

In `src/tui/terminal.rs`, after `with_italic()` (around line 212):

```rust
/// Add strikethrough modifier to this span.
#[must_use]
pub fn with_strikethrough(mut self) -> Self {
    self.style.crossed_out = true;
    self
}
```

### Step 2: Write failing test for the builder

In `src/tui/terminal.rs`, in the existing `#[cfg(test)]` block or add one:

```rust
#[test]
fn test_strikethrough_style() {
    let span = StyledSpan::raw("test").with_strikethrough();
    assert!(span.style.crossed_out);
}
```

### Step 3: Run test to verify it passes

```bash
cargo test -q terminal 2>&1 | grep -E "FAILED|ok|error"
```

Expected: `test result: ok.`

### Step 4: Handle `Tag::Strikethrough` in markdown renderer

In `src/tui/highlight/markdown.rs`, add state variable after `in_blockquote`:

```rust
let mut in_strikethrough = false;
```

In the `Event::Start(tag)` match block:

```rust
Tag::Strikethrough => in_strikethrough = true,
```

In the `Event::End(tag_end)` match block:

```rust
TagEnd::Strikethrough => in_strikethrough = false,
```

In `Event::Text`, where the span is built (around the `in_bold && in_italic` block), apply strikethrough after building the span:

```rust
let mut span = if in_bold && in_italic {
    StyledSpan::bold(part.to_string()).with_italic()
} else if in_bold {
    StyledSpan::bold(part.to_string())
} else if in_italic || in_blockquote {
    StyledSpan::italic(part.to_string())
} else {
    StyledSpan::raw(part.to_string())
};
if in_strikethrough {
    span = span.with_strikethrough();
}
current_line = current_line.styled(span);
```

### Step 5: Write failing test for strikethrough in markdown

In `src/tui/highlight/markdown.rs` tests or as an integration test in `src/tui/table.rs` (the file has tests already; add to markdown's test module):

```rust
#[test]
fn test_strikethrough_renders() {
    let lines = render_markdown("~~deleted~~");
    assert!(!lines.is_empty());
    let has_strikethrough = lines[0].spans.iter().any(|s| s.style.crossed_out);
    assert!(has_strikethrough, "expected crossed_out span");
}
```

### Step 6: Run tests

```bash
cargo test -q 2>&1 | grep -E "FAILED|test result"
```

Expected: all pass.

### Step 7: Commit

```bash
git add src/tui/terminal.rs src/tui/highlight/markdown.rs
git commit -m "Add strikethrough support to markdown renderer"
```

---

## Task 2: Task list support (tk-xj3g)

pulldown-cmark emits `Event::TaskListMarker(checked: bool)` for `- [x]` and `- [ ]` items when `ENABLE_TASKLISTS` is set. The marker event arrives between `Tag::Item` and the item text. We want to replace the default `- ` prefix with `☑ ` or `☐ `.

**Files:**

- Modify: `src/tui/highlight/markdown.rs`

### Step 1: Write failing test

In `src/tui/highlight/markdown.rs` tests:

```rust
#[test]
fn test_task_list_checked() {
    let lines = render_markdown("- [x] done\n- [ ] todo");
    let text: String = lines.iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_str()))
        .collect();
    assert!(text.contains("☑"), "expected checked checkbox ☑");
    assert!(text.contains("☐"), "expected unchecked checkbox ☐");
}
```

Run: `cargo test test_task_list_checked` — expected: FAIL (marker not handled yet).

### Step 2: Enable `ENABLE_TASKLISTS` in parser options

In `render_markdown_with_width`, change:

```rust
// Before:
options.insert(Options::ENABLE_TABLES);

// After:
options.insert(Options::ENABLE_TABLES);
options.insert(Options::ENABLE_TASKLISTS);
```

### Step 3: Handle `Event::TaskListMarker`

Add a state variable for the pending task marker:

```rust
let mut pending_task_marker: Option<bool> = None; // Some(true)=checked, Some(false)=unchecked
```

In the main event match, add a new arm:

```rust
Event::TaskListMarker(checked) => {
    pending_task_marker = Some(checked);
}
```

In `Event::Text`, before building the span, consume the pending marker. When a task marker is pending, the item prefix has already been written as `"- "`. We need to replace that prefix line with the checkbox. The cleanest approach: when `pending_task_marker` is set and we're about to write text, replace the last span of the current line (which should be `"- "`) with the checkbox prefix:

```rust
Event::Text(text) => {
    // ... existing code above ...
    if let Some(checked) = pending_task_marker.take() {
        // Replace the "- " list prefix that was already added to current_line
        // The current_line was set to LineBuilder::new().raw("- ") in Tag::Item
        let checkbox = if checked { "☑ " } else { "☐ " };
        // Rebuild current_line with checkbox instead of "- "
        current_line = LineBuilder::new().raw(checkbox);
        current_line_is_prefix_only = true;
    }
    // ... rest of existing Event::Text handling ...
```

### Step 4: Run test

```bash
cargo test test_task_list -q 2>&1
```

Expected: PASS.

### Step 5: Run full suite

```bash
cargo test -q 2>&1 | grep -E "FAILED|test result"
```

### Step 6: Commit

```bash
git add src/tui/highlight/markdown.rs
git commit -m "Add task list checkbox support to markdown renderer"
```

---

## Task 3: OSC 8 hyperlinks (tk-vcm4)

OSC 8 escape format: `\x1b]8;;URL\x07` to open, `\x1b]8;;\x07` to close. (`\x07` = BEL). Add `url: Option<String>` to `StyledSpan`. When rendering, emit OSC 8 around the rnk-styled content.

`StyledLine::write_to` currently batches all spans into one rnk call. For spans with URLs, we need per-span rendering. Use a fast path (no URLs → existing batch) and a URL path (any URL → iterate span-by-span).

**Files:**

- Modify: `src/tui/terminal.rs`
- Modify: `src/tui/highlight/markdown.rs`

### Step 1: Add `url` field to `StyledSpan`

In `src/tui/terminal.rs`, change the `StyledSpan` struct:

```rust
/// A styled span of text.
#[derive(Clone, Debug)]
pub struct StyledSpan {
    pub content: String,
    pub style: TextStyle,
    /// Optional hyperlink URL for OSC 8 terminal hyperlinks.
    pub url: Option<String>,
}
```

Update all constructors (`raw`, `colored`, `dim`, `bold`, `italic`, `new`) to set `url: None`:

```rust
pub fn raw(content: impl Into<String>) -> Self {
    Self {
        content: content.into(),
        style: TextStyle::new(),
        url: None,
    }
}
// ... same pattern for colored, dim, bold, italic, new ...
```

### Step 2: Add `with_url()` builder method

After the `with_strikethrough()` method:

```rust
/// Attach a hyperlink URL (OSC 8) to this span.
#[must_use]
pub fn with_url(mut self, url: impl Into<String>) -> Self {
    self.url = Some(url.into());
    self
}
```

### Step 3: Write failing test for `with_url`

```rust
#[test]
fn test_span_url_field() {
    let span = StyledSpan::raw("link text").with_url("https://example.com");
    assert_eq!(span.url.as_deref(), Some("https://example.com"));
}
```

Run: `cargo test test_span_url_field` — Expected: PASS (this one compiles immediately since we're just adding a field).

### Step 4: Emit OSC 8 in `StyledSpan::write_to`

Update `StyledSpan::write_to`:

```rust
pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
    if let Some(ref url) = self.url {
        write!(w, "\x1b]8;;{url}\x07")?;
    }
    let span = to_rnk_span(self);
    let width = display_width(&self.content).max(1);
    let rendered = render_no_wrap_text_line(Text::spans(vec![span]), width);
    write!(w, "{rendered}")?;
    if self.url.is_some() {
        write!(w, "\x1b]8;;\x07")?;
    }
    Ok(())
}
```

### Step 5: Add per-span fallback path in `StyledLine::write_to`

When any span has a URL, iterate span-by-span instead of batching:

```rust
pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
    if self.spans.is_empty() {
        return Ok(());
    }
    // Per-span path when any span carries a URL (to emit OSC 8 around each)
    if self.spans.iter().any(|s| s.url.is_some()) {
        for span in &self.spans {
            span.write_to(w)?;
        }
        return Ok(());
    }
    // Existing fast path: batch all spans
    let mut spans = Vec::with_capacity(self.spans.len());
    let mut line_width = 0usize;
    for span in &self.spans {
        line_width += display_width(&span.content);
        spans.push(to_rnk_span(span));
    }
    let rendered = render_no_wrap_text_line(Text::spans(spans), line_width.max(1));
    write!(w, "{rendered}")?;
    Ok(())
}
```

Apply the same per-span fallback to `write_to_width`:

```rust
pub fn write_to_width<W: Write>(&self, w: &mut W, width: u16) -> io::Result<()> {
    if self.spans.is_empty() {
        return Ok(());
    }
    if self.spans.iter().any(|s| s.url.is_some()) {
        for span in &self.spans {
            span.write_to(w)?;
        }
        write!(w, "\r\n")?;
        return Ok(());
    }
    // ... existing code unchanged ...
```

**Note:** The per-span path in `write_to_width` skips the `max_cells` truncation. This is acceptable for hyperlinks — OSC 8 text truncation at the right edge is still handled by the terminal. If this causes issues, add manual cell-counting truncation later.

### Step 6: Handle `Tag::Link` in markdown renderer

In `src/tui/highlight/markdown.rs`, add state variable:

```rust
let mut link_url: Option<String> = None;
```

In `Event::Start(tag)` match:

```rust
Tag::Link { dest_url, .. } => {
    link_url = Some(dest_url.into_string());
}
```

In `Event::End(tag_end)` match:

```rust
TagEnd::Link => {
    link_url = None;
}
```

In `Event::Text`, after building the span and applying `in_strikethrough`, apply the URL:

```rust
if let Some(ref url) = link_url {
    span = span.with_url(url.clone());
}
current_line = current_line.styled(span);
```

### Step 7: Write test for hyperlink rendering

```rust
#[test]
fn test_link_has_url() {
    let lines = render_markdown("[click here](https://example.com)");
    assert!(!lines.is_empty());
    let link_span = lines[0].spans.iter().find(|s| s.url.is_some());
    assert!(link_span.is_some(), "expected span with url");
    assert_eq!(
        link_span.unwrap().url.as_deref(),
        Some("https://example.com")
    );
}
```

### Step 8: Run full test suite

```bash
cargo test -q 2>&1 | grep -E "FAILED|test result"
cargo clippy -q 2>&1 | grep -E "^error|^warning:"
```

Expected: all pass, clean.

### Step 9: Commit

```bash
git add src/tui/terminal.rs src/tui/highlight/markdown.rs
git commit -m "Add OSC 8 hyperlink support to terminal renderer and markdown"
```

---

## Task 4: Fix `table.rs` display_width consistency (tk-9ar3)

`table.rs` uses `unicode_width::UnicodeWidthStr::width` directly. Replace with `crate::tui::util::display_width` for consistency. The behavior is identical (both sum `UnicodeWidthChar::width` per char), so no tests should change — this is a consistency fix.

**Files:**

- Modify: `src/tui/table.rs`

### Step 1: Replace `measure_width` implementation

In `src/tui/table.rs`, change the `measure_width` function:

```rust
// Before:
fn measure_width(s: &str) -> usize {
    s.lines()
        .map(unicode_width::UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
}

// After:
fn measure_width(s: &str) -> usize {
    s.lines()
        .map(crate::tui::util::display_width)
        .max()
        .unwrap_or(0)
}
```

### Step 2: Replace `word.width()` in `wrap_text`

In `wrap_text`, replace all uses of `.width()` (from `UnicodeWidthStr`) with `crate::tui::util::display_width`:

```rust
// Before:
let word_width = word.width();
// After:
let word_width = crate::tui::util::display_width(word);

// Before:
current_width = current_line.width();
// After:
current_width = crate::tui::util::display_width(&current_line);
```

### Step 3: Replace char-level width calls in `break_word`

Keep `unicode_width::UnicodeWidthChar::width(ch)` in `break_word` — this is per-character and is the correct level of abstraction (can't use the string-level helper here).

### Step 4: Replace `content.width()` in `pad_cell`

```rust
// Before:
let content_width = content.width();
// After:
let content_width = crate::tui::util::display_width(content);
```

### Step 5: Remove now-unused imports

Check if `use unicode_width::UnicodeWidthStr;` at the top is still needed. After the replacements, it's only used by char-level calls in `break_word`. The import is actually for the trait that enables `.width()` on strings. After removing the string-level calls, check if any remain:

```bash
grep -n "\.width()" src/tui/table.rs
```

If none remain, remove `use unicode_width::UnicodeWidthStr;`. Keep `use unicode_width::UnicodeWidthChar;` for `break_word` and `render_narrow`.

### Step 6: Run existing tests

```bash
cargo test -q table 2>&1 | grep -E "FAILED|test result"
```

Expected: all pass (behavior unchanged).

### Step 7: Commit

```bash
git add src/tui/table.rs
git commit -m "Fix table.rs: use display_width from util for consistency"
```

---

## Task 5: Fix `direct.rs` selector column display_width (tk-7kqq)

`selector_data()` in `direct.rs` uses `.len()` and `format!("{:width$}", ...)` for column alignment in selector hint strings. These should use `display_width` to correctly handle non-ASCII provider/model names.

**Files:**

- Modify: `src/tui/render/direct.rs`

### Step 1: Import `display_width`

At the top of `direct.rs`, add to the existing util import:

```rust
use crate::tui::util::{
    display_width, format_context_window, format_price, format_relative_time, shorten_home_prefix,
};
```

### Step 2: Fix Provider picker (`max_id_len`)

Currently:

```rust
let max_id_len = self
    .provider_picker
    .filtered()
    .iter()
    .map(|s| s.provider.id().len())  // <-- .len() = bytes
    .max()
    .unwrap_or(0);
```

Replace with:

```rust
let max_id_len = self
    .provider_picker
    .filtered()
    .iter()
    .map(|s| display_width(s.provider.id()))
    .max()
    .unwrap_or(0);
```

Replace the `format!("{:width$}", id, ...)` padding with explicit spacing:

```rust
// Before:
format!("{:width$}", id, width = max_id_len),
// After:
format!("{}{}", id, " ".repeat(max_id_len.saturating_sub(display_width(id)))),
```

Apply the same to the `format!("{:width$}  {}", id, auth_hint, ...)` call:

```rust
// Before:
format!("{:width$}  {}", id, auth_hint, width = max_id_len),
// After:
format!("{}{}  {}", id, " ".repeat(max_id_len.saturating_sub(display_width(id))), auth_hint),
```

Also fix the column header: `"{:<max_id_len$}  Auth"` → manual padding:

```rust
let col_hint = format!(
    "{}{}  Auth",
    "ID",
    " ".repeat(max_id_len.max(2).saturating_sub(display_width("ID")))
);
```

### Step 3: Fix Model picker (`max_provider_w`, `max_in_w`, `max_out_w`)

```rust
let max_provider_w = models
    .iter()
    .map(|m| display_width(&m.provider))  // was .len()
    .max()
    .unwrap_or(3)
    .max(3);

let max_in_w = models
    .iter()
    .map(|m| format_price(m.pricing.input).len())  // ASCII prices, .len() fine
    .max()
    .unwrap_or(2)
    .max("In".len());

let max_out_w = models
    .iter()
    .map(|m| format_price(m.pricing.output).len())  // ASCII, fine
    .max()
    .unwrap_or(3)
    .max("Out".len());
```

For the hint format strings: `format!("{:<max_provider_w$} ...")` — replace with explicit padding for the provider column:

```rust
let hint = format!(
    "{}{}  {:<6}  {:<max_in_w$}  {:<max_out_w$}",
    m.provider,
    " ".repeat(max_provider_w.saturating_sub(display_width(&m.provider))),
    ctx,
    price_in,
    price_out,
    max_in_w = max_in_w,
    max_out_w = max_out_w,
);
```

Apply same to column header:

```rust
let col_hint = format!(
    "{}{}  {:<6}  {:<max_in_w$}  {:<max_out_w$}",
    "Org",
    " ".repeat(max_provider_w.saturating_sub(display_width("Org"))),
    "Ctx",
    "In",
    "Out",
    max_in_w = max_in_w,
    max_out_w = max_out_w,
);
```

### Step 4: Build and test

```bash
cargo build -q 2>&1 | grep "^error"
cargo test -q 2>&1 | grep -E "FAILED|test result"
```

### Step 5: Commit

```bash
git add src/tui/render/direct.rs
git commit -m "Fix direct.rs: use display_width for selector column alignment"
```

---

## Task 6: Visual token usage bar (tk-avmd)

Add `render_token_bar(pct: u64, bar_width: usize) -> String` to `util.rs`. Use 6-block bars with `█` (filled) and `░` (empty). Update `status_line_spans` to show `██████ 45%` instead of just `45%`.

**Files:**

- Modify: `src/tui/util.rs`
- Modify: `src/tui/render/bottom_ui.rs`

### Step 1: Write failing test for `render_token_bar`

In `src/tui/util.rs` test module:

```rust
#[test]
fn test_render_token_bar() {
    assert_eq!(render_token_bar(0, 6), "░░░░░░");
    assert_eq!(render_token_bar(100, 6), "██████");
    assert_eq!(render_token_bar(50, 6), "███░░░");
    assert_eq!(render_token_bar(33, 6), "██░░░░");
}
```

Run: `cargo test test_render_token_bar` — Expected: FAIL (function doesn't exist).

### Step 2: Implement `render_token_bar`

In `src/tui/util.rs`, add before the test module:

```rust
/// Render a compact block-char progress bar.
/// Returns `bar_width` characters using █ (filled) and ░ (empty).
pub(crate) fn render_token_bar(pct: u64, bar_width: usize) -> String {
    let filled = ((pct as usize).min(100) * bar_width / 100).min(bar_width);
    let empty = bar_width - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}
```

### Step 3: Run test

```bash
cargo test test_render_token_bar -q 2>&1
```

Expected: PASS.

### Step 4: Import and use in `status_line_spans`

In `src/tui/render/bottom_ui.rs`, add `render_token_bar` to the util imports:

```rust
use crate::tui::util::{display_width, format_cost, format_elapsed, format_tokens, render_token_bar, truncate_to_display_width};
```

In `status_line_spans`, update the `(pct_text, detail_text)` block to build a combined bar+pct string. Find the `pct_seg` calculation and update:

```rust
// Build bar display: "██████ 45%" instead of "45%"
let pct_display = if pct_text.is_empty() {
    String::new()
} else {
    // Extract percentage value for bar rendering
    let bar = match self.token_usage {
        Some((used, max)) if max > 0 => {
            let pct_val = (used * 100) / max;
            render_token_bar(pct_val, 6)
        }
        _ => "░░░░░░".to_string(),
    };
    format!("{bar} {pct_text}")
};

let pct_seg = if pct_display.is_empty() {
    0
} else {
    3 + display_width(&pct_display)
};
```

Then use `pct_display` instead of `pct_text` when building the spans:

```rust
if !pct_display.is_empty() {
    // ... existing pct_color logic ...
    spans.push(Span::new(" • ").dim());
    let mut pct_span = Span::new(&pct_display);  // changed from &pct_text
    if let Some(c) = pct_color {
        pct_span = pct_span.color(c);
    }
    spans.push(pct_span);
    if show_detail
        && let Some(ref detail) = detail_text
    {
        spans.push(Span::new(format!(" {detail}")).dim());
    }
}
```

### Step 5: Verify drop-level widths still correct

The `pct_seg` now uses `display_width(&pct_display)`. The bar chars `█`/`░` each have display_width=1. A 6-block bar + space + "45%" = 6+1+3=10. Old was 3 (just "45%"). Verify `w0`/`w1`/`w2`/`w3` still make sense — the bar will make the status line slightly wider, so the model or detail may drop sooner. This is correct behavior.

### Step 6: Run tests and check clippy

```bash
cargo test -q 2>&1 | grep -E "FAILED|test result"
cargo clippy -q 2>&1 | grep "^error"
```

### Step 7: Commit

```bash
git add src/tui/util.rs src/tui/render/bottom_ui.rs
git commit -m "Add visual token usage bar to status line"
```

---

## Final verification

```bash
cargo test -q 2>&1 | tail -3
cargo clippy -q 2>&1 | grep "^error"
```

Expected: all tests pass, no clippy errors.

---

## Execution notes

- Tasks 1 and 2 are fully independent. Task 3 depends on the `with_url` method added in Task 3 itself (self-contained). Task 4 and 5 are independent bug fixes. Task 6 is independent.
- Tasks 1, 2, 4, 5 are low risk — purely additive or internal consistency fixes with no behavior change.
- Task 3 (OSC 8) modifies `StyledLine::write_to_width` — the most sensitive rendering path. The change adds a per-span fallback path gated on `s.url.is_some()`. Since no existing spans have URLs, the fast path is always taken for all existing rendering. New path only activates for markdown links.
- Task 6 changes the status line layout (wider `pct_seg`). If the status line looks wrong at narrow terminal widths after this change, adjust `w0`/`w1`/`w2`/`w3` constants.

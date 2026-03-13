# Chat Rendering Enhancements

## Summary

Analysis of the chat rendering pipeline (2026-02-20) identified open bugs and missing features. This doc covers the rendering architecture, the correctness invariant it depends on, known bugs, and planned enhancements.

## Rendering Architecture

Ion uses an inline scrollback model: chat history is pushed into the terminal's native scrollback via `ScrollUp`, and a fixed-height bottom UI is anchored at the bottom. This is the same model used by Claude Code, Codex CLI, gemini-cli, and pi.

```
Terminal scrollback  ←  chat messages (StyledLine per row)
─────────────────────────────────────────────────────────
[ progress bar        ]  ← bottom UI, anchored
[ input box (1–N rows)]
[ status line         ]
```

**Key invariant:** Every `StyledLine` written to scrollback must occupy exactly 1 terminal row. If any `StyledLine` triggers a terminal line-wrap, the scroll amount is off and the bottom UI collides with the last chat message.

This invariant holds because:

- `build_lines` pre-wraps content at `width - 2` cells (upstream in `wrap_line` / `render_markdown_with_width`)
- `write_to_width` clips at `width - 1` cells as a safety net
- Tables produce one `StyledLine` per visual row, pre-computed
- `plan_chat_insert` uses `lines.len()` directly as the `ScrollUp` amount

## Known Bugs

### 1. `table.rs`: `UnicodeWidthStr::width` instead of `display_width`

`table.rs` uses `unicode_width::UnicodeWidthStr::width` directly in `measure_width`, `wrap_text`, `pad_cell`, and `break_word`. The rest of the codebase uses `crate::tui::util::display_width`. These should be consistent. In practice they agree for most text, but the inconsistency is a latent bug.

**Fix:** Replace direct `unicode_width` calls in `table.rs` with `display_width` from `crate::tui::util`.

### 2. `direct.rs`: `.len()` for column width calculations in selector hints

`selector_data()` for the Provider picker uses `s.provider.id().len()` to compute `max_id_len`, and for the Model picker uses `m.provider.len()` for `max_provider_w`. These are then used in `format!("{:width$}", ...)` which pads by scalar count, not display cells.

Provider IDs and names are currently ASCII-only, so this doesn't manifest. But it should be consistent with the rest of the column alignment code.

**Fix:** Use `display_width` for all `max_*` column calculations in `direct.rs`.

### 3. Resize race (incremental inserts)

During a resize, `term_width` changes. If `take_chat_inserts` computes `wrap_width = new_width - 2` but the resize+reflow hasn't completed yet, the line count from `build_lines` may not match what was already in scrollback. The existing `needs_reflow` path handles full redraws, but incremental inserts arriving in the same frame as a resize are a window of potential off-by-one.

This is the hardest correctness problem. Current mitigation: `needs_reflow` is triggered on `Resize` events and clears + reprints. It would fire before new inserts in normal event ordering, so in practice this rarely manifests.

**Tracking:** Create a dedicated task if/when visible.

## Planned Enhancements

### Strikethrough text

pulldown-cmark parses `~~text~~` as `Tag::Strikethrough` / `TagEnd::Strikethrough`. crossterm supports it via `SetAttribute(Attribute::CrossedOut)`. Currently `TextStyle` in `terminal.rs` has no strikethrough flag.

**Changes needed:**

- `TextStyle`: add `strikethrough: bool`
- `terminal.rs` `write_to` / `write_to_width`: emit `SetAttribute(CrossedOut)` / `Reset` around strikethrough spans
- `highlight/markdown.rs`: handle `Tag::Strikethrough` and `TagEnd::Strikethrough`
- `StyledSpan`: add `with_strikethrough()` builder method

**Risk:** Low. Purely additive.

### Task lists (`- [x]` / `- [ ]`)

pulldown-cmark supports `Options::ENABLE_TASKLISTS`. This emits a `TaskListMarker(checked: bool)` event between `Tag::Item` and the item text.

**Changes needed:**

- Add `Options::ENABLE_TASKLISTS` to the parser options in `render_markdown_with_width`
- Handle `Event::TaskListMarker(checked)`: replace the default `- ` prefix with `☑ ` or `☐ `

**Risk:** Low. Purely additive.

### OSC 8 hyperlinks

OSC 8 is the terminal hyperlink escape sequence: `\x1b]8;;URL\a text \x1b]8;;\a`. Supported in iTerm2, kitty, wezterm, ghostty, foot, and most modern terminals. Ignored gracefully by terminals that don't support it.

Makes URLs in agent output clickable — code review links, file paths, GitHub URLs.

**Changes needed:**

- `StyledSpan`: add `url: Option<String>` field
- `terminal.rs` `write_to`: emit OSC 8 open/close around span content when `url` is `Some`
- `highlight/markdown.rs`: handle `Tag::Link` to extract URL, set on spans within the link

**Risk:** Low. OSC 8 is a well-defined standard; terminals that don't support it treat the escapes as no-ops.

### Visual token usage bar

Replace the plain `45%` in the status line with a compact visual bar: `████░░░░░░ 45%`.

**Changes needed:**

- New helper `fn render_token_bar(pct: u64, width: usize) -> String` in `util.rs`
- Update `status_line_spans` in `bottom_ui.rs` to use it when `token_usage` is `Some((used, max))`
- Adjust `pct_seg` width calculation to account for bar width

**Risk:** Low. Status line already handles varying segment widths.

### Collapsible thinking blocks

Extended thinking output from Claude can be long. Show a collapsed summary by default: `▶ Thinking (47 lines)` with a toggle.

**Changes needed:**

- `MessagePart` needs a `Thinking(String)` variant (or already has one — check)
- `ChatRenderer` renders thinking blocks collapsed by default with a toggle key
- App state tracks which entries have thinking blocks expanded
- Key binding (e.g. `t` while not in input mode) toggles the focused entry's thinking

**Risk:** Medium. Requires new message part type, new app state, new key binding.

## What Other Agents Do

| Agent       | Strikethrough | Task lists | OSC 8 links | Token bar | Thinking collapse |
| ----------- | ------------- | ---------- | ----------- | --------- | ----------------- |
| Claude Code | ✓             | ✓          | ✓           | —         | ✓                 |
| Codex CLI   | ✓             | ✓          | —           | —         | —                 |
| gemini-cli  | ✓             | ✓          | ✓           | ✓         | —                 |
| opencode    | ✓             | ✓          | ✓           | ✓         | —                 |
| pi          | ✓             | —          | —           | ✓         | —                 |
| **ion**     | —             | —          | —           | —         | —                 |

## Priority Order

1. **Strikethrough** — most visible gap; commonly used in agent output for edits
2. **Task lists** — LLMs produce `- [x]` checklists routinely
3. **OSC 8 links** — high UX value, zero risk
4. **Token usage bar** — visual improvement to status line
5. **`table.rs` display_width** — bug fix, internal consistency
6. **`direct.rs` display_width** — bug fix, currently ASCII-only so not visible
7. **Collapsible thinking** — medium effort, only matters with extended thinking models

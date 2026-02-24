# TUI Integration Review (2026-02-23)

Commits reviewed: `8ffa33f..80eb1af` (5 commits on main)

## Summary

Port of event handling, slash commands, and state sync from the old TUI App to the new
`crates/tui`-based `IonApp`. Dead code from the old render pipeline removed. Terminal
Drop guard added for safety. Overall: solid work, well-structured migration.

Build: clean. Tests: 516 passing. Clippy: 8 warnings in changed files (all style:
collapsible ifs, map_or simplifications -- not bugs).

No critical issues found. Three WARN-level items below.

---

## Correctness / Safety Issues

### [WARN] src/tui/ion_app.rs:266 -- Skill name/args lowercased by to_lowercase()

`handle_slash_command` lowercases the entire input on line 266 before parsing
`//skill-name args`. Skill names are stored case-sensitively in `skill.entries`
(skill/mod.rs:120). A mixed-case skill name would fail lookup.

Pre-existing (same pattern in `src/tui/events.rs:490`). Low severity since skill
file names are conventionally lowercase.

```
-> Fix: Parse the skill portion from the original `input`, not from `cmd_line`:
   let trimmed = input.trim();
   let cmd_line = trimmed.to_lowercase();
   if trimmed.starts_with("//") {
       let skill_input = trimmed.strip_prefix("//").unwrap_or("").trim();
       ...
   }
```

### [WARN] src/ui/conversation.rs:270-276 -- scroll_down auto-scroll check uses stale total_lines

When `scroll_down()` is called after a `push()` (which sets `total_lines = usize::MAX`
as a dirty sentinel) but before the next `view()` call (which resolves it),
`max_offset` computes as `usize::MAX - visible_height`. The `scroll_offset >= max_offset`
check becomes unreachable, preventing auto-scroll re-enable from scroll_down.

Mitigated by: `resume_auto_scroll()` called explicitly when streaming completes
(ion_app.rs:512), and `ensure_rendered()` resolves the sentinel every frame (~16ms).
The failure window is small.

```
-> Fix: Guard the stale sentinel:
   pub fn scroll_down(&mut self, n: usize) {
       self.scroll_offset = self.scroll_offset.saturating_add(n);
       if self.total_lines != usize::MAX {
           let max_offset = self.total_lines.saturating_sub(self.visible_height);
           if self.scroll_offset >= max_offset {
               self.auto_scroll = true;
           }
       }
   }
```

### [WARN] src/tui/ion_app.rs:31 -- Duplicate CANCEL_WINDOW constant

`CANCEL_WINDOW` defined in both `ion_app.rs:31` and `types.rs:6` with identical value
(1500ms). Changing one won't update the other.

```
-> Fix: Remove the local const, import from types:
   use crate::tui::types::CANCEL_WINDOW;
```

---

## Quality / Refactoring Issues

### [NIT] crates/tui/src/widgets/input.rs:265-275 -- insert_text does not filter \r

`insert_text` handles `\n` but passes `\r` to `insert_char`, inserting it as a
literal control character. Crossterm's bracketed paste passes strings as-is. On
macOS this is a non-issue, but Windows-style `\r\n` in paste content would produce
visible artifacts.

```
Before: } else { self.insert_char(c); }
After:  '\r' => {} // strip carriage returns
```

### [NIT] src/tui/ion_app.rs:69-72 -- Parallel vectors for tool entry tracking

`tool_entry_indices` and `tool_content_lens` are kept in lockstep via paired
push/clear calls. Currently correct, but fragile. A single struct vector would
be more maintainable:

```rust
struct TrackedTool { msg_idx: usize, conv_idx: usize, content_len: usize }
tool_entries: Vec<TrackedTool>
```

---

## Positive Observations

- Terminal Drop guard (`crates/tui/src/terminal.rs:243-246`) prevents leaving the
  terminal in raw mode on panic or unclean exit.
- Idempotent `restore()` (`terminal.rs:186-189`) prevents double-restore issues.
  Signature changed from `pub fn restore(mut self)` to `pub fn restore(&mut self)` --
  necessary for Drop impl but also better API (doesn't consume Terminal).
- Event-to-message mapping in `handle_event` is well-organized: global bindings first,
  mode-specific second, input-mode third.
- Cursor positioning architecture is correct: computed in `view()`, read in
  `cursor_position()`, applied after `render()` in AppRunner.
- Tool result update tracking via content-length comparison (ion_app.rs:194-203) is
  a pragmatic approach avoiding change events from the message list.
- Dead code removal (ansi.rs, text.rs, util.rs) was thorough: 442 lines removed,
  12 dead tests removed, zero regressions.
- Slash command handler includes fuzzy "did you mean" suggestions for typos.
- Poisoned mutex recovery in message queue (ion_app.rs:649-651) is good defensive code.

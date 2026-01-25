# TUI Module Review

**Date:** 2026-01-25
**Status:** Good with minor fixes needed

## Summary

The TUI module is well-architected with good separation of concerns, comprehensive bounds checking, and thoughtful state management.

## Issues Found

### IMPORTANT

**1. Token Percentage Overflow Risk**
File: `src/tui/render.rs:368`

```rust
let pct = (used * 100) / context_max;
```

With 1M+ token context windows, `used * 100` could overflow on 32-bit systems.

Fix:

```rust
let pct = used.saturating_mul(100) / context_max.max(1);
```

**2. Slash Command History Index Not Reset**
File: `src/tui/events.rs:196-258`

When handling slash commands like `/model`, the code clears input but doesn't reset `history_index`. After using a slash command, pressing Up gives unexpected history entry.

Fix: Reset `history_index = self.input_history.len()` after `clear_input()` in all slash command branches.

### Already Fixed

**History Draft Blob Loss** (856f37b) - Was fixed in previous sprint.

## Code Quality

**Positive:**

- Excellent bounds checking throughout
- Good separation: Composer state ephemeral, buffer persistent
- Comprehensive visual line wrapping
- 12 unit tests covering edge cases

**Minor:**

- Some magic numbers could be configurable (PASTE_BLOB_LINE_THRESHOLD, etc.)
- Duplicate scroll_to_cursor logic could be extracted

## Refactor Recommendations

None required. Module is well-structured.

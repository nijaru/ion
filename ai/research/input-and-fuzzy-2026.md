# Research: Text Input + Fuzzy Matching (2026)

## Summary

Codex CLI implements its own custom `TextArea` and `fuzzy_match` rather than using external crates. For ion, rat-text is the most complete Rust TUI text editor widget available (multi-line, selection, undo/redo, ropey-backed), while tui-input is a lightweight single-line input helper. For fuzzy matching, fuzzy-matcher remains a good default for small lists; nucleo is stronger but heavier and MPL-2.0.

## Key Findings

### Codex CLI

- Uses a custom `TextArea` implementation in `codex-rs/tui2/src/bottom_pane/textarea.rs`.
- Uses a custom case-insensitive subsequence `fuzzy_match` helper in `codex-rs/common/src/fuzzy_match.rs`.
- Command popups and chat composer use this helper for fuzzy filtering.

### rat-text

- Provides `TextInput` (single-line) and `TextArea` (multi-line) widgets.
- Features: undo/redo, selection, grapheme-aware cursoring, ropey backend, clipboard integration, word-wrap, indent/dedent, range styling.
- Built for ratatui and designed for editor-like text handling.

### tui-input

- Lightweight multi-backend input widget.
- Designed for simple line input; no editor-grade multi-line behavior.
- Good for prompts, not sufficient for a chat composer with selection and history.

### fuzzy-matcher vs nucleo

- fuzzy-matcher: MIT, simple integration, good enough for provider/model lists and command palettes.
- nucleo: very strong matching/scoring, but MPL-2.0 and more complex.
- Codex uses a simple subsequence matcher (closer in spirit to fuzzy-matcher than nucleo).

## Options Considered

| Option      | Pros                                             | Cons                          |
| ----------- | ------------------------------------------------ | ----------------------------- |
| rat-text    | Full editor features; ratatui-native             | Heavier dependency surface    |
| tui-input   | Small, easy to integrate                         | Single-line, lacks editor UX  |
| custom      | Full control (Codex-style)                       | High maintenance cost         |
| fuzzy-matcher | Simple, MIT, easy highlighting               | Less advanced scoring         |
| nucleo      | Best-in-class scoring/perf                       | MPL-2.0, heavier API surface  |

## Recommended Approach

- Adopt rat-text for the main input editor and selector search to avoid future rewrites.
- Keep fuzzy-matcher for list filtering and highlighting; revisit nucleo only if list sizes or scoring quality become a clear problem.

## Sources

- Codex CLI repo (custom TextArea + fuzzy matcher): https://github.com/openai/codex
- rat-text README: https://github.com/thscharler/rat-salsa/blob/master/rat-text/readme.md
- tui-input README: https://github.com/sayanarijit/tui-input
- fuzzy-matcher docs: https://docs.rs/fuzzy-matcher
- nucleo docs: https://docs.rs/nucleo

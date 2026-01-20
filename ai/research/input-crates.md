# Research: Input Crates (rat-text vs tui-input)

## Summary

We need a long-term, multi-line text editor that handles graphemes correctly and supports selection/cursor movement without custom edge cases. `rat-text` appears to be the better long-term fit for the main input area; `tui-input` is solid but single-line oriented and better suited to search bars.

## Key Findings

### rat-text

- Crate: `rat-text` (ratatui text input widgets)
- Docs: https://docs.rs/rat-text/3.0.3
- Repo: https://github.com/thscharler/rat-salsa
- Keywords visible in docs: textarea, cursor, selection, editor, input
- Likely supports multi-line editing and selection (textarea focus)

### tui-input

- Crate: `tui-input` (TUI input library supporting multiple backends)
- Docs: https://docs.rs/tui-input/0.15.0
- Repo: https://github.com/sayanarijit/tui-input
- Keywords visible in docs: cursor, input
- Well-suited for single-line inputs (search/filter)

## Recommendation

- **Primary editor**: adopt `rat-text` for the main multi-line input to avoid future refactors.
- **Search bars**: use `rat-text` for consistency, unless single-line simplicity is preferred.

## Open Questions

- Validate `rat-text` supports the required keybindings (word navigation, Home/End, multi-line cursor movement, selection).
- Confirm performance with large inputs.

## Sources

- docs.rs: rat-text 3.0.3 https://docs.rs/rat-text/3.0.3
- docs.rs: tui-input 0.15.0 https://docs.rs/tui-input/0.15.0

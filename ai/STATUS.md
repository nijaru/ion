# ion Status

## Current State

| Metric    | Value                                                    | Updated    |
| --------- | -------------------------------------------------------- | ---------- |
| Phase     | TUI inline rendering — fixing crates/tui scrollback mode | 2026-02-24 |
| Status    | Two bugs fixed, needs manual testing                     | 2026-02-24 |
| Toolchain | stable                                                   | 2026-01-22 |
| Tests     | 516 passing                                              | 2026-02-24 |
| Clippy    | clean (zero new warnings)                                | 2026-02-24 |
| Blocker   | Manual testing of inline rendering                       | 2026-02-24 |

## Active Work: Inline Rendering Fix

IonApp uses `crates/tui` in inline mode (`.inline(3)`) — chat goes to native terminal scrollback via `insert_before()`, only bottom UI (status + input) renders in the 3-row inline region.

**Bugs fixed (commit 93f2ac3):**

1. **Blank space gap** — `insert_before()` always used `ScrollUp`, creating a gap between the shell prompt and the inline region on startup. Added `content_cursor` to Terminal for two-phase insertion: direct write fills empty space first, then `ScrollUp` for scrollback.

2. **Focus event leak (`^[[O`)** — `EnableFocusChange` executed before `EventStream::new()`, so terminal response escaped as visible text. Moved enables after event stream creation in `AppRunner::run_loop()`.

**Review:** LGTM, 0 errors, 0 warnings, 2 nits (cosmetic).

**Next:** Manual test with `cargo run` and `cargo run -- --continue` to verify rendering.

## What Works in IonApp (crates/tui)

- App launches, shows status bar + input at terminal bottom
- Chat content pushed to native terminal scrollback via insert_before
- Cursor visible in input widget
- Ctrl+C double-tap quit, Esc cancel, Shift+Tab mode toggle
- Slash commands, paste events, mouse scroll
- Picker/help/history-search overlays (switch to fullscreen mode)
- File/command completers, input history
- Tool expansion resync (Ctrl+O)
- Startup header pushed to scrollback on init

## Remaining Polish (not blocking parity)

| Gap                    | Impact | Notes                                       |
| ---------------------- | ------ | ------------------------------------------- |
| Editor open (Ctrl+G)   | P3     | Sets flag but nothing reads it              |
| Thinking level display | P3     | Ctrl+T cycles but no visual indicator       |
| ask_user visual prompt | P2     | Text works but no distinct visual treatment |

## Open Backlog (p4 only)

| Task    | Title                   | Notes                                             |
| ------- | ----------------------- | ------------------------------------------------- |
| tk-xhl5 | Plugin/extension system | Defer — premature without users/plugins           |
| tk-vru7 | colgrep evaluation      | Research: semantic code search as external tool   |
| tk-r11l | Agent config locations  | Research: standard paths for agent configs/skills |
| tk-nyqq | Symlink agents/skills   | Chezmoi dotfile task, not a code change           |
| tk-4gm9 | Settings selector UI    | Needs design doc first                            |

## Key References

| Topic                     | Location                                             |
| ------------------------- | ---------------------------------------------------- |
| TUI inline rendering plan | `/Users/nick/.claude/plans/async-dazzling-widget.md` |
| TUI lib spec              | `ai/design/tui-lib-spec.md`                          |
| TUI v3 architecture       | `ai/design/tui-v3-architecture-2026-02.md`           |
| Feature gap analysis      | `ai/research/feature-gap-analysis-2026-02.md`        |
| TUI render review         | `ai/review/tui-render-layout-review-2026-02-20.md`   |

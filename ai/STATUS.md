# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | OAuth Testing  | 2026-02-04 |
| Status    | In Progress    | 2026-02-04 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 299 passing    | 2026-01-31 |
| Clippy    | pedantic clean | 2026-01-31 |

## In Progress

**OAuth Subscription Auth** (2026-02-04):

- User wants **subscription OAuth moved to plugin/extension** so core stays supported-only. New task: `tk-kfx5`.
- **Gemini OAuth (tk-toyu)**: Antigravity OAuth implemented (fixed port 51121 `/oauth-callback`, `loadCodeAssist` project resolution, request wrapper `project`). Will move into plugin/extension per new direction.
- **ChatGPT OAuth (tk-uqt6)**: Responses API client now sets `store=false` and sends `function_call_output.output` as string. Will move into plugin/extension per new direction.

## Open Blockers

| Provider | Issue                                  | Next Step                     |
| -------- | -------------------------------------- | ----------------------------- |
| Gemini   | 403 license error (project-bound)      | Move OAuth to plugin; re-test there |
| ChatGPT  | 400 invalid_type for input[*].output   | Move OAuth to plugin; re-test there |
| TUI      | Line handling audit pending             | Review render/input/history for similar newline/scrollback issues |

## Top Priorities

1. Rebuild and test Gemini OAuth without project header
2. Rebuild and test ChatGPT Responses API
3. Plan OAuth move to plugin/extension (core supported-only)

## Key References

| Topic                 | Location                                      |
| --------------------- | --------------------------------------------- |
| Gemini OAuth research | ai/research/gemini-oauth-subscription-auth.md |
| OAuth design          | ai/design/oauth-subscriptions.md              |
| Architecture overview | ai/DESIGN.md                                  |

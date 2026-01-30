# ion Roadmap

**Updated:** 2026-01-29

## Completed

- ✅ Provider layer replacement (native HTTP, cache_control, provider routing)
- ✅ Anthropic caching support
- ✅ Kimi/DeepSeek reasoning extraction
- ✅ OAuth infrastructure (PKCE, callback server)
- ✅ Skills YAML frontmatter
- ✅ Subagent support
- ✅ TUI refactor (v2)

## In Progress

### OAuth Testing (P0)

**Goal:** Verify OAuth flows work with real subscriptions

| Task         | ID      | Status          |
| ------------ | ------- | --------------- |
| ChatGPT auth | tk-3a5h | Ready to test   |
| Gemini auth  | tk-toyu | Ready to test   |
| OAuth review | tk-uqt6 | Decision needed |

### Bug Fixes (P1)

| Task                     | ID      | Root Cause                   |
| ------------------------ | ------- | ---------------------------- |
| Error visibility         | tk-u25b | Chat rendering timing        |
| Progress line duplicates | tk-7aem | Missing focus event handling |
| Error JSON pretty-print  | tk-eu8s | Raw JSON in error messages   |

## Upcoming

### Vision & Input (P2)

| Task                 | ID      | Description                   |
| -------------------- | ------- | ----------------------------- |
| Image attachment     | tk-80az | @image:path syntax, base64    |
| File autocomplete    | tk-ik05 | @ triggers path picker        |
| Command autocomplete | tk-hk6p | / for builtins, // for skills |
| History navigation   | tk-50sw | Ctrl+P/N readline-style       |
| Fuzzy history search | tk-g3dt | Ctrl+R search                 |

### Code Organization (P2)

| Task            | Description                                     |
| --------------- | ----------------------------------------------- |
| Split render.rs | Extract selector + progress rendering (820 LOC) |
| Split events.rs | Extract keys + commands handling (630 LOC)      |
| Add context/    | Message conversion, ID remapping, truncation    |

### Extensibility (P3)

| Task                 | ID      | Description                     |
| -------------------- | ------- | ------------------------------- |
| Extensible providers | tk-o0g7 | Config-defined API providers    |
| Hook system          | -       | Lifecycle events for extensions |

## Deferred

| Task                    | Notes                               |
| ----------------------- | ----------------------------------- |
| PDF handling            | tk-ur3b - pdf2text integration      |
| Scrollback preservation | tk-2bk7 - complex terminal handling |
| True sandboxing         | Container/namespace for bash        |

## Timeline Guidance

| Phase             | Effort   | Impact |
| ----------------- | -------- | ------ |
| OAuth testing     | 1 day    | HIGH   |
| Bug fixes         | 1-2 days | HIGH   |
| Vision & input    | 2-3 days | HIGH   |
| Code organization | 1-2 days | MEDIUM |
| Extensibility     | 2-3 days | LOW    |

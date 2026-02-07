# Code Quality & Refactoring Audit

**Date:** 2026-02-06
**Codebase:** ion (27,660 lines across 91 .rs files)
**Status:** All 314 tests passing, 2 clippy warnings only

---

## 1. Top 10 Largest Files

| #   | File                                  | Lines |
| --- | ------------------------------------- | ----- |
| 1   | `src/tui/message_list.rs`             | 892   |
| 2   | `src/tui/events.rs`                   | 776   |
| 3   | `src/cli.rs`                          | 763   |
| 4   | `src/session/store.rs`                | 614   |
| 5   | `src/tui/composer/state.rs`           | 591   |
| 6   | `src/skill/mod.rs`                    | 591   |
| 7   | `src/tui/render/direct.rs`            | 574   |
| 8   | `src/tui/table.rs`                    | 567   |
| 9   | `src/provider/subscription/gemini.rs` | 563   |
| 10  | `src/tui/chat_renderer.rs`            | 533   |

**Notes:** Files 1-2 are large mainly due to thorough test suites (message_list has ~320 lines of tests, events is all event handling). The actual logic-to-test ratio is reasonable. `cli.rs` at 763 is the biggest concern -- it combines clap definitions, setup logic, and run mode logic.

## 2. Top 10 Longest Functions

| #   | Function                                 | File:Line                                  | ~Lines | Concern                                        |
| --- | ---------------------------------------- | ------------------------------------------ | ------ | ---------------------------------------------- |
| 1   | `handle_input_mode`                      | `src/tui/events.rs:79`                     | ~442   | Already has `#[allow(clippy::too_many_lines)]` |
| 2   | `handle_selector_mode`                   | `src/tui/events.rs:525`                    | ~171   | Already has `#[allow(clippy::too_many_lines)]` |
| 3   | `GeminiRequest::from_chat_request`       | `src/provider/subscription/gemini.rs:338`  | ~170   | Already has `#[allow(clippy::too_many_lines)]` |
| 4   | `AnthropicClient::build_request`         | `src/provider/anthropic/client.rs:121`     | ~131   | Already has `#[allow(clippy::too_many_lines)]` |
| 5   | `EditTool::execute`                      | `src/tool/builtin/edit.rs:55`              | ~143   | Already has `#[allow(clippy::too_many_lines)]` |
| 6   | `push_event`                             | `src/tui/message_list.rs:350`              | ~115   | Complex event dispatch                         |
| 7   | `load_from_messages`                     | `src/tui/message_list.rs:490`              | ~82    | Session restore logic                          |
| 8   | `build_instructions_and_input` (chatgpt) | `src/provider/subscription/chatgpt.rs:250` | ~100   | Message conversion                             |
| 9   | `search_with_grep`                       | `src/tool/builtin/grep.rs:153`             | ~100   | Orchestrates search modes                      |
| 10  | `Client::from_provider_sync`             | `src/provider/client.rs:93`                | ~52    | Near-duplicate of `from_provider`              |

## 3. Critical Issues (Must Fix)

### C1. `from_provider` / `from_provider_sync` duplication

**File:** `src/provider/client.rs:41-143`
**Confidence:** 95%

`from_provider` (async) and `from_provider_sync` share ~90% identical logic. The sync variant exists for cases where a tokio runtime isn't available during construction, but the body is essentially copy-pasted with the only difference being `auth::get_credentials` vs `storage.load()`.

**Impact:** Bug fixes or provider additions must be made in two places.
**Fix:** Extract shared logic into a private helper that takes credentials as input, called by both methods.

### C2. Unwrap in production code: `gemini.rs:50,54` (header parsing)

**File:** `src/provider/subscription/gemini.rs:50-54`
**Confidence:** 99%

```rust
format!("Bearer {}", self.access_token).parse().unwrap(),
"application/json".parse().unwrap(),
```

While `"application/json"` is guaranteed safe, the Bearer token `parse().unwrap()` will panic if the access token contains non-visible ASCII. The ChatGPT client (`chatgpt.rs:42`) correctly uses `map_err` for the same pattern.

**Fix:** Use `.map_err(|_| Error::Api("Invalid access token"))` like the ChatGPT client does.

### C3. Unwrap in production code: `auth/google.rs:307-320`

**File:** `src/auth/google.rs:307-320`
**Confidence:** 95%

Multiple `.parse().unwrap()` calls for header values. While the static strings are safe, the `access_token` one could contain invalid characters.

### C4. Unwrap in production code: `skill/mod.rs:112,333`

**File:** `src/skill/mod.rs:112,333`
**Confidence:** 90%

```rust
let path = entry.source_path.clone().unwrap();  // line 112
skills.push(current_skill.take().unwrap());       // line 333
```

The unwrap on line 112 is guarded by a preceding `filter` that ensures `source_path.is_some()`, but it's fragile -- a refactor could break the invariant silently. Line 333 is guarded by `current_skill.is_some()` check. Both should use `if let` or `expect` with a clear message.

## 4. Important Issues (Should Fix)

### I1. Dead code: `Explorer` struct

**File:** `src/agent/explorer.rs:1-15`
**Confidence:** 99%

15-line placeholder file with a struct that is never used anywhere in the codebase. The `#[allow(dead_code)]` annotation confirms this was intentional but it should either be removed or implemented.

### I2. Dead code: `ToolSource`, `ToolCapability`, `ToolMetadata` types

**File:** `src/tool/types.rs:166-211`
**Confidence:** 99%

These types are defined but never used outside their definition file. They appear to be speculative API surface for a future plugin/MCP tool metadata system. They are exported via `pub use types::*` from `tool/mod.rs`, inflating the public API surface.

### I3. Dead code: `DiscoverTool` registered but never used

**File:** `src/tool/builtin/discover.rs` (74 lines)
**Confidence:** 90%

`DiscoverTool` is defined and exported but the comment in `tool/mod.rs:133` says "Note: DiscoverTool requires semantic search backend (not yet implemented)". It's never registered in `with_builtins()`.

### I4. Hook points `OnError` and `OnResponse` never triggered

**File:** `src/hook/mod.rs:17-19`
**Confidence:** 99%

`HookPoint::OnError` and `HookPoint::OnResponse` are defined but never used anywhere in the codebase. Only `PreToolUse` and `PostToolUse` are ever triggered (in `tool/mod.rs:53-103`). This is speculative API.

### I5. `events.rs` `handle_input_mode` is too long and should be decomposed

**File:** `src/tui/events.rs:79-521`
**Confidence:** 95%

At ~442 lines, this is the longest function. The command completer handling (84-137), file completer handling (140-207), and slash command dispatch (347-458) are three independent concerns that should be extracted into separate methods.

### I6. Config merge function is repetitive

**File:** `src/config/mod.rs:231-295`
**Confidence:** 85%

The `merge` method has 30+ nearly identical `if other.X.is_some() { self.X = other.X }` lines. While not incorrect, this is maintenance-heavy and easy to miss fields when adding new config options. Could use a macro or a trait-based approach.

### I7. `pub use types::*` glob re-exports

**Files:** `src/tool/mod.rs:6`, `src/provider/mod.rs:35`
**Confidence:** 80%

These glob re-exports expose everything from `types.rs` including the dead types mentioned in I2. Explicit re-exports would document the actual public API and catch dead code at compile time.

### I8. Stringly-typed provider names in Config

**File:** `src/config/mod.rs:92-96`
**Confidence:** 85%

`Config.provider` is `Option<String>` but there's a perfectly good `Provider` enum in `provider/api_provider.rs`. The string is parsed via `Provider::from_id()` at usage sites. Similarly, `PermissionConfig.default_mode` is `Option<String>` instead of `Option<ToolMode>`.

**Why it matters:** Invalid values are only caught at runtime, not at deserialization time. A typo in `config.toml` like `provider = "anthopic"` silently fails.

### I9. `uuid_v4` is not actually UUID v4

**File:** `src/provider/subscription/gemini.rs:220-236`
**Confidence:** 95%

The `uuid_v4()` function generates an ID from `SystemTime + process ID`, but this is not a valid UUID v4 (which requires cryptographic randomness). The format masks this -- it looks like a UUID but has poor uniqueness guarantees, especially if two requests happen within the same nanosecond.

**Fix:** Use `uuid` crate or at minimum mix in a counter (which the `into_message` method already does for tool call IDs at line 527).

## 5. Uncertain Issues (Verify)

### U1. `table.rs:283` and `chat_renderer.rs:279` -- unwrap on chars iterator

**Files:** `src/tui/table.rs:283`, `src/tui/chat_renderer.rs:279`
**Confidence:** 70%

Both do `chars.next().unwrap()` inside a loop that checks `chars.peek()` first. Logically safe but brittle -- the unwrap relies on the iterator not being consumed between `peek()` and `next()`. Worth converting to `if let Some(c) = chars.next()`.

### U2. Mutex unwrap in grep tool

**File:** `src/tool/builtin/grep.rs:249,281,312,345,375`
**Confidence:** 60%

Multiple `results.lock().unwrap()` and `results.into_inner().unwrap()`. Standard Mutex poison recovery. These unwraps are idiomatic for single-threaded walker usage, but if the walker ever panics during a search, these will panic too. Low risk in practice.

### U3. `std::sync::Mutex` for message queue in async context

**File:** `src/tui/mod.rs:117`, used in `src/tui/events.rs:335-341`
**Confidence:** 65%

`message_queue: Option<Arc<std::sync::Mutex<Vec<String>>>>` -- a `std::sync::Mutex` is held across `.push()` in an async context. Since the lock is held for only a brief push operation and never across `.await`, this is technically fine but inconsistent with the rest of the codebase which uses `tokio::sync::Mutex` and `tokio::sync::RwLock`.

## 6. Module Organization Assessment

### Good

- **provider/**: Well-organized with clear separation (api_provider, client, types, error, http, anthropic, openai_compat, subscription). Each provider backend is isolated.
- **tool/**: Clean split between types, permissions, builtins, and orchestrator.
- **compaction/**: Small, focused module with counter and pruning.
- **tui/composer/**: Well-extracted buffer, state, visual_lines.

### Could Improve

- **tui/**: 28 sub-files is getting crowded. The `events.rs` (776 lines) mixes input, selector, and history-search handling. The session/ subdirectory is a good pattern that could be extended.
- **agent/**: The main `mod.rs` (397 lines) handles both agent loop orchestration AND configuration/construction. Constructor logic could move to a `builder.rs`.
- **cli.rs**: Combines argument parsing, setup flow, run mode, and login/logout subcommands. At 763 lines it would benefit from splitting into subcommand modules.

### Dependencies

No circular dependencies detected. The dependency graph flows cleanly: `main -> tui -> agent -> provider, tool, session, compaction`. Clean layering.

## 7. Type Design Assessment

### Good

- `Provider` enum instead of strings for provider identification
- `ToolMode` enum (Read/Write) instead of boolean
- `DangerLevel` enum for tool classification
- `ContentBlock` tagged enum for message content types
- `CommandRisk` enum with reason payload (using `Cow<'static, str>`)
- `Arc<Vec<ContentBlock>>` for shared message content (avoids cloning)

### Issues

- **Stringly-typed config fields** (I8 above): `Config.provider`, `PermissionConfig.default_mode`
- **Large App struct**: `App` in `tui/mod.rs` has 30+ public fields. This is a "god object" pattern. Some fields could be grouped into sub-structs (e.g., `PickerState { model_picker, provider_picker, session_picker, pending_provider }`).
- **`HookContext` option-heavy**: 5 of 7 fields are `Option<T>`. This could use the builder pattern (which it partially does) or separate structs per hook point.

## 8. Error Handling Assessment

### Good

- `thiserror` used correctly for library-boundary errors (`ToolError`, `provider::Error`, `error::Error`)
- `anyhow` used in application code (`config::load`, `session::store`)
- Error context provided via `.with_context()` in config loading
- `format_api_error` extracts human-readable messages from JSON error responses -- well-tested
- Provider errors include HTTP status codes and response bodies

### Issues

- **Silent error swallowing**: Several places use `let _ = tx.send(...).await` in streaming code. If the channel is closed, the error is silently dropped. This is likely intentional (receiver disconnected = user cancelled) but should be documented.
- **tracing::warn for parse failures**: Multiple places (e.g., `anthropic/client.rs:106`, `gemini.rs:159`) log parse failures at warn level but continue. Good resilience, but these could benefit from structured logging (include the raw data length, not just the error).

## 9. Test Coverage Assessment

### Well-Tested (have tests)

- `message_list.rs` -- 320 lines of tests (comprehensive)
- `config/mod.rs` -- merge, defaults, system prompt
- `tool/builtin/edit.rs` -- 7 test cases covering edge cases
- `tool/builtin/guard.rs` -- extensive command risk detection
- `provider/client.rs` -- backend selection tests
- `provider/api_provider.rs` -- roundtrip, sorting, availability
- `compaction/` -- pruning tiers, counters
- `skill/mod.rs` -- parsing, summary, search
- `hook/mod.rs` -- registry, matching, abort behavior
- `provider/error.rs` -- error formatting with multiple JSON shapes

### Coverage Gaps (no tests, non-trivial logic)

| Module                  | Lines | Risk                                    |
| ----------------------- | ----- | --------------------------------------- |
| `agent/mod.rs`          | 397   | High -- core agent loop, turn execution |
| `agent/stream.rs`       | 301   | High -- streaming response handling     |
| `agent/context.rs`      | 212   | Medium -- system prompt assembly        |
| `mcp/mod.rs`            | 243   | Medium -- MCP client integration        |
| `tui/events.rs`         | 776   | Medium -- keyboard input handling       |
| `tui/render/direct.rs`  | 574   | Medium -- terminal rendering            |
| `tui/chat_renderer.rs`  | 533   | Medium -- markdown rendering            |
| `tool/builtin/bash.rs`  | 163   | Medium -- command execution             |
| `tool/builtin/read.rs`  | 223   | Medium -- file reading                  |
| `tool/builtin/write.rs` | 132   | Medium -- file writing                  |
| `tool/builtin/glob.rs`  | 136   | Low -- uses well-tested libraries       |
| `tool/builtin/grep.rs`  | 438   | Low -- uses well-tested libraries       |
| `session/store.rs`      | 614   | Low -- has tests already                |

**Highest priority gaps:** `agent/mod.rs` and `agent/stream.rs` contain the core agent loop and streaming logic with no unit tests. These are the riskiest untested modules.

## 10. Summary of Recommended Actions

### Quick Wins (low effort, high value)

1. Fix unwrap in `gemini.rs:50` (header parsing panic risk)
2. Fix unwrap in `auth/google.rs:307` (same issue)
3. Delete `agent/explorer.rs` (dead code placeholder)
4. Remove `ToolSource`, `ToolCapability`, `ToolMetadata` (dead types)
5. Remove `HookPoint::OnError`, `HookPoint::OnResponse` (unused variants)

### Medium Effort

6. Extract `from_provider` / `from_provider_sync` shared logic
7. Extract `handle_input_mode` into sub-methods (completer, slash commands)
8. Replace `pub use types::*` with explicit re-exports
9. Use `Provider` enum in `Config` instead of `Option<String>`

### Larger Refactors

10. Split `cli.rs` into subcommand modules
11. Break up `App` struct into grouped sub-structs
12. Add integration tests for `agent/mod.rs` agent loop
13. Replace `uuid_v4` with proper UUID generation

### Code Health Metrics

- **Clippy:** 2 warnings (both cosmetic)
- **Tests:** 314 passing, 0 failing
- **Dead code:** ~300 lines removable (explorer, dead types, unused hook variants)
- **Unwraps in prod:** 6 locations (2 critical, 4 low-risk)
- **Files >400 lines:** 12 files (reasonable for a 27k-line codebase)

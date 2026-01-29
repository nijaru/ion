# Sprint Plan: ion Stabilization & UX

Source: ai/DESIGN.md, ai/STATUS.md, ai/design/inline-viewport.md, ai/design/session-storage.md
Generated: 2026-01-22
Updated: 2026-01-23

## Status

| Sprint | Goal                              | Status   |
| ------ | --------------------------------- | -------- |
| 0      | TUI Architecture                  | COMPLETE |
| 1      | Inline Viewport Stabilization     | COMPLETE |
| 2      | Run State UX & Error Handling     | COMPLETE |
| 3      | Selector & Resume UX              | COMPLETE |
| 4      | Visual Polish & Advanced Features | PLANNED  |
| 5      | Session Storage Redesign          | PLANNED  |
| 6      | TUI Module Refactor               | COMPLETE |
| 7      | Codebase Review & Refactor        | COMPLETE |
| 8      | Core Loop & TUI Deep Review       | COMPLETE |
| 9      | Feature Parity & Extensibility    | COMPLETE |
| 10     | Stabilization & Refactor          | COMPLETE |
| 11     | TUI v2: Remove ratatui            | PLANNED  |
| 12     | Clippy Pedantic Refactoring       | ACTIVE   |

## Sprint 0: TUI Architecture - Custom Text Entry + Viewport Fix

**Goal:** Replace rat-text with custom Composer and fix viewport content leaking.
**Source:** `~/.claude/plans/merry-knitting-crab.md`
**Status:** COMPLETE (2026-01-23)

## Task: Port Composer from ion-copy

**Sprint:** 0
**Depends on:** none
**Status:** DONE (tk-l6yf)

### Description

Port the existing custom text entry implementation from `../ion-copy/src/tui/widgets/composer/` to ion. This includes:

- `buffer.rs` - ComposerBuffer with ropey, blob storage for large pastes
- `mod.rs` - ComposerState with grapheme-safe cursor, word navigation

Add scroll support:

- `scroll_offset` field for internal scrolling
- `scroll_to_cursor()` to keep cursor visible
- Update rendering to only show visible lines

### Acceptance Criteria

- [x] ComposerBuffer ported with ropey backend
- [x] ComposerState ported with grapheme-aware cursor
- [x] Scroll offset tracks visible window
- [x] Widget renders only visible lines
- [x] Cargo.toml updated: add ropey, unicode-segmentation; remove rat-text, rat-event

---

## Task: Wire up platform-specific keybindings

**Sprint:** 0
**Depends on:** Port Composer from ion-copy
**Status:** DONE (tk-1uns)

### Description

Map crossterm key events to Composer methods with platform-specific handling.

**All platforms:**

- Left/Right: move by grapheme
- Up/Down: move by line / history at edges
- Home/End, Ctrl+A/E: line start/end
- Backspace/Delete: delete grapheme
- Ctrl+W: delete word before cursor
- Ctrl+U: delete entire line
- Ctrl+K: delete to end of line
- Shift+Enter: insert newline
- Enter: submit
- Ctrl+G: open external editor
- Esc Esc: clear input

**macOS only (Option = ALT in crossterm):**

- Opt+Left/Right: move by word
- Opt+Backspace: delete word before cursor

**Windows/Linux only:**

- Ctrl+Left/Right: move by word
- Ctrl+Backspace: delete word before cursor

Use `cfg!(target_os = "macos")` for compile-time platform detection.

### Acceptance Criteria

- [x] All-platform keybindings work
- [x] macOS Option+arrow moves by word
- [x] Windows/Linux Ctrl+arrow moves by word
- [x] Shift+Enter inserts newline
- [x] Ctrl+G opens $VISUAL/$EDITOR

---

## Task: Implement dynamic input height

**Sprint:** 0
**Depends on:** Port Composer from ion-copy
**Status:** DONE (tk-042d)

### Description

Input box grows dynamically with content, up to terminal height minus 6 reserved lines:

- Empty line above UI (1)
- Progress line (1-2)
- Input box minimum (3) - 1 content + 2 borders
- Status line (1)

When content exceeds max display height, internal scrolling keeps cursor visible.

### Acceptance Criteria

- [x] Input grows from 3 lines minimum
- [x] Input never exceeds term_height - 6
- [x] Internal scroll kicks in at max height
- [x] Cursor always visible during scroll

---

## Task: Fix viewport content leaking

**Sprint:** 0
**Depends on:** Implement dynamic input height
**Status:** DONE (tk-qo7b)

### Description

Current issue: recreating Terminal on viewport height change causes content to leak into scrollback.

Solution: Full-height viewport created once, never recreated except on actual terminal resize.

- Viewport height = terminal height
- UI rendered at bottom of viewport
- Empty space above absorbs size changes
- Remove all viewport recreation based on UI size changes

### Acceptance Criteria

- [x] Viewport created once at terminal height
- [x] Only recreated on Event::Resize with new height
- [x] Submit message does not leak input box to scrollback
- [x] Growing/shrinking input does not corrupt scrollback
- [x] Terminal resize works cleanly

---

## Task: Verification and cleanup

**Sprint:** 0
**Depends on:** Fix viewport content leaking
**Status:** DONE (tk-x1x6)

### Description

End-to-end verification of the new TUI architecture:

1. Type text → cursor moves correctly by grapheme
2. Ctrl+W / Opt+Backspace → deletes word
3. Ctrl+A/E → moves to line start/end
4. Shift+Enter → inserts newline
5. Type 50 lines → grows to near-terminal height, can scroll internally
6. Arrow up/down in input → scrolls through content
7. Submit message → input shrinks, no leakage
8. Ctrl+G → opens external editor
9. Resize terminal → clean recreation
10. Cmd+F → search works in scrollback

### Acceptance Criteria

- [x] All tests pass (cargo test)
- [x] No regressions in existing TUI functionality
- [x] rat-text and rat-event fully removed from codebase

---

## Sprint 1: Inline Viewport Stabilization

**Goal:** Finalize the inline viewport migration and ensure native terminal behavior.

## Task: Remove residual alternate-screen behavior

**Sprint:** 1
**Depends on:** none

### Description

Ensure the TUI never clears or emulates alternate-screen behavior. Inline viewport should preserve native scrollback, selection, and mouse wheel scrolling. Audit terminal init for EnterAlternateScreen/EnableMouseCapture and remove any full-screen clear/reset calls.

### Acceptance Criteria

- [x] No alternate screen or mouse capture in terminal init/shutdown
- [x] Click/drag selection and mouse wheel scrolling work natively
- [x] Sending a message does not clear the full terminal buffer

---

## Task: Fix viewport spacing and message margins

**Sprint:** 1
**Depends on:** none

### Description

Tighten viewport layout so chat content sits immediately above the viewport without large blank gaps. Add a 1-column left/right margin for messages. Ensure input separators span edge-to-edge.

### Acceptance Criteria

- [x] No large empty block between last message and viewport
- [x] Messages render with left/right margin (1 column)
- [x] Input top/bottom separators extend full width

---

## Task: Append chat via insert-before scrollback

**Sprint:** 1
**Depends on:** none

### Description

Move chat rendering out of the viewport and append new chat lines directly into terminal scrollback using `Terminal::insert_before`.

### Acceptance Criteria

- [x] Chat output is appended to scrollback; viewport redraws only progress/input/status
- [x] Selector mode buffers new chat lines and flushes on close
- [ ] Verified in a real terminal with scrolling and selection

---

## Task: User prefix only on first line

**Sprint:** 1
**Depends on:** none

### Description

Render user messages with a `> ` prefix only on the first line; subsequent lines are unprefixed. User text is dim cyan.

### Acceptance Criteria

- [x] `> ` prefix appears only on the first line
- [x] Wrapped lines do not include the prefix

---

## Task: Refactor draw into render helpers

**Sprint:** 1
**Depends on:** none

### Description

Split `App::draw` into focused layout/data/render helpers to reduce complexity and isolate regressions. Layout computation should be separate from rendering.

### Acceptance Criteria

- [x] Layout computation is separate from rendering
- [x] Chat, progress, input, and status rendering are in dedicated helpers
- [x] Behavior matches current UI

---

## Task: Extract chat renderer module

**Sprint:** 1
**Depends on:** Refactor draw into render helpers

### Description

Move chat message formatting (user/agent/tool/system) into a dedicated renderer module. Consider a `ChatRenderer` type with a `build_lines()` method.

### Acceptance Criteria

- [x] Chat rendering is isolated from `tui::mod` draw logic
- [x] Output matches current formatting
- [x] No behavioral regressions in tool/diff rendering

---

## Task: Fix chat_lines order in draw layout

**Sprint:** 1
**Depends on:** none

### Description

Ensure chat line collection happens before any logic that uses its length, so viewport height calculations are correct.

### Acceptance Criteria

- [x] `chat_lines` is built before any size/height calculations
- [x] draw() compiles without use-before-define errors

---

## Task: Restore write-mode tool approvals

**Sprint:** 1
**Depends on:** none

### Description

Write mode should only auto-allow safe tools and explicitly approved restricted tools.

### Acceptance Criteria

- [x] Restricted tools still require approval unless whitelisted
- [x] No blanket allow for non-bash tools

---

## Task: Make truncation UTF-8 safe

**Sprint:** 1
**Depends on:** none

### Description

Avoid panics when truncating non-ASCII text in CLI and TUI displays.

### Acceptance Criteria

- [x] No byte-slicing in truncation helpers
- [x] Truncation handles Unicode safely

---

## Task: Fix input editor phantom line + history navigation

**Sprint:** 1
**Depends on:** none

### Description

Ensure the input editor does not show a phantom blank line when typing. Up/Down history should work on first press at top/bottom line and restore draft text correctly.

### Acceptance Criteria

- [x] No extra blank line appears when typing (trailing newlines show new line - expected behavior)
- [x] Up retrieves last sent message on first press at top line
- [x] Down restores newer history and draft without clearing input

## Sprint 2: Run State UX & Error Handling

**Goal:** Provide clear feedback during task execution and handle failures gracefully.

## Task: Progress line state mapping

**Sprint:** 2
**Depends on:** Sprint 1

### Description

Define and render clear run states on the ionizing/progress line: running, cancelling, cancelled, error, completed.

### Acceptance Criteria

- [x] Running shows normal ionizing text
- [x] Cancelling shows yellow "Canceling..." with warning indicator
- [x] Error shows red "Error" with red indicator
- [x] Completed shows normal completion state

---

## Task: Provider retry/backoff + chat log entries

**Sprint:** 2
**Depends on:** Sprint 1

### Description

Add retry/backoff for transient provider errors (OpenRouter timeouts) and log "Retrying..." in chat history before retry attempts.

### Acceptance Criteria

- [x] Retries occur on timeout/network errors
- [x] Chat shows a retry notice before retry attempt
- [x] Final error appears in chat in red if retries exhausted

---

## Task: Status line accuracy for context usage

**Sprint:** 2
**Depends on:** Sprint 1

### Description

Populate max context length from model registry on model selection. If unknown, show used/0k and omit percent.

### Acceptance Criteria

- [x] Status line shows used/max when max known
- [x] Percent only shown when max known
- [x] Unknown max shows used/0k with no percent

---

## Task: Graceful TUI init error handling

**Sprint:** 2
**Depends on:** Sprint 1

### Description

Replace `unwrap/expect` in TUI init paths with user-visible errors and clean exits. Config/session/client/terminal init failures should surface clearly.

### Acceptance Criteria

- [x] Config/session/client/terminal init failures surface clearly
- [x] App exits without panic

## Task: Extract token usage emit helper

**Sprint:** 2
**Depends on:** none

### Description

Consolidate repeated token usage emission in agent loop into a single helper.

### Acceptance Criteria

- [x] No duplicated token usage emission code in agent loop
- [x] Behavior unchanged

---

## Task: Handle NaN pricing sort in registry

**Sprint:** 2
**Depends on:** none

### Description

Avoid panics when sorting by price if a model has NaN pricing data.

### Acceptance Criteria

- [x] Sorting never panics on NaN pricing
- [x] Sorting remains stable for invalid price data

## Sprint 3: Selector & Resume UX

**Goal:** Enhance session management and navigation within the TUI.

## Task: /resume selector UI for past sessions

**Sprint:** 3
**Depends on:** none

### Description

Add a /resume command that opens the shared selector UI for prior sessions. Reuse selector shell list + filter infrastructure.

### Acceptance Criteria

- [x] /resume opens selector with recent sessions
- [x] Selecting a session loads it
- [x] Escape closes selector without changes

---

## Task: --resume/--continue CLI flags

**Sprint:** 3
**Depends on:** none

### Description

Add CLI flags to resume the latest or a specific session by ID. Ensure flags integrate with existing config loading.

### Acceptance Criteria

- [x] --resume reopens latest session
- [x] --continue <id> reopens specified session
- [x] Invalid IDs print a clear error and exit

---

## Task: Fuzzy search ordering (substring first)

**Sprint:** 3
**Depends on:** none

### Description

Prioritize exact substring matches before fuzzy matches in selectors and command suggestions.

### Acceptance Criteria

- [x] Substring matches appear before fuzzy matches
- [x] Fuzzy matches appear only when no substring matches exist

## Sprint 4: Visual Polish & Advanced Features

**Goal:** Refine the aesthetic and core architecture.

## Task: Diff highlighting for file edits

**Sprint:** 4
**Depends on:** Sprint 1

### Description

Implement syntax-highlighted diffs for the `edit` tool results. Show additions in green, deletions in red, with word-level highlighting for changed parts of lines.

### Acceptance Criteria

- [x] ToolResult enhanced with DiffInfo (diff included in content, TUI detects edit tool)
- [x] Edit tool populates DiffInfo on success (generates unified diff via similar crate)
- [x] TUI renders styled diff lines in chat (highlight_diff_line function)

---

## Task: Startup header line

**Sprint:** 4
**Depends on:** Sprint 1

### Description

Render a minimal startup header (ION + version) without pushing the viewport down or clearing scrollback. Mimic Claude Code: header stays above the viewport.

### Acceptance Criteria

- [x] Header visible at startup
- [x] No excessive blank lines before viewport
- [x] Header does not clear scrollback

---

## Task: Decompose Agent loop into discrete phases

**Sprint:** 4
**Depends on:** none

### Description

Refactor the core multi-turn loop into Response, Tool, and State phases to improve reliability and enable better unit testing of tool execution.

### Acceptance Criteria

- [ ] Phases are clearly separated in code
- [ ] Error handling is robust at phase boundaries
- [ ] Unit tests verify tool execution without live LLM

---

## Task: Grep/Glob tool upgrade (ignore crate)

**Sprint:** 4
**Depends on:** none

### Description

Replace the current manual recursion and `glob` crate with the `ignore` crate for the grep and glob tools. This adds support for `.gitignore` and improves performance.

### Acceptance Criteria

- [x] Grep tool respects `.gitignore`
- [x] Glob tool uses `globset` via the `ignore` crate
- [x] `walkdir` and `glob` dependencies removed

---

## Task: Token counter swap (bpe-openai)

**Sprint:** 4
**Depends on:** none

### Description

Swap `tiktoken-rs` for the faster `bpe-openai` crate.

### Acceptance Criteria

- [x] Token counting uses `bpe-openai`
- [x] Performance improved for large messages
- [x] `tiktoken-rs` dependency removed

## Sprint 5: Session Storage Redesign

**Goal:** Portable JSONL-based sessions with per-directory organization.
**Design:** ai/design/session-storage.md

## Task: Implement JSONL session format

**Sprint:** 5
**Depends on:** none

### Description

Replace SQLite session storage with JSONL files. Each session is a single file with typed events (meta, user, assistant, tool_use, tool_result).

### Acceptance Criteria

- [ ] SessionFile struct reads/writes JSONL format
- [ ] Append-only writes for crash safety
- [ ] All message types serialized correctly

---

## Task: Per-directory session organization

**Sprint:** 5
**Depends on:** Implement JSONL session format

### Description

Organize sessions by encoded working directory path. Create `~/.ion/sessions/{encoded-path}/` structure.

### Acceptance Criteria

- [ ] Path encoding (slashes → dashes) implemented
- [ ] Sessions stored in correct directory
- [ ] Old sessions migrated on first run

---

## Task: Per-directory index and input history

**Sprint:** 5
**Depends on:** Per-directory session organization

### Description

Add per-directory index.db for fast picker queries and input.db for input history.

### Acceptance Criteria

- [ ] index.db tracks: id, updated_at, message_count, last_preview, branch
- [ ] input.db stores per-directory input history
- [ ] Index updated on session save

---

## Task: Resume picker UI

**Sprint:** 5
**Depends on:** Per-directory index and input history

### Description

Implement /resume command with session picker showing time, message count, and preview.

### Acceptance Criteria

- [ ] /resume opens selector with sessions from current directory
- [ ] Display: relative time, message count, last preview
- [ ] Selected session loads correctly

---

## Task: CLI resume flags

**Sprint:** 5
**Depends on:** Resume picker UI

### Description

Add --continue (latest from cwd) and --resume (picker or specific ID) CLI flags.

### Acceptance Criteria

- [ ] --continue loads most recent session from current directory
- [ ] --resume opens picker
- [ ] --resume <id> loads specific session

## Sprint 6: TUI Module Refactor

**Goal:** Split tui/mod.rs (2325 lines) into 6 focused modules for maintainability.
**Source:** Refactoring analysis from session 2026-01-23
**Status:** COMPLETE (2026-01-24)

| Module       | Lines | Purpose                        |
| ------------ | ----- | ------------------------------ |
| `types.rs`   | 122   | Enums, structs, constants      |
| `util.rs`    | 122   | Standalone utility functions   |
| `input.rs`   | 211   | Input/composer handling        |
| `events.rs`  | 468   | Event dispatch, mode handlers  |
| `render.rs`  | 787   | All rendering/drawing          |
| `session.rs` | 603   | Session, provider, agent, init |
| `mod.rs`     | 125   | Module wiring, re-exports      |

---

## Task: Extract TUI modules and verify

**Sprint:** 6
**Depends on:** none
**Status:** DONE

### Description

Split tui/mod.rs into focused modules. All tasks completed in single session.

### Acceptance Criteria

- [x] types.rs created (122 lines)
- [x] util.rs created (122 lines)
- [x] input.rs created (211 lines)
- [x] events.rs created (468 lines)
- [x] render.rs created (787 lines)
- [x] session.rs created (603 lines)
- [x] mod.rs reduced to 125 lines
- [x] cargo build succeeds
- [x] cargo test passes
- [x] All re-exports working

## Sprint 7: Codebase Review & Refactor

**Goal:** Zero clippy warnings, all critical bugs fixed, performance baselined
**Source:** Codebase analysis 2026-01-25
**Status:** ACTIVE

### Demoable Outcomes

- [ ] `cargo clippy` produces 0 warnings (currently 13)
- [ ] All tests pass (89 currently)
- [ ] No critical bugs in code review
- [ ] Startup time baselined
- [ ] Refactor plan documented

### Module Priority

| Module        | Lines | Risk | Review Focus                              |
| ------------- | ----- | ---- | ----------------------------------------- |
| tui/          | ~4k   | High | render.rs 800L, composer 1086L, events.rs |
| agent/        | 735   | High | Main loop, state transitions              |
| provider/     | ~1.5k | Med  | API errors, retries, registry 676L        |
| tool/builtin/ | ~3k   | Med  | Recently reviewed - quick scan            |
| session/      | 565   | Low  | Persistence correctness                   |
| compaction/   | ~500  | Low  | Pruning edge cases                        |
| config/       | 532   | Low  | Validation                                |
| mcp/          | ~300  | Low  | Resource cleanup                          |
| skill/        | ~200  | Low  | Loading edge cases                        |

### Issue Severity Rubric

| Severity  | Criteria                       | Action          |
| --------- | ------------------------------ | --------------- |
| CRITICAL  | Crashes, data loss, security   | Fix immediately |
| IMPORTANT | Incorrect behavior, poor UX    | Fix this sprint |
| MINOR     | Style, minor inefficiency      | Log for later   |
| WONTFIX   | Intentional, tradeoff accepted | Document why    |

### Review Checklist (per module)

- **Correctness**: logic errors, off-by-one, boundary conditions, panic paths
- **Safety**: unwraps without guard, unchecked bounds, resource leaks
- **State**: inconsistent state after errors, race conditions
- **Errors**: swallowed errors, unclear messages, missing recovery

---

## Task: S7-1 Fix clippy warnings and format

**Sprint:** 7
**Depends on:** none
**Status:** PENDING

### Description

Clean baseline - zero warnings, consistent formatting.

### Acceptance Criteria

- [ ] `cargo clippy --fix --lib --bin ion` applied
- [ ] Remaining 13 warnings fixed manually
- [ ] `cargo clippy` produces 0 warnings
- [ ] `cargo fmt --check` passes
- [ ] All tests still pass

---

## Task: S7-2 Review tui/ module

**Sprint:** 7
**Depends on:** S7-1
**Status:** PENDING

### Description

Review TUI module using checklist. Priority files: render.rs (800L), composer/mod.rs (1086L), events.rs (505L).

### Files to Review

- [ ] render.rs - rendering logic
- [ ] composer/mod.rs - text input state
- [ ] events.rs - event handling
- [ ] session.rs - session management in TUI
- [ ] model_picker.rs, provider_picker.rs - selectors

### Acceptance Criteria

- [ ] Checklist applied to all priority files
- [ ] CRITICAL issues fixed inline
- [ ] IMPORTANT/MINOR logged in ai/review/tui.md
- [ ] Tests added for any fixed bugs

---

## Task: S7-3 Review agent/ module

**Sprint:** 7
**Depends on:** S7-1
**Status:** PENDING

### Description

Review core agent loop. Focus on state management and error recovery.

### Files to Review

- [ ] mod.rs (735L) - main loop, message handling
- [ ] instructions.rs - AGENTS.md loading

### Acceptance Criteria

- [ ] State transitions documented
- [ ] Error recovery paths verified
- [ ] CRITICAL issues fixed inline
- [ ] Logged in ai/review/agent.md

---

## Task: S7-4 Review provider/ module

**Sprint:** 7
**Depends on:** S7-1
**Status:** PENDING

### Description

Review provider abstraction. Focus on error handling and registry.

### Files to Review

- [ ] client.rs (382L) - API client
- [ ] registry.rs (676L) - model registry
- [ ] prefs.rs (403L) - preferences

### Acceptance Criteria

- [ ] API error handling verified
- [ ] Retry logic reviewed
- [ ] Registry edge cases checked
- [ ] Logged in ai/review/provider.md

---

## Task: S7-5 Review remaining modules

**Sprint:** 7
**Depends on:** S7-1
**Status:** PENDING

### Description

Quick review of lower-risk modules using checklist.

### Modules

- [ ] session/store.rs (565L) - persistence
- [ ] config/mod.rs (532L) - config loading
- [ ] compaction/\*.rs (~500L) - pruning
- [ ] mcp/\*.rs (~300L) - MCP client
- [ ] skill/\*.rs (~200L) - skill loading

### Acceptance Criteria

- [ ] Checklist spot-checks on each
- [ ] CRITICAL issues fixed
- [ ] Logged in ai/review/misc.md

---

## Task: S7-6 Performance profiling

**Sprint:** 7
**Depends on:** S7-1 (can run parallel with S7-2 through S7-5)
**Status:** PENDING

### Description

Profile key performance metrics. Run parallel with code reviews.

### Measurements

- [ ] Startup time: `hyperfine './target/release/ion --help'`
- [ ] First response latency (subjective)
- [ ] Large chat history scrolling
- [ ] Memory baseline with `cargo instruments` or heaptrack

### Acceptance Criteria

- [ ] Metrics baselined in ai/review/performance.md
- [ ] Bottlenecks identified (if any)
- [ ] Obvious optimizations noted

---

## Task: S7-7 Consolidate and plan

**Sprint:** 7
**Depends on:** S7-2, S7-3, S7-4, S7-5, S7-6
**Status:** DONE

### Description

Aggregate all findings, create action plan.

### Acceptance Criteria

- [x] ai/review/SUMMARY.md with all findings by severity
- [x] All critical and important issues fixed (a916d76)
- [x] Refactor recommendations documented
- [x] Sprint 8 scope defined

## Sprint 8: Core Loop & TUI Deep Review

**Goal:** Zero known bugs in core loop and TUI; documented understanding of key flows.
**Source:** Sprint 7 revealed automated reviews find theoretical issues, not real bugs.
**Status:** ACTIVE

### Demoable Outcomes

- [ ] All critical bugs fixed (with commit SHAs)
- [ ] ai/review/sprint8-summary.md with findings
- [ ] Manual test checklist with pass/fail status

### Approach

No subagents. Manual code reading, tracing actual user flows. S8-1, S8-2, S8-3 can run in parallel.

---

## Task: S8-1 Trace Agent Turn Loop

**Sprint:** 8
**Depends on:** none
**Status:** PENDING

### Description

Read `src/agent/mod.rs` line by line. Trace what happens each turn.

### Flows to Trace

1. `run_task()` entry → user message added → first turn
2. `execute_turn()` → stream or complete → assistant blocks collected
3. Tool calls → `execute_tools_parallel()` → results collected → loop continues
4. Cancellation mid-stream → abort token checked
5. Cancellation mid-tool → JoinSet aborted
6. Retry on transient error → backoff → retry
7. Compaction trigger → messages pruned

### Checklist

- [ ] Cancellation works mid-stream (abort_token checked in select!)
- [ ] Cancellation works mid-tool (JoinSet aborted)
- [ ] Tool results ordered correctly (index preserved)
- [ ] Retry logic actually retries (delay, counter increment)
- [ ] Compaction triggers at threshold (token check)
- [ ] Queued user messages drain between turns (message_queue Arc<Mutex<Vec>>)
- [ ] Malformed tool call from API handled gracefully
- [ ] Individual tool timeout doesn't hang entire execution

### Output

Issues found → ai/review/agent-deep.md

---

## Task: S8-2 Trace TUI Event Flow

**Sprint:** 8
**Depends on:** none
**Status:** PENDING

### Description

Read `src/tui/events.rs` and `src/tui/session.rs`. Trace key → action → state.

### Flows to Trace

1. Char input → insert into buffer → cursor update
2. Enter → input validated → `run_agent_task()` spawned
3. Slash command → recognized → action (model/provider/clear/quit)
4. Esc during task → `abort_token.cancel()` called
5. Esc Esc (idle) → input cleared
6. Ctrl+C Ctrl+C (idle, empty) → quit
7. Up/Down → history navigation or cursor movement
8. Mode transitions → state consistency

### Checklist

- [ ] Double-Esc clears input
- [ ] Double-Ctrl+C quits when idle (with empty input)
- [ ] Esc cancels running task
- [ ] Mode transitions (Input↔Selector↔Approval) reset relevant state
- [ ] History works with multiline input
- [ ] Mid-task message injection via Enter works
- [ ] Large paste creates blob placeholder correctly
- [ ] Terminal resize during operation doesn't corrupt display

### Output

Issues found → ai/review/tui-events-deep.md

---

## Task: S8-3 Trace TUI Rendering

**Sprint:** 8
**Depends on:** none
**Status:** PENDING

### Description

Read `src/tui/render.rs` and `src/tui/composer/`. Trace display updates.

### Flows to Trace

1. Empty state → startup header inserted
2. User submits → message in list → `take_chat_inserts()` → scrollback
3. Agent streams → deltas → message entry updated
4. Tool call → start event → result event → display
5. Progress line → spinner vs summary
6. Cursor position → `calculate_cursor_pos()` → screen coords

### Checklist

- [ ] Streaming updates display incrementally
- [ ] Cursor position correct in multiline (visual line wrapping)
- [ ] Scroll works with 100+ messages
- [ ] Progress shows accurate elapsed/tokens
- [ ] Tool results formatted (diffs, code blocks)
- [ ] Large tool output truncated sensibly

### Output

Issues found → ai/review/tui-render-deep.md

---

## Task: S8-4 Integration Testing

**Sprint:** 8
**Depends on:** S8-1, S8-2, S8-3
**Status:** PENDING

### Description

Actually use the app. Hit each flow manually.

### Test Scenarios

1. Cold start → setup flow → model selection → first message
2. Multi-turn with tool use (read, write, edit)
3. Cancel mid-response
4. Long conversation (20+ turns) → compaction
5. `/resume` → load previous session
6. Ctrl+P → switch provider → fetch models

### Output

Bugs encountered → ai/review/integration-bugs.md

---

## Task: S8-5 Fix and Document

**Sprint:** 8
**Depends on:** S8-1, S8-2, S8-3, S8-4
**Status:** PENDING

### Description

Collect issues from S8-1 through S8-4. Fix critical/important. Document the rest.

### Acceptance Criteria

- [ ] All critical issues fixed (commit SHAs in summary)
- [ ] All important issues fixed or added to `tk`
- [ ] Minor issues logged for future
- [ ] ai/review/sprint8-summary.md complete
- [ ] STATUS.md updated

## Sprint 9: Feature Parity & Extensibility

**Goal:** Web fetch, skills spec compliance, subagents, API caching
**Target:** Pi + Claude Code feature blend
**Source:** Competitive analysis 2026-01-25
**Status:** ACTIVE

### Demoable Outcomes

- [ ] `web_fetch` tool works (fetch URL, extract content)
- [ ] Skills load with YAML frontmatter (agentskills.io spec)
- [ ] Progressive disclosure: summaries at startup, full on activation
- [ ] Subagent spawning with tool restrictions
- [ ] Anthropic cache_control in requests

### Priority Order

| Priority | Task                       | Rationale                              |
| -------- | -------------------------- | -------------------------------------- |
| 1        | Web fetch tool             | Core utility, all competitors have it  |
| 2        | Skills YAML frontmatter    | agentskills.io spec compliance         |
| 3        | Skills progressive load    | Context efficiency, spec compliance    |
| 4        | Subagents                  | Task delegation, matches Claude Code   |
| 5        | Anthropic caching          | Cost savings, better context           |
| 6        | Image attachment           | Model detection exists, just needs UI  |
| 7        | Skill/command autocomplete | Fuzzy search on / and // prefix        |
| 8        | File path autocomplete     | @ syntax with path picker              |
| 9        | More providers             | OpenRouter covers most, lower priority |

---

## Task: S9-1 Web Fetch Tool

**Sprint:** 9
**Depends on:** none
**Status:** PENDING

### Description

Add `web_fetch` builtin tool. Design first, then implement.

### Design Considerations

1. **Fetch method**: reqwest with timeout (30s connect, 60s total)
2. **Content extraction**: html2text or similar, not raw HTML
3. **Content mode**: Option to summarize via fast model (like Claude Code's Haiku approach)
4. **Security**: No dynamic URL construction, only user-provided or from search results
5. **Size limit**: Cap response at ~100KB, truncate with notice

### Interface

```rust
struct WebFetchArgs {
    url: String,
    query: Option<String>,  // Question to answer about the page
    raw: Option<bool>,      // Return raw text vs summarized
}
```

### Acceptance Criteria

- [ ] Tool registered in orchestrator
- [ ] Fetches URL with proper timeout/error handling
- [ ] Extracts readable text from HTML
- [ ] Optional query-focused summarization
- [ ] Size limit enforced
- [ ] Tests for common sites (GitHub, docs)

---

## Task: S9-2 Skills YAML Frontmatter

**Sprint:** 9
**Depends on:** none
**Status:** PENDING

### Description

Update skill loader to parse YAML frontmatter per agentskills.io spec.

### Current Format (XML)

```xml
<skill>
    <name>skill-name</name>
    <description>...</description>
    <prompt>...</prompt>
</skill>
```

### Target Format (YAML)

```yaml
---
name: skill-name
description: What it does and when to use it
allowed-tools: Bash(git:*) Read Write
---
# Skill instructions in Markdown body
```

### Fields to Support

| Field           | Required | Notes                             |
| --------------- | -------- | --------------------------------- |
| `name`          | Yes      | Max 64 chars, lowercase + hyphens |
| `description`   | Yes      | Max 1024 chars                    |
| `allowed-tools` | No       | Space-delimited tool patterns     |
| `license`       | No       | For attribution                   |
| `compatibility` | No       | Environment requirements          |
| `metadata`      | No       | Arbitrary key-value               |

### Acceptance Criteria

- [ ] YAML frontmatter parsed correctly
- [ ] Markdown body extracted as prompt
- [ ] Old XML format still works (migration period)
- [ ] All fields accessible in Skill struct
- [ ] Tests for valid/invalid frontmatter

---

## Task: S9-3 Skills Progressive Disclosure

**Sprint:** 9
**Depends on:** S9-2
**Status:** PENDING

### Description

Load only name+description at startup (~100 tokens per skill). Full body loaded on activation.

### Current Behavior

All skills loaded fully at startup, all prompts in memory.

### Target Behavior

1. **Startup**: Load frontmatter only (name, description)
2. **Context**: Include skill summaries in system prompt
3. **Activation**: Load full SKILL.md body on demand
4. **Cache**: Keep loaded skills in memory for session

### Implementation

```rust
struct SkillSummary {
    name: String,
    description: String,
    path: PathBuf,  // For lazy loading
}

struct SkillRegistry {
    summaries: Vec<SkillSummary>,     // Always loaded
    loaded: HashMap<String, Skill>,   // On-demand
}

impl SkillRegistry {
    fn get_or_load(&mut self, name: &str) -> Result<&Skill>;
}
```

### Acceptance Criteria

- [ ] Only frontmatter parsed at startup
- [ ] System prompt includes skill summaries
- [ ] Full skill loaded when activated
- [ ] Memory usage lower with many skills
- [ ] Benchmark: 10 skills vs current

---

## Task: S9-4 Subagent Support

**Sprint:** 9
**Depends on:** none
**Status:** PENDING

### Description

Enable spawning isolated agent instances with restricted tools/model.

### Design Considerations

1. **Isolation**: Separate message history, own abort token
2. **Tool restriction**: Subagent only gets specified tools
3. **Model override**: Can use different/cheaper model
4. **Result aggregation**: Parent receives subagent output as tool result
5. **No nesting**: Subagents cannot spawn subagents (prevent runaway)

### Interface

```rust
struct SubagentConfig {
    name: String,
    tools: Vec<String>,           // Tool whitelist
    model: Option<String>,        // Override model
    system_prompt: Option<String>, // Additional context
    max_turns: usize,             // Iteration limit (default 10)
}

// Called as tool:
struct SpawnSubagentArgs {
    config: String,  // Config name from subagents/
    task: String,    // Task description
}
```

### Acceptance Criteria

- [ ] Subagent spawns with isolated state
- [ ] Tools restricted to whitelist
- [ ] Model override works
- [ ] Max turns enforced
- [ ] Result returned to parent
- [ ] No recursive spawning

---

## Task: S9-5 Anthropic Cache Control

**Sprint:** 9
**Depends on:** none
**Status:** PENDING

### Description

Pass cache_control to Anthropic API for system prompt caching.

### Current State

- `supports_cache` field on models (from registry)
- `prefer_cache` in filter preferences
- **No cache_control in requests**

### Target

```rust
// In message conversion for Anthropic
if provider == Anthropic && model.supports_cache {
    system_message.cache_control = Some(CacheControl::Ephemeral);
}
```

### Verification

- Check llm-connector supports cache_control
- If not, extend or use raw reqwest for Anthropic

### Acceptance Criteria

- [ ] cache_control passed for Anthropic requests
- [ ] System prompt cached on multi-turn
- [ ] Cache tokens reported in usage
- [ ] No regression for other providers

---

## Task: S9-6 Image Attachment

**Sprint:** 9
**Depends on:** none
**Status:** PENDING

### Description

Allow attaching images to messages for vision-capable models.

### Current State

- `supports_vision` field on models
- `ContentBlock::Image` variant exists
- **No UI for attachment**

### Design

1. **Attachment syntax**: `@image:path/to/file.png` in input
2. **Validation**: Check model supports vision before sending
3. **Encoding**: Base64 encode image data
4. **Size limit**: Warn if >20MB

### Acceptance Criteria

- [ ] @image syntax recognized in input
- [ ] Image encoded as ContentBlock::Image
- [ ] Model vision capability checked
- [ ] Error if model doesn't support vision
- [ ] Size limit enforced

---

## Task: S9-7 Skill/Command Autocomplete

**Sprint:** 9
**Depends on:** S9-2
**Status:** PENDING

### Description

Show autocomplete popup when user types `/` (builtins) or `//` (custom skills).

### Behavior

1. **Single slash `/`**: Show built-in commands (/model, /provider, /clear, /quit, /help, /resume)
2. **Double slash `//`**: Show custom skills from registry
3. **Fuzzy match**: Filter as user types (e.g., `/mo` shows `/model`)
4. **Selection**: Up/Down to navigate, Enter to complete, Esc to dismiss

### UI

- Popup above input area
- Shows name + short description
- Highlights matching chars
- Max 5-7 visible items, scrollable

### Acceptance Criteria

- [ ] Popup appears on / or //
- [ ] Fuzzy filtering works
- [ ] Tab/Enter completes selection
- [ ] Esc dismisses without completing
- [ ] Works with multiline input (popup at cursor line)

---

## Task: S9-8 File Path Autocomplete

**Sprint:** 9
**Depends on:** none
**Status:** PENDING

### Description

Show file path picker when user types `@` in input.

### Behavior

1. **@ trigger**: Show file picker popup
2. **@path**: Filter to files matching path prefix
3. **Directory nav**: Enter on dir descends, Backspace ascends
4. **Completion**: Enter on file inserts `@path/to/file`

### Design

- Use ignore crate for .gitignore-aware listing
- Cache directory listings for performance
- Show file type indicators (dir, file, symlink)

### Acceptance Criteria

- [ ] @ triggers file picker
- [ ] Typing filters files
- [ ] Enter completes with full path
- [ ] Directories navigable
- [ ] .gitignore respected

---

## Task: S9-9 Review and Document

**Sprint:** 9
**Depends on:** S9-1 through S9-8
**Status:** PENDING

### Description

After implementing features, compare to competitors and document gaps.

### Comparison Points

- [ ] Web fetch vs Claude Code/Codex
- [ ] Skills vs agentskills.io reference impl
- [ ] Subagents vs Claude Code Task agent
- [ ] Caching effectiveness (token savings)

### Acceptance Criteria

- [ ] ai/review/sprint9-summary.md with results
- [ ] Feature comparison table updated
- [ ] Known gaps documented
- [ ] Next priorities identified

## Sprint 10: Stabilization & Refactor

**Goal:** Fix known issues from code review, refactor large functions, improve code quality
**Source:** Code review 2026-01-26
**Status:** COMPLETE (2026-01-26)

### Demoable Outcomes

- [x] All review issues fixed or documented
- [x] `render_selector_shell` split into 3 functions (308 → ~100 lines each)
- [x] `stream_response` decomposed (248 → ~80 lines each)
- [x] Formatting helpers extracted to util.rs
- [x] All tests pass (104), 0 clippy warnings

### Issue Summary

| Category    | Count | Severity |
| ----------- | ----- | -------- |
| Agent       | 2     | Low      |
| Input       | 2     | Low      |
| Session     | 2     | Low      |
| Persistence | 1     | Low      |

### Refactoring Summary

| Target                  | Current   | After         | Effort |
| ----------------------- | --------- | ------------- | ------ |
| `render_selector_shell` | 308 lines | ~3×100        | Medium |
| `stream_response`       | 248 lines | ~3×80         | Medium |
| `render_progress`       | 145 lines | ~80 + helpers | Low    |
| Format duplication      | 2 places  | 1 helper      | Quick  |

---

## Task: S10-1 Extract formatting helpers

**Sprint:** 10
**Depends on:** none
**Status:** PENDING
**Effort:** Quick win (15 min)

### Description

Extract duplicated elapsed time and token formatting to util.rs.

### Duplications Found

1. **Elapsed time** (render.rs:226-230, 296-300):

   ```rust
   let secs = elapsed.as_secs();
   if secs >= 60 {
       format!("{}m {}s", secs / 60, secs % 60)
   } else {
       format!("{}s", secs)
   }
   ```

2. **Token stats** (render.rs:234-239, 304-309):
   ```rust
   stats.push(format!("↑ {}", format_tokens(self.input_tokens)));
   stats.push(format!("↓ {}", format_tokens(self.output_tokens)));
   ```

### Implementation

Add to `src/tui/util.rs`:

```rust
/// Format seconds as human-readable duration (e.g., "1m 30s" or "45s")
pub(super) fn format_elapsed(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Format token stats as "↑ Xk · ↓ Yk" string
pub(super) fn format_token_stats(input: usize, output: usize) -> String {
    let mut parts = Vec::new();
    if input > 0 {
        parts.push(format!("↑ {}", format_tokens(input)));
    }
    if output > 0 {
        parts.push(format!("↓ {}", format_tokens(output)));
    }
    parts.join(" · ")
}
```

### Acceptance Criteria

- [ ] `format_elapsed()` in util.rs
- [ ] `format_token_stats()` in util.rs
- [ ] render.rs uses helpers (no duplication)
- [ ] Tests for edge cases (0s, 59s, 60s, 3600s)

---

## Task: S10-2 Split render_selector_shell

**Sprint:** 10
**Depends on:** S10-1
**Status:** PENDING
**Effort:** Medium (45 min)

### Description

Split 308-line `render_selector_shell` into focused functions.

### Current Structure

```rust
fn render_selector_shell(&mut self, frame: &mut Frame) {
    // ~50 lines: layout calculation
    // ~80 lines: provider selector rendering
    // ~100 lines: model selector rendering
    // ~80 lines: session selector rendering
}
```

### Target Structure

```rust
fn render_selector_shell(&mut self, frame: &mut Frame) {
    let layout = self.selector_layout(frame.area());
    match self.selector_page {
        SelectorPage::Provider => self.render_provider_selector(frame, layout),
        SelectorPage::Model => self.render_model_selector(frame, layout),
        SelectorPage::Session => self.render_session_selector(frame, layout),
    }
}

fn selector_layout(&self, area: Rect) -> SelectorLayout { ... }
fn render_provider_selector(&mut self, frame: &mut Frame, layout: SelectorLayout) { ... }
fn render_model_selector(&mut self, frame: &mut Frame, layout: SelectorLayout) { ... }
fn render_session_selector(&mut self, frame: &mut Frame, layout: SelectorLayout) { ... }
```

### Acceptance Criteria

- [ ] `SelectorLayout` struct holds computed areas
- [ ] Each selector type in own function (~80-100 lines)
- [ ] Dispatcher function <30 lines
- [ ] Visual behavior unchanged
- [ ] Manual test: all three selectors work

---

## Task: S10-3 Decompose stream_response

**Sprint:** 10
**Depends on:** none
**Status:** PENDING
**Effort:** Medium (45 min)

### Description

Split 248-line `stream_response` into focused functions (relates to tk-mmpr).

### Current Structure

```rust
async fn stream_response(...) -> Result<(Vec<ContentBlock>, Vec<ToolCallEvent>)> {
    // ~30 lines: setup, tool defs
    // ~120 lines: streaming path with retry
    // ~80 lines: non-streaming fallback with retry
    // ~20 lines: result assembly
}
```

### Target Structure

```rust
async fn stream_response(...) -> Result<(Vec<ContentBlock>, Vec<ToolCallEvent>)> {
    let request = self.build_chat_request(session, thinking).await;
    let use_streaming = self.should_use_streaming(&request);

    if use_streaming {
        self.stream_with_retry(request, tx, abort_token).await
    } else {
        self.complete_with_retry(request, tx, abort_token).await
    }
}

async fn build_chat_request(...) -> ChatRequest { ... }
fn should_use_streaming(&self, request: &ChatRequest) -> bool { ... }
async fn stream_with_retry(...) -> Result<...> { ... }
async fn complete_with_retry(...) -> Result<...> { ... }
```

### Acceptance Criteria

- [ ] Request building extracted
- [ ] Streaming/non-streaming decision extracted
- [ ] Each retry loop in own function
- [ ] Retry logic unchanged
- [ ] Tests pass

---

## Task: S10-4 Fix review issues (Agent)

**Sprint:** 10
**Depends on:** none
**Status:** PENDING
**Effort:** Low (20 min)

### Description

Fix agent-related issues found in code review.

### Issues

1. **Queued messages don't update token display** (Low)
   - Location: `agent/mod.rs:302-307`
   - Problem: Messages from queue don't emit TokenUsage until assistant responds
   - Fix: Call `emit_token_usage()` after draining queue

2. **JoinSet panic error unclear** (Low)
   - Location: `agent/mod.rs:705`
   - Problem: If tool task panics, `res?` returns confusing JoinError
   - Fix: Wrap with context: `.map_err(|e| anyhow!("Tool execution panicked: {}", e))?`

### Acceptance Criteria

- [ ] Token usage updates immediately when queue drained
- [ ] Panic error message is clear
- [ ] Tests pass

---

## Task: S10-5 Fix review issues (Input/Session)

**Sprint:** 10
**Depends on:** none
**Status:** PENDING
**Effort:** Low (30 min)

### Description

Fix input and session issues found in code review.

### Issues

1. **Blob placeholder collision** (Low)
   - Location: `composer/buffer.rs:109`
   - Problem: `replace()` would match user-typed `[Pasted text #1]`
   - Fix: Use unique delimiter unlikely to be typed: `⟦paste:1⟧`

2. **History loses blobs** (Low)
   - Location: `events.rs:297`
   - Problem: Placeholders stored, blobs lost on history reload
   - Fix: Store resolved content in history (larger but complete)

3. **Model registry only recreated for OpenRouter** (Low)
   - Location: `session.rs:469-474`
   - Problem: Other providers keep old registry with wrong API key
   - Fix: Always recreate registry on provider switch

4. **Load session loses tool details** (Low)
   - Location: `session.rs:554-558`
   - Problem: ToolCall only saves name, not args/output
   - Decision: Document as known limitation (full history in session file)

### Acceptance Criteria

- [ ] Blob placeholder uses unique delimiter
- [ ] History stores full resolved content
- [ ] Registry recreated on any provider switch
- [ ] Tool detail limitation documented in code comment

---

## Task: S10-6 Add WAL mode to SQLite

**Sprint:** 10
**Depends on:** none
**Status:** PENDING
**Effort:** Quick (5 min)

### Description

Enable WAL mode for better concurrent access to session database.

### Implementation

Add to `session/store.rs` in `init_schema()`:

```rust
self.db.execute("PRAGMA journal_mode=WAL", [])?;
```

### Acceptance Criteria

- [ ] WAL mode enabled on database open
- [ ] Existing sessions still readable
- [ ] No performance regression

---

## Task: S10-7 Verification and cleanup

**Sprint:** 10
**Depends on:** S10-1, S10-2, S10-3, S10-4, S10-5, S10-6
**Status:** PENDING

### Description

Final verification of all changes.

### Checklist

- [ ] `cargo build --release` succeeds
- [ ] `cargo test` all pass
- [ ] `cargo clippy` 0 warnings
- [ ] Manual test: start → model select → chat → tool use → cancel → resume
- [ ] STATUS.md updated
- [ ] Commit with descriptive message

### Acceptance Criteria

- [ ] All S10 tasks complete
- [ ] No regressions
- [ ] ai/STATUS.md reflects current state

---

## Sprint 11: TUI v2 - Remove ratatui, Pure Crossterm

**Goal:** Remove ratatui dependency, use crossterm directly. Native scroll/search work.
**Source:** `ai/design/tui-v2.md`
**Status:** PLANNED

### Overview

Key behaviors:

- Chat prints to native scrollback (scroll/search work)
- Resize: clear scrollback + reprint everything (no duplicates)
- Streaming: print lines to scrollback as received, input hidden during stream
- Exit: clear bottom UI only, chat stays in scrollback

### Dependency Graph (Revised)

```
Phase 1: Foundation
S11-T1 (StyledLine API)

Phase 2: Rendering (sequential - highlight must come before chat_renderer)
S11-T2 (highlight functions) ──> S11-T3 (markdown renderer) ──> S11-T4 (chat_renderer)

Phase 3: Core Behavior
S11-T5 (resize handler)
S11-T6 (streaming to scrollback)

Phase 4: Selectors
S11-T7 (selector base) ──> S11-T8 (provider) ──> S11-T9 (model) ──> S11-T10 (session)

Phase 5: Cleanup
S11-T11 (delete ratatui render path)
S11-T12 (delete widgets)
S11-T13 (remove ratatui from Cargo.toml)
S11-T14 (dead code + exit cleanup)
```

---

## Task: S11-T1 Consolidate StyledLine API

**Sprint:** 11
**Depends on:** none
**Status:** PENDING

### Description

terminal.rs has `StyledLine`/`StyledSpan`. Make it the canonical type, remove ratatui conversion functions.

### Implementation

```rust
// Add to StyledLine
pub fn println(&self) -> io::Result<()> {
    self.write_to(&mut io::stdout())?;
    writeln!(io::stdout())
}

// Add to StyledSpan
pub fn bold(content: impl Into<String>) -> Self { ... }
pub fn italic(content: impl Into<String>) -> Self { ... }
```

### Acceptance Criteria

- [ ] Remove `convert_style()`, `convert_span()`, `convert_line()`, `convert_lines()`
- [ ] Remove `print_lines_to_scrollback()` (will be replaced)
- [ ] Add `StyledLine::println()` convenience method
- [ ] Add `StyledSpan::bold()`, `StyledSpan::italic()` builders
- [ ] `cargo build` passes

**Files:** `src/tui/terminal.rs`

---

## Task: S11-T2 Migrate highlight functions to StyledLine

**Sprint:** 11
**Depends on:** S11-T1
**Status:** PENDING

### Description

Migrate syntax highlighting functions to return `StyledLine`. Keep syntect integration.

### Implementation

```rust
pub fn highlight_diff_line(line: &str) -> StyledLine {
    let color = if line.starts_with('+') { Color::Green } else { ... };
    StyledLine::new(vec![StyledSpan::colored(line, color)])
}

pub fn highlight_line(line: &str, syntax: &str) -> StyledLine {
    // Same syntect logic, output to StyledSpan instead of ratatui Span
}
```

### Acceptance Criteria

- [ ] `highlight_diff_line()` returns `StyledLine`
- [ ] `highlight_line()` returns `StyledLine`
- [ ] `highlight_code_block()` returns `Vec<StyledLine>`
- [ ] Syntect highlighting preserved
- [ ] `cargo build` passes

**Files:** `src/tui/highlight.rs`

---

## Task: S11-T3 Replace tui-markdown with pulldown-cmark renderer

**Sprint:** 11
**Depends on:** S11-T2
**Status:** PENDING

### Description

tui-markdown depends on ratatui. Replace with custom renderer using pulldown-cmark.

### Implementation

```rust
pub fn render_markdown(text: &str) -> Vec<StyledLine> {
    use pulldown_cmark::{Parser, Event, Tag};

    let parser = Parser::new(text);
    let mut lines = Vec::new();
    let mut current_line = Vec::new();

    for event in parser {
        match event {
            Event::Text(text) => current_line.push(StyledSpan::raw(text.to_string())),
            Event::Code(code) => current_line.push(StyledSpan::colored(code.to_string(), Color::Yellow)),
            Event::Start(Tag::Strong) => { /* set bold flag */ }
            Event::Start(Tag::Emphasis) => { /* set italic flag */ }
            Event::Start(Tag::CodeBlock(kind)) => { /* delegate to highlight_code_block */ }
            Event::SoftBreak | Event::HardBreak => {
                lines.push(StyledLine::new(current_line));
                current_line = Vec::new();
            }
            // ... handle headers, lists, etc.
        }
    }
    lines
}
```

### Acceptance Criteria

- [ ] Add `pulldown-cmark` to Cargo.toml
- [ ] `render_markdown()` handles: bold, italic, code spans, code blocks, headers, lists
- [ ] Code blocks delegate to `highlight_code_block()` for syntax highlighting
- [ ] No tui-markdown import
- [ ] Manual test: agent response with markdown renders correctly

**Files:** `src/tui/highlight.rs`, `Cargo.toml`

**Estimate:** 150-250 lines of new code

---

## Task: S11-T4 Migrate chat_renderer to StyledLine

**Sprint:** 11
**Depends on:** S11-T3
**Status:** PENDING

### Description

Change `ChatRenderer::build_lines()` to return `Vec<StyledLine>`.

### Implementation

```rust
pub fn build_lines(entries: &[MessageEntry], ...) -> Vec<StyledLine> {
    for entry in entries {
        match entry.sender {
            Sender::User => {
                // Cyan prefix, dim text
                chat_lines.push(StyledLine::new(vec![
                    StyledSpan::colored("> ", Color::Cyan),
                    StyledSpan::dim(text),
                ]));
            }
            Sender::Agent => {
                // Use render_markdown() from T3
                chat_lines.extend(render_markdown(&text));
            }
            Sender::Tool => {
                // Handle ANSI escapes in tool output
                chat_lines.extend(parse_ansi_line(&line));
            }
            // ...
        }
    }
}

// Replace ansi-to-tui with simple SGR parser
fn parse_ansi_line(line: &str) -> StyledLine {
    // Parse SGR sequences (colors), output StyledSpans
}
```

### Acceptance Criteria

- [ ] `build_lines()` returns `Vec<StyledLine>`
- [ ] User messages: cyan `>` prefix, dim text
- [ ] Agent messages: markdown rendered via `render_markdown()`
- [ ] Tool messages: ANSI colors preserved (replace ansi-to-tui)
- [ ] System messages: dim or red for errors
- [ ] No ratatui imports in chat_renderer.rs
- [ ] `cargo test` passes

**Files:** `src/tui/chat_renderer.rs`

---

## Task: S11-T5 Implement resize handler

**Sprint:** 11
**Depends on:** S11-T4
**Status:** PENDING

### Description

On resize: clear scrollback + screen, reprint all chat from message_list at new width.

### Implementation

```rust
fn handle_resize(&mut self) -> io::Result<()> {
    // Clear scrollback and screen (CSI 3J + 2J + H)
    print!("\x1b[3J\x1b[2J\x1b[H");
    io::stdout().flush()?;

    // Reset state to force full reprint
    self.rendered_entries = 0;
    self.header_inserted = false;

    Ok(())
}
```

### Acceptance Criteria

- [ ] `Event::Resize` triggers `handle_resize()`
- [ ] Uses `\x1b[3J\x1b[2J\x1b[H` to clear scrollback + screen
- [ ] `rendered_entries` reset to 0
- [ ] `header_inserted` reset to false
- [ ] Next render loop reprints all entries
- [ ] Bottom UI re-renders at correct position
- [ ] No duplicate content after resize
- [ ] Manual test: resize terminal, all chat re-renders at new width

**Files:** `src/tui/events.rs`, `src/main.rs`

---

## Task: S11-T6 Streaming response to scrollback

**Sprint:** 11
**Depends on:** S11-T4
**Status:** PENDING

### Description

Print streaming response lines directly to scrollback as received. Hide input during streaming.

### Implementation

Add to App struct:

```rust
pub streaming_buffer: String,
```

In agent event handler:

```rust
AgentEvent::Delta(text) => {
    self.streaming_buffer.push_str(&text);
    while let Some(pos) = self.streaming_buffer.find('\n') {
        let line: String = self.streaming_buffer.drain(..=pos).collect();
        // Print line with leading space (agent indent)
        print!(" {}", line);
        io::stdout().flush()?;
    }
    // Re-render bottom UI after printing
    self.render_bottom_ui()?;
}
AgentEvent::Complete => {
    // Flush remaining partial line
    if !self.streaming_buffer.is_empty() {
        println!(" {}", self.streaming_buffer);
        self.streaming_buffer.clear();
    }
    println!(); // Blank separator
}
```

### Acceptance Criteria

- [ ] Add `streaming_buffer: String` to App struct
- [ ] Streaming text prints line-by-line to scrollback
- [ ] Each complete line prints immediately (not per-token)
- [ ] Input area hidden during streaming (progress shows instead)
- [ ] Esc cancels streaming (abort_token)
- [ ] Keystrokes during streaming queued to `message_queue`
- [ ] Bottom UI re-renders after each line printed
- [ ] On complete: flush partial line, add blank separator
- [ ] Manual test: send message, watch lines stream

**Files:** `src/tui/mod.rs`, `src/tui/session.rs`, `src/main.rs`

---

## Task: S11-T7 Selector base renderer

**Sprint:** 11
**Depends on:** S11-T1
**Status:** PENDING

### Description

Create reusable selector renderer for all pickers.

### Implementation

```rust
struct SelectorItem {
    label: String,
    hint: Option<String>,
    enabled: bool,
}

fn render_selector_list<W: Write>(
    w: &mut W,
    items: &[SelectorItem],
    selected: usize,
    start_row: u16,
    height: u16,
) -> io::Result<()> {
    for (i, item) in items.iter().take(height as usize).enumerate() {
        execute!(w, MoveTo(0, start_row + i as u16))?;
        if i == selected {
            execute!(w, SetBackgroundColor(Color::DarkGrey))?;
            write!(w, "▸ ")?;
        } else {
            write!(w, "  ")?;
        }
        if item.enabled {
            write!(w, "{}", item.label)?;
        } else {
            execute!(w, SetAttribute(Attribute::Dim))?;
            write!(w, "{}", item.label)?;
        }
        if let Some(hint) = &item.hint {
            execute!(w, SetAttribute(Attribute::Dim))?;
            write!(w, " {}", hint)?;
        }
        execute!(w, ResetColor, SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}
```

### Acceptance Criteria

- [ ] `SelectorItem` struct defined
- [ ] `render_selector_list()` renders items with selection highlight
- [ ] Supports enabled/disabled items (dim when disabled)
- [ ] Supports optional hint text
- [ ] `cargo build` passes

**Files:** `src/tui/render.rs`

---

## Task: S11-T8 Port provider picker to crossterm

**Sprint:** 11
**Depends on:** S11-T7
**Status:** PENDING

### Description

Migrate provider picker to use `render_selector_list()`.

### Acceptance Criteria

- [ ] Provider picker uses `render_selector_list()`
- [ ] Authenticated providers show green dot
- [ ] Unauthenticated providers show "set API_KEY" hint
- [ ] Filter input renders with crossterm
- [ ] Arrow keys, Enter, Esc work
- [ ] Manual test: open provider picker, filter, select

**Files:** `src/tui/render.rs`, `src/tui/provider_picker.rs`

---

## Task: S11-T9 Port model picker to crossterm

**Sprint:** 11
**Depends on:** S11-T8
**Status:** PENDING

### Description

Migrate model picker to use `render_selector_list()`.

### Acceptance Criteria

- [ ] Model picker uses `render_selector_list()`
- [ ] Shows model name and context size
- [ ] Loading state renders without ratatui
- [ ] Error state renders without ratatui
- [ ] Manual test: open model picker, filter, select

**Files:** `src/tui/render.rs`, `src/tui/model_picker.rs`

---

## Task: S11-T10 Port session picker to crossterm

**Sprint:** 11
**Depends on:** S11-T9
**Status:** PENDING

### Description

Migrate session picker to use `render_selector_list()`.

### Acceptance Criteria

- [ ] Session picker uses `render_selector_list()`
- [ ] Shows relative time, session ID preview, first message
- [ ] Empty state ("No sessions found") renders
- [ ] After selector closes, bottom UI restored
- [ ] Manual test: open session picker, filter, select

**Files:** `src/tui/render.rs`, `src/tui/session_picker.rs`

---

## Task: S11-T11 Delete ratatui render path

**Sprint:** 11
**Depends on:** S11-T10
**Status:** PENDING

### Description

Remove `draw()` function and all ratatui Frame usage from render.rs.

### Acceptance Criteria

- [ ] `draw()` function deleted
- [ ] `take_snapshot()` deleted
- [ ] `render_progress()` (Frame version) deleted
- [ ] `render_input_or_approval()` deleted
- [ ] `render_status_line()` (Frame version) deleted
- [ ] `render_selector_shell()` deleted
- [ ] `render_help_overlay()` rewritten for crossterm
- [ ] `layout_areas()` deleted
- [ ] `take_chat_inserts()` returns `Vec<StyledLine>` or deleted
- [ ] Only `draw_direct()` and `*_direct()` methods remain
- [ ] `cargo build` passes

**Files:** `src/tui/render.rs`

---

## Task: S11-T12 Delete ratatui widget implementations

**Sprint:** 11
**Depends on:** S11-T11
**Status:** PENDING

### Description

Delete ComposerWidget, FilterInput widget, and unused widgets.

### Acceptance Criteria

- [ ] `ComposerWidget` struct deleted from composer/mod.rs
- [ ] `FilterInput` widget deleted from filter_input.rs
- [ ] `FilterInputState` kept (state management only)
- [ ] Delete `src/tui/widgets/mod.rs` entirely
- [ ] No ratatui Widget trait implementations remain
- [ ] `cargo build` passes

**Files:** `src/tui/composer/mod.rs`, `src/tui/filter_input.rs`, `src/tui/widgets/`

---

## Task: S11-T13 Remove ratatui from Cargo.toml

**Sprint:** 11
**Depends on:** S11-T12
**Status:** PENDING

### Description

Remove ratatui and dependent crates from Cargo.toml.

### Acceptance Criteria

- [ ] `ratatui` removed from Cargo.toml
- [ ] `tui-markdown` removed (replaced by pulldown-cmark in T3)
- [ ] `ansi-to-tui` removed (replaced by custom parser in T4)
- [ ] `pulldown-cmark` added (if not already)
- [ ] No ratatui imports in any file
- [ ] `cargo build` passes
- [ ] `cargo test` passes

**Files:** `Cargo.toml`

---

## Task: S11-T14 Dead code cleanup and exit handling

**Sprint:** 11
**Depends on:** S11-T13
**Status:** PENDING

### Description

Remove unused code and ensure clean exit.

### Acceptance Criteria

- [ ] Remove `LayoutAreas` struct from types.rs
- [ ] Remove unused imports from all tui files
- [ ] On exit: clear bottom UI area only
- [ ] Chat history visible in scrollback after exit
- [ ] No cursor positioning artifacts after exit
- [ ] `cargo clippy` passes with no warnings
- [ ] `cargo test` passes
- [ ] Update ai/STATUS.md

**Files:** `src/tui/types.rs`, `src/main.rs`, all `src/tui/*.rs`

---

## Sprint 11 Validation Checklist

### After Phase 2 (T4 complete):

- [ ] `cargo build`
- [ ] Manual test: basic chat renders correctly

### After Phase 3 (T6 complete):

- [ ] Manual test: resize clears and reprints
- [ ] Manual test: streaming prints line-by-line

### After Phase 4 (T10 complete):

- [ ] Manual test: all three selectors work

### Final (T14 complete):

- [ ] `cargo build`
- [ ] `cargo test`
- [ ] `cargo clippy` - no warnings
- [ ] Binary size reduced (ratatui removed)
- [ ] Manual test: full session flow
- [ ] Manual test: resize terminal - no duplicates
- [ ] Manual test: Cmd+F search works
- [ ] Manual test: native scroll works
- [ ] Manual test: clean exit (no artifacts)

---

## Risk Notes

| Risk                          | Mitigation                                      |
| ----------------------------- | ----------------------------------------------- |
| tui-markdown replacement (T3) | ~200 LOC, handle bold/italic/code/headers/lists |
| ansi-to-tui replacement (T4)  | Simple SGR parser, ~50 LOC                      |
| ComposerWidget cursor math    | Don't change - only output format               |
| Syntect integration           | Keep syntect, change output type only           |

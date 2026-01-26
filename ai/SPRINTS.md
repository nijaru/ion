# Sprint Plan: ion Stabilization & UX

Source: ai/DESIGN.md, ai/STATUS.md, ai/design/inline-viewport.md, ai/design/session-storage.md
Generated: 2026-01-22
Updated: 2026-01-23

## Status

| Sprint | Goal                              | Status     |
| ------ | --------------------------------- | ---------- |
| 0      | TUI Architecture                  | COMPLETE   |
| 1      | Inline Viewport Stabilization     | COMPLETE   |
| 2      | Run State UX & Error Handling     | COMPLETE   |
| 3      | Selector & Resume UX              | COMPLETE   |
| 4      | Visual Polish & Advanced Features | PLANNED    |
| 5      | Session Storage Redesign          | PLANNED    |
| 6      | TUI Module Refactor               | COMPLETE   |
| 7      | Codebase Review & Refactor        | COMPLETE   |
| 8      | Core Loop & TUI Deep Review       | **ACTIVE** |

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

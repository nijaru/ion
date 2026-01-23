# Sprint Plan: ion Stabilization & UX

Source: ai/DESIGN.md, ai/STATUS.md, ai/design/inline-viewport.md, ai/design/diff-highlighting.md
Generated: 2026-01-22

## Sprint 1: Inline Viewport Stabilization
**Goal:** Finalize the inline viewport migration and ensure native terminal behavior.

## Task: Remove residual alternate-screen behavior
**Sprint:** 1
**Depends on:** none

### Description
Ensure the TUI never clears or emulates alternate-screen behavior. Inline viewport should preserve native scrollback, selection, and mouse wheel scrolling. Audit terminal init for EnterAlternateScreen/EnableMouseCapture and remove any full-screen clear/reset calls.

### Acceptance Criteria
- [ ] No alternate screen or mouse capture in terminal init/shutdown
- [ ] Click/drag selection and mouse wheel scrolling work natively
- [ ] Sending a message does not clear the full terminal buffer

---

## Task: Fix viewport spacing and message margins
**Sprint:** 1
**Depends on:** none

### Description
Tighten viewport layout so chat content sits immediately above the viewport without large blank gaps. Add a 1-column left/right margin for messages. Ensure input separators span edge-to-edge.

### Acceptance Criteria
- [ ] No large empty block between last message and viewport
- [ ] Messages render with left/right margin (1 column)
- [ ] Input top/bottom separators extend full width

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
- [ ] Layout computation is separate from rendering
- [ ] Chat, progress, input, and status rendering are in dedicated helpers
- [ ] Behavior matches current UI

---

## Task: Extract chat renderer module
**Sprint:** 1
**Depends on:** Refactor draw into render helpers

### Description
Move chat message formatting (user/agent/tool/system) into a dedicated renderer module. Consider a `ChatRenderer` type with a `build_lines()` method.

### Acceptance Criteria
- [ ] Chat rendering is isolated from `tui::mod` draw logic
- [ ] Output matches current formatting
- [ ] No behavioral regressions in tool/diff rendering

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
- [ ] No extra blank line appears when typing
- [ ] Up retrieves last sent message on first press at top line
- [ ] Down restores newer history and draft without clearing input

## Sprint 2: Run State UX & Error Handling
**Goal:** Provide clear feedback during task execution and handle failures gracefully.

## Task: Progress line state mapping
**Sprint:** 2
**Depends on:** Sprint 1

### Description
Define and render clear run states on the ionizing/progress line: running, cancelling, cancelled, error, completed.

### Acceptance Criteria
- [ ] Running shows normal ionizing text
- [ ] Cancelling shows yellow "Canceling..." with warning indicator
- [ ] Error shows red "Error" with red indicator
- [ ] Completed shows normal completion state

---

## Task: Provider retry/backoff + chat log entries
**Sprint:** 2
**Depends on:** Sprint 1

### Description
Add retry/backoff for transient provider errors (OpenRouter timeouts) and log "Retrying..." in chat history before retry attempts.

### Acceptance Criteria
- [ ] Retries occur on timeout/network errors
- [ ] Chat shows a retry notice before retry attempt
- [ ] Final error appears in chat in red if retries exhausted

---

## Task: Status line accuracy for context usage
**Sprint:** 2
**Depends on:** Sprint 1

### Description
Populate max context length from model registry on model selection. If unknown, show used/0k and omit percent.

### Acceptance Criteria
- [ ] Status line shows used/max when max known
- [ ] Percent only shown when max known
- [ ] Unknown max shows used/0k with no percent

---

## Task: Graceful TUI init error handling
**Sprint:** 2
**Depends on:** Sprint 1

### Description
Replace `unwrap/expect` in TUI init paths with user-visible errors and clean exits. Config/session/client/terminal init failures should surface clearly.

### Acceptance Criteria
- [ ] Config/session/client/terminal init failures surface clearly
- [ ] App exits without panic

## Task: Extract token usage emit helper
**Sprint:** 2
**Depends on:** none

### Description
Consolidate repeated token usage emission in agent loop into a single helper.

### Acceptance Criteria
- [ ] No duplicated token usage emission code in agent loop
- [ ] Behavior unchanged

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
- [ ] /resume opens selector with recent sessions
- [ ] Selecting a session loads it
- [ ] Escape closes selector without changes

---

## Task: --resume/--continue CLI flags
**Sprint:** 3
**Depends on:** none

### Description
Add CLI flags to resume the latest or a specific session by ID. Ensure flags integrate with existing config loading.

### Acceptance Criteria
- [ ] --resume reopens latest session
- [ ] --continue <id> reopens specified session
- [ ] Invalid IDs print a clear error and exit

---

## Task: Fuzzy search ordering (substring first)
**Sprint:** 3
**Depends on:** none

### Description
Prioritize exact substring matches before fuzzy matches in selectors and command suggestions.

### Acceptance Criteria
- [ ] Substring matches appear before fuzzy matches
- [ ] Fuzzy matches appear only when no substring matches exist

## Sprint 4: Visual Polish & Advanced Features
**Goal:** Refine the aesthetic and core architecture.

## Task: Diff highlighting for file edits
**Sprint:** 4
**Depends on:** Sprint 1

### Description
Implement syntax-highlighted diffs for the `edit` tool results. Show additions in green, deletions in red, with word-level highlighting for changed parts of lines.

### Acceptance Criteria
- [ ] ToolResult enhanced with DiffInfo
- [ ] Edit tool populates DiffInfo on success
- [ ] TUI renders styled diff lines in chat

---

## Task: Startup header line
**Sprint:** 4
**Depends on:** Sprint 1

### Description
Render a minimal startup header (ION + version) without pushing the viewport down or clearing scrollback. Mimic Claude Code: header stays above the viewport.

### Acceptance Criteria
- [ ] Header visible at startup
- [ ] No excessive blank lines before viewport
- [ ] Header does not clear scrollback

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
- [ ] Grep tool respects `.gitignore`
- [ ] Glob tool uses `globset` via the `ignore` crate
- [ ] `walkdir` and `glob` dependencies removed

---

## Task: Token counter swap (bpe-openai)
**Sprint:** 4
**Depends on:** none

### Description
Swap `tiktoken-rs` for the faster `bpe-openai` crate.

### Acceptance Criteria
- [ ] Token counting uses `bpe-openai`
- [ ] Performance improved for large messages
- [ ] `tiktoken-rs` dependency removed

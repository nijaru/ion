# Sprint Plan: ion Inline TUI Stabilization

Source: ai/design/inline-viewport.md, ai/design/tui.md, ai/design/interrupt-handling.md
Generated: 2026-01-20

## Sprint 1: Inline Viewport Parity (Native Scrollback + Selection)

## Task: Remove residual alternate-screen behavior

**Sprint:** 1
**Depends on:** none

### Description
Ensure the TUI never clears or emulates alternate-screen behavior. Inline viewport should preserve native scrollback, selection, and mouse wheel scrolling.

### Acceptance Criteria
- [ ] No alternate screen or mouse capture in terminal init/shutdown
- [ ] Click/drag selection and mouse wheel scrolling work natively
- [ ] Sending a message does not clear the full terminal buffer

### Technical Notes
- Audit terminal init for EnterAlternateScreen/EnableMouseCapture
- Remove any full-screen clear/reset calls

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

### Technical Notes
- Re-check Viewport::Inline height calculations
- Avoid padding-based bottom-align that inserts blank lines

---

## Task: Refactor draw into render helpers

**Sprint:** 1
**Depends on:** none

### Description
Split `App::draw` into focused layout/data/render helpers to reduce complexity and isolate regressions.

### Acceptance Criteria
- [ ] Layout computation is separate from rendering
- [ ] Chat, progress, input, and status rendering are in dedicated helpers
- [ ] Behavior matches current UI

### Technical Notes
- Keep helper signatures minimal and pass precomputed data

---

## Task: Extract chat renderer module

**Sprint:** 1
**Depends on:** Refactor draw into render helpers

### Description
Move chat message formatting (user/agent/tool/system) into a dedicated renderer module.

### Acceptance Criteria
- [ ] Chat rendering is isolated from `tui::mod` draw logic
- [ ] Output matches current formatting
- [ ] No behavioral regressions in tool/diff rendering

### Technical Notes
- Consider a `ChatRenderer` type with `build_lines()` method

---

## Task: Fix chat_lines order in draw layout

**Sprint:** 1
**Depends on:** none

### Description
Ensure chat line collection happens before any logic that uses its length, so viewport height calculations are correct and the draw method compiles.

### Acceptance Criteria
- [ ] `chat_lines` is built before any size/height calculations
- [ ] draw() compiles without use-before-define errors

### Technical Notes
- Move chat line assembly above viewport height calculation

---

## Task: Restore write-mode tool approvals

**Sprint:** 1
**Depends on:** none

### Description
Write mode should only auto-allow safe tools and explicitly approved restricted tools. It should not auto-allow all non-bash tools.

### Acceptance Criteria
- [ ] Restricted tools still require approval unless whitelisted
- [ ] No blanket allow for non-bash tools

### Technical Notes
- Remove `tool.name() != "bash"` bypass

---

## Task: Make truncation UTF-8 safe

**Sprint:** 1
**Depends on:** none

### Description
Avoid panics when truncating non-ASCII text in CLI and TUI displays.

### Acceptance Criteria
- [ ] No byte-slicing in truncation helpers
- [ ] Truncation handles Unicode safely

### Technical Notes
- Replace `s[..]` truncation with char-safe logic

---

## Task: Fix input editor phantom line + history navigation

**Sprint:** 1
**Depends on:** none

### Description
Ensure the input editor does not show a phantom blank line when typing. Up/Down history should work on first press and restore draft text correctly.

### Acceptance Criteria
- [ ] No extra blank line appears when typing
- [ ] Up retrieves last sent message on first press at top line
- [ ] Down restores newer history and draft without clearing input

### Technical Notes
- Validate TextArea line counting and cursor placement
- Ensure history navigation respects cursor line positions

## Sprint 2: Run State UX + Error Handling

## Task: Progress line state mapping (running/cancelling/cancelled/error)

**Sprint:** 2
**Depends on:** Sprint 1

### Description
Define and render clear run states on the ionizing/progress line: running, cancelling, cancelled, error, completed.

### Acceptance Criteria
- [ ] Running shows normal ionizing text
- [ ] Cancelling shows yellow "Canceling..." with warning indicator
- [ ] Cancelled shows yellow "Cancelled" state
- [ ] Error shows red "Error" with red indicator
- [ ] Completed shows normal completion state (no error coloring)

### Technical Notes
- Ensure cancel vs error are distinct
- Avoid duplication in status line

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

### Technical Notes
- Keep retry count small (2-3) with exponential backoff
- Ensure abort/cancel stops retries

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

### Technical Notes
- Source model metadata at selection time
- Cache max context alongside active model

---

## Task: Extract token usage emit helper

**Sprint:** 2
**Depends on:** none

### Description
Consolidate repeated token usage emission in agent loop into a single helper.

### Acceptance Criteria
- [ ] No duplicated token usage emission code in agent loop
- [ ] Behavior unchanged

### Technical Notes
- Helper should accept `Session` and `Sender` to avoid extra state

---

## Task: Handle NaN pricing sort in registry

**Sprint:** 2
**Depends on:** Sprint 1

### Description
Avoid panics when sorting by price if a model has NaN pricing data.

### Acceptance Criteria
- [ ] Sorting never panics on NaN pricing
- [ ] Sorting remains stable for invalid price data

### Technical Notes
- Use `total_cmp` or sanitize inputs before compare

---

## Task: Graceful TUI init error handling

**Sprint:** 2
**Depends on:** Sprint 1

### Description
Replace `unwrap/expect` in TUI init paths with user-visible errors and clean exits.

### Acceptance Criteria
- [ ] Config/session/client/terminal init failures surface clearly
- [ ] App exits without panic

### Technical Notes
- Return errors up the call stack and render a fatal message

## Sprint 3: Selector + Resume UX

## Task: /resume selector UI for past sessions

**Sprint:** 3
**Depends on:** none

### Description
Add a /resume command that opens the shared selector UI for prior sessions.

### Acceptance Criteria
- [ ] /resume opens selector with recent sessions
- [ ] Selecting a session loads it
- [ ] Escape closes selector without changes

### Technical Notes
- Reuse selector shell list + filter infrastructure
- Source sessions from session store

---

## Task: --resume/--continue CLI flags

**Sprint:** 3
**Depends on:** none

### Description
Add CLI flags to resume the latest or a specific session by ID.

### Acceptance Criteria
- [ ] --resume reopens latest session
- [ ] --continue <id> reopens specified session
- [ ] Invalid IDs print a clear error and exit

### Technical Notes
- Ensure flags integrate with existing config loading

---

## Task: Fuzzy search ordering (substring first)

**Sprint:** 3
**Depends on:** none

### Description
Prioritize exact substring matches before fuzzy matches in selectors and command suggestions.

### Acceptance Criteria
- [ ] Substring matches appear before fuzzy matches
- [ ] Fuzzy matches appear only when no substring matches exist

### Technical Notes
- Keep scoring stable across providers/models and commands

## Sprint 4: Visual Polish

## Task: Startup header line (ION + version)

**Sprint:** 4
**Depends on:** Sprint 1

### Description
Render a minimal startup header without pushing the viewport down or clearing scrollback.

### Acceptance Criteria
- [ ] Header visible at startup
- [ ] No excessive blank lines before viewport
- [ ] Header does not clear scrollback

### Technical Notes
- Use inline insert to avoid clearing

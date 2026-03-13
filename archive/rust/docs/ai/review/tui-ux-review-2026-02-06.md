# TUI UX Deep Review (2026-02-06)

## Executive Summary

Ion's TUI is a well-architected direct-crossterm implementation at ~10K lines (excluding tests). The core rendering model -- chat to stdout scrollback, bottom UI via cursor positioning -- is sound and matches the approach proven by Claude Code and Codex. The input composer (ropey-backed with blob storage for pastes) is the most polished subsystem. The weakest areas are streaming response rendering (no live display of agent text) and tool output display (no expand/collapse). The codebase has been improving -- the picker trait extraction and completer state refactoring already happened since the Feb 4 analysis.

---

## 1. State Management

### App Struct: 34 Fields

The App struct (`mod.rs:63-134`) has 34 fields. The previous analysis recommended sub-struct decomposition and two have been extracted (`TaskState`, `InteractionState`). Remaining grouping opportunities:

| Proposed Group                   | Fields                                                                        | Lines                |
| -------------------------------- | ----------------------------------------------------------------------------- | -------------------- |
| Already done: `TaskState`        | start_time, tokens, current_tool, retry, thinking                             | `app_state.rs:9-45`  |
| Already done: `InteractionState` | cancel_pending, esc_pending, editor_requested                                 | `app_state.rs:51-58` |
| Could group: `SessionState`      | session, store, session_rx, session_tx                                        | 4 fields             |
| Could group: `ProviderState`     | api_provider, provider_picker, model_picker, model_registry, pending_provider | 5 fields             |

**Assessment:** Adequate. The two extracted sub-structs address the worst grouping issues. Further decomposition has diminishing returns -- each additional sub-struct adds indirection for 2-4 fields. The struct is readable as-is.

### RenderState: Clean Separation

`render_state.rs` is well-designed. It centralizes chat positioning logic, has clear reset methods for different scenarios (`reset_for_new_conversation`, `reset_for_session_load`, `mark_reflow_complete`), and documents the two positioning modes (row-tracking vs scroll) with ASCII diagrams. This is one of the best-documented modules.

### Mode Enum: Minimal

```rust
enum Mode { Input, Selector, HelpOverlay, HistorySearch }
```

Four modes is clean. No unused variants after the permissions v2 cleanup (Mode::Approval was removed).

---

## 2. Event Handling

### Key Dispatch (`events.rs`, 776 lines)

**`handle_input_mode`** (lines 79-521, ~440 lines): This is the largest function in the TUI. It has a `#[allow(clippy::too_many_lines)]` annotation. The function handles:

1. Command completer active state (84-137) -- 53 lines
2. File completer active state (140-207) -- 67 lines
3. Escape/cancel logic (211-226) -- 15 lines
4. Ctrl+C quit logic (230-245) -- 15 lines
5. Ctrl+D quit logic (248-259) -- 11 lines
6. Mode toggle keybindings (262-317) -- 55 lines
7. Shift+Enter newline (322-324) -- 2 lines
8. Enter / slash command dispatch (327-478) -- 151 lines
9. Navigation (483-519) -- 36 lines

The slash command dispatch inside the Enter handler (lines 347-458) is especially long -- 111 lines of inline command matching. This is the single largest improvement opportunity in event handling.

**`handle_selector_mode`** (lines 525-695, 170 lines): Reasonable for its complexity. The `dispatch_picker` helper (710-716) is well-designed, dispatching to the active picker via trait object. Tab/Enter logic has some nesting but is manageable.

**`handle_history_search_mode`** (lines 719-775, 56 lines): Clean and focused. Good separation.

### Issues

| Issue                                                           | Location            | Severity               |
| --------------------------------------------------------------- | ------------------- | ---------------------- |
| `handle_input_mode` too long                                    | `events.rs:79-521`  | Medium                 |
| Slash command dispatch inline in Enter handler                  | `events.rs:347-458` | Medium                 |
| Duplicate completer handling patterns (command vs file)         | `events.rs:84-207`  | Low                    |
| `/compact` does synchronous `compact_messages` in event handler | `events.rs:358`     | Medium -- may block UI |

---

## 3. Rendering

### Architecture Assessment

The dual-mode rendering (row-tracking + scroll mode) in `render/chat.rs` and `run.rs` is the most complex part of the codebase and also the most impressive. It handles:

- Row-tracking mode: content fits on screen, UI follows chat content
- Scroll mode: content exceeds screen, UI pinned to bottom
- Transition between modes when content overflow occurs
- Resize reflow: clear and reprint all chat at new width

**Main loop** (`run.rs:248-519`, 271 lines): The main loop is long but well-structured with clear phases: event poll, size check, update, special-case handling (clear, reflow, selector close, header), chat insertion, synchronized render. The `#[allow(clippy::too_many_lines)]` is justified here -- splitting would fragment the carefully sequenced phases.

### Streaming Response Display

**Critical gap**: Agent text responses (`AgentEvent::TextDelta`) are appended to `MessageList` but the active streaming entry is skipped during `take_chat_inserts` (chat.rs:38). The entry only renders after `AgentEvent::Finished`. This means the user sees no live text output while the agent is responding.

This is the single largest UX gap. Every competitor (Claude Code, Gemini CLI, opencode, pi-mono, amp) shows streaming text as it arrives. The design doc (`tui-v2.md:196-211`) describes the intended pattern: buffer in managed area, differential render, commit on complete. This was designed but not yet implemented.

**Progress line during streaming** (`direct.rs:244-302`): Shows spinner + tool name + elapsed time. This works but is cold feedback when the agent is generating text for 10-30 seconds with no visible output.

### Tool Output Display

Tool output rendering lives in `message_list.rs:350-465` and `chat_renderer.rs:90-175`.

**Strengths:**

- Smart key-arg extraction per tool type (`extract_key_arg` handles read, write, edit, bash, glob, grep with contextual truncation)
- Collapsed display for context-gathering tools (read, glob, grep show count instead of content)
- Status icons: checkmark for success, X for error
- Truncation: 5 lines shown with overflow indicator
- Syntax highlighting for read/grep results based on file extension
- Diff highlighting for edit/write tools
- ANSI escape parsing for bash output

**Gaps:**

- No expand/collapse toggle (Ctrl+O as recommended by research doc)
- No spinner during tool execution (only progress line says tool name)
- Tool result count of 5 lines is lower than industry standard of 10
- No width-aware diff display (split vs unified based on terminal width)
- Tool errors show inline but are easy to miss in a long session

### Markdown Rendering

`highlight/markdown.rs` (331 lines) uses pulldown-cmark. Handles: bold, italic, code spans, fenced code blocks with syntax highlighting, headers, ordered/unordered lists, blockquotes, horizontal rules, tables.

**Quality:** Good coverage. The table rendering (`table.rs`, 567 lines) handles column width distribution, alignment, and border drawing. It is the heaviest single-feature module.

**Gap:** No link rendering. Markdown links render as plain text (the URL is lost). This matters less for a coding agent but is noticeable.

### Status Bar

`direct.rs:422-485` renders: `[READ/WRITE] . model-name [think:Xk] . used/max (pct%)`

**Good:** Compact, informative, clear mode indicator with color coding. Token usage shown when available.

**Missing:**

- No cost estimate (competitors like opencode show estimated cost)
- No message count or turn count
- Model name truncation could be smarter (shows last path segment, which works for most but not all model IDs)

---

## 4. UX Patterns

### Input Experience

**Multi-line input:** Excellent. Shift+Enter (Kitty protocol) or Alt+Enter (universal fallback) for newlines. The ropey-backed buffer handles large inputs efficiently. Visual line wrapping with correct cursor tracking across wrapped lines. Scroll-within-input for tall content.

**Paste handling:** Excellent. Large pastes (>5 lines or >500 chars) stored as blobs with invisible-delimiter placeholders. This prevents the input buffer from becoming unwieldy while preserving the full content for the agent.

**History:** Good. Persistent history stored in SQLite. Up/Down navigation with draft preservation. Ctrl+R reverse search with fuzzy matching. History deduplicated and normalized.

**Emacs keybindings:** Comprehensive. Ctrl+A/E (line start/end), Ctrl+W (delete word), Ctrl+U (delete to line start), Ctrl+K (delete to end), Alt+B/F (word movement). Cmd+Left/Right for macOS visual line navigation.

**External editor:** Ctrl+G opens VISUAL/EDITOR with tempfile. Properly suspends/resumes TUI raw mode.

**Completers:**

- `/` triggers command completion popup (7 commands, fuzzy matched)
- `@` triggers file path completion (recursive up to depth 2, skips node_modules/target)
- Both render as floating popups above the input with selection highlight

### Navigation

| Key           | Action                                          |
| ------------- | ----------------------------------------------- |
| PageUp/Down   | Scroll chat history                             |
| Up/Down       | History recall or cursor movement               |
| Ctrl+R        | Reverse history search                          |
| Ctrl+M        | Model picker                                    |
| Ctrl+P        | Provider picker                                 |
| Shift+Tab     | Toggle Read/Write mode                          |
| Ctrl+T        | Cycle thinking level                            |
| Ctrl+H or `?` | Help overlay                                    |
| Ctrl+G        | External editor                                 |
| Esc           | Cancel running task / double-tap to clear input |
| Ctrl+C        | Clear input / double-tap to quit                |

This is comprehensive and well-designed. The double-tap safety on Esc/Ctrl+C is good UX.

### Feedback

**Progress:** Spinner animation (10-frame braille pattern) with tool name and elapsed time. Retry status with countdown. Completion summary with success/error/cancel icon, duration, and token counts.

**Errors:** Displayed in status line (last_error) and as system messages in chat. API errors are cleaned up (`format_api_error`, `strip_error_prefixes`). Friendly messages for common errors (e.g., "Network timeout").

**Missing feedback:**

- No "thinking" indicator during model reasoning (thinking_start is tracked but only used for duration display after completion)
- No typing/streaming indicator while agent generates text
- No confirmation after write/edit operations beyond the tool result line

---

## 5. Comparison to Competitors

### What ion has that peers do

| Feature                       | Ion | Claude Code | Gemini CLI | opencode        |
| ----------------------------- | --- | ----------- | ---------- | --------------- |
| Native terminal scrollback    | Yes | Yes         | Yes        | No (fullscreen) |
| Persistent chat after exit    | Yes | Yes         | No         | No              |
| Ropey-backed input            | Yes | No          | No         | No              |
| Blob storage for large pastes | Yes | No          | No         | No              |
| Multi-provider model picker   | Yes | No          | No         | Yes             |
| Session persistence + resume  | Yes | Yes         | No         | Yes             |
| @file autocomplete            | Yes | Yes         | Yes        | Yes             |
| /command autocomplete         | Yes | Yes         | Yes        | Yes             |
| Ctrl+R history search         | Yes | Yes         | No         | Yes             |
| External editor support       | Yes | Yes         | No         | Yes             |

### What competitors have that ion lacks

| Feature                              | Claude Code  | Gemini CLI   | opencode    | Impact   |
| ------------------------------------ | ------------ | ------------ | ----------- | -------- |
| **Live streaming text**              | Yes          | Yes          | Yes         | Critical |
| **Tool output expand/collapse**      | Yes (Ctrl+R) | No           | Yes         | High     |
| **Diff viewer for edits**            | Inline diff  | Side-by-side | Diff view   | High     |
| **Cost tracking**                    | Yes          | No           | Yes         | Medium   |
| **Image display** (iTerm/Kitty)      | Yes          | Yes          | No          | Low      |
| **Multi-tool parallel display**      | Yes          | Yes          | No          | Medium   |
| **File viewer** (syntax highlighted) | Scrollable   | No           | Yes         | Low      |
| **Context usage bar**                | Yes          | No           | Yes         | Low      |
| **Compact status on completion**     | Time + cost  | Time         | Time + cost | Medium   |

---

## 6. Code Quality

### File Size Assessment

| File                | Lines | Status     | Notes                                         |
| ------------------- | ----- | ---------- | --------------------------------------------- |
| `message_list.rs`   | 892   | Large      | 320 lines tests, 572 code -- OK with tests    |
| `events.rs`         | 776   | Large      | Should extract slash command dispatch         |
| `composer/state.rs` | 591   | Acceptable | Dense cursor/navigation logic, well-organized |
| `render/direct.rs`  | 574   | Acceptable | Main render functions, reasonable             |
| `table.rs`          | 567   | Large      | Over-engineered for current use               |
| `chat_renderer.rs`  | 533   | Large      | Core rendering, justified complexity          |
| `run.rs`            | 528   | Acceptable | Main loop, justified                          |
| `composer/tests.rs` | 449   | Tests      | Good coverage                                 |
| `model_picker.rs`   | 404   | Acceptable | Two-stage picker, reasonable                  |

### Long Functions

| Function                     | File:Line                  | Lines | Issue                                          |
| ---------------------------- | -------------------------- | ----- | ---------------------------------------------- |
| `handle_input_mode`          | `events.rs:79`             | ~440  | Too long -- extract completer + slash dispatch |
| `render_markdown_with_width` | `highlight/markdown.rs:17` | ~310  | Could extract per-tag handlers                 |
| `build_lines`                | `chat_renderer.rs:12`      | ~240  | Could extract per-sender rendering             |
| `handle_selector_mode`       | `events.rs:525`            | ~170  | Borderline -- dispatch_picker helps            |
| `run`                        | `run.rs:214`               | ~315  | Justified -- main loop phases are sequential   |
| `with_permissions`           | `session/setup.rs:38`      | ~210  | Initialization -- hard to split meaningfully   |

### Consistency Issues

1. **ModelPicker vs ProviderPicker/SessionPicker**: ModelPicker does NOT use `FilterablePicker<T>`. It has its own `move_up`, `move_down`, `jump_to_top`, `jump_to_bottom`, `apply_filter` implementations that duplicate the trait. The ProviderPicker and SessionPicker correctly use `FilterablePicker<T>`. ModelPicker should be migrated to use it too, but its two-stage design (Provider -> Model) makes this slightly more complex.

2. **Inconsistent render method signatures**: Some render methods take `&self` (`render_progress_running`, `render_status_direct`, `render_history_search`), while `render_input_direct` and `draw_direct` take `&mut self`. The mutation in `render_input_direct` is for `calculate_cursor_pos_with` which updates `cursor_pos` and `last_width`. This is a render-time side effect that could be moved to `update()`.

3. **Error handling in event handlers**: Some operations use `let _ =` to ignore errors (e.g., `let _ = self.store.save(&self.session)` at events.rs:367, `let _ = self.store.add_input_history(...)` at events.rs:472). These should at minimum log the error with `tracing::warn!`.

### Dead or Questionable Code

1. **`message_list.scroll_offset` used but partially broken**: `scroll_up` and `scroll_down` exist and are called from PageUp/PageDown. But the scroll_offset is not used during rendering in the current architecture (chat goes to scrollback, terminal handles scrolling). The `push_entry` method adjusts scroll_offset to maintain position (line 471-476), suggesting this was partially implemented for managed-area scrollback. Currently, this state has no visible effect -- native terminal scroll is what the user actually uses.

2. **`QUEUED_PREVIEW_LINES` and queued message rendering**: The chat_renderer has logic to render queued messages (lines 202-225) but these are only visible if the message_queue contains items. The rendering happens in `build_lines` but the queue parameter is only passed as `None` in `take_chat_inserts` and `build_chat_lines`. The queued preview feature appears to be partially implemented.

---

## 7. Top 5 UX Strengths

1. **Input composer**: The ropey-backed editor with blob storage, visual line wrapping, grapheme-correct cursor movement, and Emacs keybindings is production-quality. It handles edge cases (Unicode, wide chars, paste) that most competitors get wrong.

2. **Chat-to-scrollback architecture**: Printing chat to stdout and letting the terminal manage scrollback gives users native scroll, Cmd+F search, and text persistence after exit. This is the same proven approach as Claude Code.

3. **Tool output formatting**: Context-aware key-arg extraction, collapsed display for read/grep tools, status icons, syntax highlighting, and ANSI escape parsing. The `extract_key_arg` function handles each tool type intelligently (shows path for read, command for bash, pattern for grep).

4. **Session management**: Persistent sessions with SQLite, session picker for resume, session clearing, input history persistence. The `/clear` command properly saves before starting fresh and scrolls old content into terminal scrollback (not lost).

5. **Progressive disclosure of complexity**: The UI starts minimal (input + status). Completers appear only when triggered. Pickers replace the input area temporarily. Help overlay shows on demand. The user is never overwhelmed.

---

## 8. Top 5 UX Gaps vs Competitors

1. **No streaming text display** -- The user sees nothing while the agent generates a response. Only the spinner and tool name appear. Every competitor shows text token-by-token. This makes long responses feel like the tool is frozen. (Designed in tui-v2.md but not implemented.)

2. **No tool output expand/collapse** -- Tool results are permanently displayed at their truncated length (5 lines). Users cannot expand to see full output or collapse to reduce noise. Claude Code (Ctrl+R), Pi (Ctrl+O), and opencode all have this.

3. **No diff preview for edit/write operations** -- When the agent writes or edits a file, only the status line appears. Competitors show inline diffs with added/removed highlighting. Ion has diff highlighting code for code blocks in markdown, and the research doc recommends width-aware diffs, but tool results for edit/write only show "edit(path) / checkmark".

4. **No cost tracking** -- No per-request or cumulative cost display. Token counts are shown but not translated to cost. Both Claude Code and opencode show estimated cost. For an agent that can make many API calls, cost visibility is important.

5. **No thinking/reasoning display** -- When using thinking models (Ctrl+T enables thinking budgets), the thinking content is discarded (`ThinkingDelta` is tracked for timing but not rendered). Claude Code shows a collapsible "Reasoning" section. Ion's `MessagePart::Thinking` exists in the data model but the chat_renderer skips it (chat_renderer.rs:83-86).

---

## 9. Prioritized Improvement List

Ordered by user-visible impact:

### P1 -- Critical (users notice immediately)

1. **Implement streaming text display**: Buffer agent TextDelta in managed area below chat, render incrementally, commit to scrollback on Finished. The design exists in tui-v2.md Q4. This is the single most impactful improvement.

### P2 -- High (users notice within first session)

2. **Add tool output expand/collapse**: Track collapsed/expanded state per tool entry. Default collapsed for context tools (read, glob, grep), expanded for mutation tools (edit, write, bash). Keybinding to toggle (Ctrl+O matches Pi/opencode convention). Store full result in MessageEntry, render truncated or full based on state.

3. **Show diff preview for edit/write results**: The agent already sends the result with enough info to reconstruct the diff. Render added/removed lines with green/red highlighting inline in the tool result. The `highlight_diff_line` function already exists.

4. **Display thinking/reasoning content**: Enable rendering of `MessagePart::Thinking` in chat_renderer.rs. Show as collapsible blockquote. The data model already supports it.

### P3 -- Medium (users notice over multiple sessions)

5. **Add cost tracking**: Track input/output tokens per request, multiply by model pricing (already available in ModelInfo.pricing). Show cumulative cost in status bar or completion summary.

6. **Fix scroll_offset to actually work**: Either implement proper managed-area scrollback with scroll_offset controlling the viewport, or remove the scroll state and let terminal scrollback be the only scroll mechanism. The current half-implementation is confusing.

7. **Extract slash command dispatch**: Move the 111-line inline match in `handle_input_mode` Enter handler into a `dispatch_slash_command(&str) -> bool` method. This is pure maintainability -- no user-visible change.

8. **Show "thinking" indicator in progress bar**: When `task.thinking_start` is set, show "Thinking..." or a distinct spinner instead of "Ionizing..." in the progress line. The timing is already tracked.

### P4 -- Low (polish)

9. **Migrate ModelPicker to FilterablePicker<T>**: Remove duplicated navigation code. The two-stage design needs a wrapper but the per-stage lists can each be a FilterablePicker.

10. **Add context usage visualization**: Show a visual bar or percentage more prominently. The data is already in `token_usage`.

11. **Render markdown links**: Show `[text](url)` as `text` with underline, or at least keep the URL visible.

12. **Increase tool result max lines from 5 to 10**: Industry standard is 10 lines. 5 is too aggressive for bash output.

---

## 10. Architecture Risks

1. **No synchronized output for streaming**: When streaming is implemented, the per-token rendering must use `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` to prevent flicker. The main loop already does this for the bottom UI but the streaming area will need it too.

2. **`compact_messages` called synchronously in event handler**: `/compact` runs `self.agent.compact_messages()` on the event handling thread (events.rs:358). If this involves API calls or heavy computation, it will block the UI. Should be spawned as an async task.

3. **File completer scans filesystem synchronously**: `refresh_candidates` in `file_completer.rs:213` does recursive `read_dir` up to depth 2. In large repos, this could cause a visible pause when `@` is typed. Consider async scanning or lazy loading.

---

## Summary Metrics

| Metric                       | Value                           |
| ---------------------------- | ------------------------------- |
| Total TUI lines (incl tests) | ~10,200                         |
| Test lines                   | ~1,700                          |
| Code lines (excl tests)      | ~8,500                          |
| Modules                      | 28 files                        |
| App struct fields            | 34                              |
| Mode variants                | 4                               |
| Slash commands               | 7                               |
| Keybindings documented       | 15+                             |
| Largest function             | `handle_input_mode` (440 lines) |
| Critical UX gap              | No streaming text display       |

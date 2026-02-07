# Compaction v2: LLM-Based Summarization

**Status**: Design
**Research**: `ai/research/compaction-techniques-2026.md`
**Task**: tk-k28w

## Problem

Current mechanical compaction (Tier 1/2) only prunes tool outputs. When conversations are long with many user/assistant turns, pruning tool outputs alone may not reach the target threshold. The agent loses context about earlier work, decisions, and file changes. There is no way for the agent to proactively compact before hitting limits.

## Goals

1. **Tier 3 summarization**: LLM-based compaction when Tier 1/2 can't reach target
2. **Agent-triggered compaction**: A tool the model can invoke to compact proactively
3. **Structured metadata**: Preserve file lists, tool history, decisions -- not just narrative
4. **Invisible continuation**: Seamless post-compaction experience (no user re-prompting)

## Non-Goals

- Cross-session memory (future, separate system)
- File externalization / Cursor-style context paging (future)
- Neural pruning / fine-tuned skimmers
- Focus chain / periodic re-injection (good idea, separate feature)

## Design

### Overview

```
Tier 1: Truncate large tool outputs (existing)
  |  Still over target?
Tier 2: Remove old tool output content (existing)
  |  Still over target?
Tier 3: LLM summarization of old conversation turns
  |  Replace messages[0..cutoff] with structured summary
```

### Tier 3 Flow

```
1. Calculate cutoff: messages.len() - protected_messages
2. Extract messages[0..cutoff] as "old" conversation
3. Send to summarization model with structured prompt
4. Replace messages[0..cutoff] with single System message containing summary
5. Result: [system_prompt, summary_message, protected_recent_messages]
```

### Summarization Prompt

7-section structured template. Each section enforced to ensure completeness.

```
Summarize the following conversation for seamless continuation.
Be thorough with technical details. Organize into these sections:

1. TASK STATE: Current goal, progress, remaining work items
2. FILES: All file paths read, written, or edited (full paths, list format)
3. TOOL HISTORY: Tools called and key outcomes (tool name + key result, condensed)
4. ERRORS: Problems encountered and resolutions
5. DECISIONS: Architectural/design choices made and rationale
6. USER GUIDANCE: Corrections, preferences, constraints from the user
7. NEXT STEPS: Immediate action to resume work

Preserve exact file paths, error messages, and code patterns.
Focus on information needed to continue without re-asking the user.
```

### Model Selection

Priority order:

1. User-configured `compaction.model` in config
2. Cheapest available model from active provider
3. Active session model (fallback)

Provider defaults:

| Provider   | Compaction Model |
| ---------- | ---------------- |
| Anthropic  | haiku            |
| OpenAI     | gpt-4o-mini      |
| Google     | gemini-2.5-flash |
| Groq       | llama-3.3-70b    |
| OpenRouter | cheapest routing |
| Local      | session model    |

Rationale: Structured prompt compensates for smaller model capacity. JetBrains research shows summarization quality matters less than completeness. Latency matters -- <1s is imperceptible, 5-8s is noticeable.

### Agent-Triggered Compaction (Compact Tool)

The agent gets a built-in `compact` tool it can invoke proactively.

**When the agent would use it:**

- Before starting a large new task (clear old context)
- When sensing context degradation (repeating questions, forgetting earlier work)
- After completing a major milestone (clean break)

**Tool definition:**

```rust
Tool {
    name: "compact",
    description: "Compact the conversation context to free up space. Use when:
      (1) switching to a new task area, (2) after completing major work,
      or (3) if you notice context degradation. Returns summary of what was preserved.",
    parameters: {
        "focus": {
            "type": "string",
            "description": "Optional focus area to emphasize in the summary",
        }
    },
}
```

**Implementation:**

- Runs Tier 1 → 2 → 3 pipeline (same as auto-compaction)
- Returns the summary text as tool result so the model knows what was preserved
- Emits `AgentEvent::CompactionStatus` for TUI display

### Message Structure After Compaction

Before:

```
[System prompt]
[User: "read main.rs"]
[Assistant: "I'll read it"]
[ToolResult: <1000 lines of main.rs>]
[Assistant: "Here's what I found..."]
... 40 more turns ...
[User: "now fix the bug"]          ← protected
[Assistant: "I'll fix it"]         ← protected
[ToolResult: <edit result>]        ← protected
```

After Tier 3:

```
[System prompt]
[System: "<structured summary of first 40 turns>"]
[User: "now fix the bug"]          ← protected
[Assistant: "I'll fix it"]         ← protected
[ToolResult: <edit result>]        ← protected
```

### Auto-Compaction Enhancement

Current: triggers after tool execution when over threshold.
New: also emit a hint in the status bar when approaching threshold (e.g., 70%).

```
Agent loop:
  1. After tool execution, count tokens
  2. If >= trigger_threshold (80%): run Tier 1 → 2 → 3 pipeline
  3. If >= warning_threshold (70%): emit AgentEvent::ContextWarning
```

The TUI can show a subtle indicator: `[ctx: 72%]` in the status bar.

### Integration Points

**`src/compaction/mod.rs`:**

- Add `SummarizationResult` struct
- Add `summarize_messages()` async fn (calls LLM)
- Add `CompactionConfig.summarization_model: Option<String>`

**`src/compaction/pruning.rs`:**

- Current `prune_messages()` stays synchronous (Tier 1/2 only)
- New `compact_with_summarization()` async fn wraps Tier 1/2 + Tier 3
- Returns `CompactionResult` with `summary: Option<String>`

**`src/agent/mod.rs`:**

- Change compaction block to call async `compact_with_summarization()`
- Pass provider reference for Tier 3 LLM calls

**`src/tool/builtin/compact.rs`** (new):

- Built-in `compact` tool
- Calls `Agent::compact_messages_with_summary()`
- Returns summary as tool result

**`src/config/mod.rs`:**

- Add `CompactionSettings` to config:
  ```toml
  [compaction]
  model = ""           # empty = auto-select
  auto = true          # auto-compact on threshold
  protected_messages = 12
  ```

### Error Handling

- If summarization LLM call fails: fall back to Tier 2 result (mechanical only)
- If summarization returns empty/invalid: keep Tier 2 result, log warning
- If compact tool called but already under target: return "No compaction needed"
- Network errors during summarization: non-fatal, agent continues with Tier 2

### TUI Changes

- Show context usage in status bar: `[ctx: 45%]` (always visible)
- On compaction: brief inline message like `— context compacted (150k → 105k tokens) —`
- No approval prompt -- compaction is invisible by design

## Implementation Plan

### Phase 1: Infrastructure

| #   | Task                                  | Files                               |
| --- | ------------------------------------- | ----------------------------------- |
| 1   | Add `CompactionSettings` to config    | `config/mod.rs`                     |
| 2   | Add `summarize_messages()` async fn   | `compaction/summarization.rs` (new) |
| 3   | Wire summarization model selection    | `compaction/summarization.rs`       |
| 4   | Create `compact_with_summarization()` | `compaction/mod.rs`                 |

### Phase 2: Agent Integration

| #   | Task                                   | Files                           |
| --- | -------------------------------------- | ------------------------------- |
| 5   | Update agent loop for async compaction | `agent/mod.rs`                  |
| 6   | Add `compact` built-in tool            | `tool/builtin/compact.rs` (new) |
| 7   | Register compact tool                  | `tool/builtin/mod.rs`           |
| 8   | Add context % to status bar            | `tui/render/direct.rs`          |

### Phase 3: Polish

| #   | Task                                 | Files                         |
| --- | ------------------------------------ | ----------------------------- |
| 9   | Show compaction message in chat      | `tui/message_list.rs`         |
| 10  | Add `/compact` slash command (alias) | `tui/events.rs`               |
| 11  | Tests for Tier 3 summarization       | `compaction/summarization.rs` |
| 12  | Integration test: full pipeline      | `compaction/mod.rs`           |

## Risks

| Risk                              | Mitigation                                        |
| --------------------------------- | ------------------------------------------------- |
| Summarization loses critical info | Structured 7-section prompt enforces completeness |
| Cost of summarization calls       | Default to cheapest model, prompt caching helps   |
| Agent over-compacts via tool      | Rate limit: minimum 10K tokens before eligible    |
| Latency of summarization          | Small model (<1s), non-blocking TUI               |
| Summary role confusion            | Use System role for summary, clear delimiter      |

## References

- Research: `ai/research/compaction-techniques-2026.md`
- JetBrains "Complexity Trap": observation masking matches LLM summarization in 4/5 cases
- Claude Code: 9-section structured summary, same model, file re-reading post-compact
- MemGPT: hierarchical memory model maps to our tiered approach

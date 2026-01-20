# Context Compaction Design

**Date:** 2026-01-16
**Status:** Design
**Purpose:** Cache-aware context compaction for long sessions

## Core Principles

1. **Structure forces preservation** - Dedicated sections prevent silent info loss
2. **Scope by default** - Agent reaches for context via tools, not pre-loaded
3. **Proactive extraction** - Background processing at task boundaries, not reactive overflow
4. **Learning trajectories** - Preserve "tried X, failed, Y worked because Z"

## Architecture

```
┌─────────────────────────────────────────────────────┐
│ Stable Prefix (cached, never modified)              │
│ ├── System prompt                                   │
│ ├── Tool definitions                                │
│ └── .ion/ context (CLAUDE.md, skills)              │
├─────────────────────────────────────────────────────┤
│ Compacted Summary (structured, updated at tasks)    │
│ └── See "Structured Summary Format" below           │
├─────────────────────────────────────────────────────┤
│ Recent Window (~5-8K, message-aware)                │
│ ├── Last 3-5 user/assistant exchanges (verbatim)   │
│ └── Tool outputs truncated to ~500 tokens each     │
├─────────────────────────────────────────────────────┤
│ Current Turn                                        │
└─────────────────────────────────────────────────────┘
```

## Token Budget (200K context)

| Region         | Tokens | %    | Notes                        |
| -------------- | ------ | ---- | ---------------------------- |
| System prefix  | ~10K   | 5%   | Cached, stable               |
| Output reserve | ~16K   | 8%   | max_tokens headroom          |
| Recent window  | ~5-8K  | 3-4% | Message-aware, not raw count |
| Usable         | ~174K  | 87%  | History + summaries          |

### Empirically-Backed Thresholds (2026 Research)

| Model           | Context | Degradation Onset | Recommended Max |
| --------------- | ------- | ----------------- | --------------- |
| Claude Opus 4.5 | 200K    | 65-75%            | 60-70%          |
| GPT-5.2         | 400K    | 70-80%            | 65-75%          |
| Gemini 3 Pro    | 2M      | 60-70%            | 55-65%          |
| DeepSeek v3.2+  | 256K+   | 80-90% (MLA)      | 75-85%          |
| Grok 4.20       | 512K    | 75-85%            | 70-80%          |

**Key findings** (RULER, LongBench, InfiniteBench 2026):

- "Lost in the middle" causes 20-50% recall drop in central context regions
- Multi-hop QA degrades 15-20% earlier than single retrieval
- Latency increases 150% at threshold; 2-5x spikes at 80%+
- DeepSeek's MLA attention resists LIM significantly better (+15-20% usable)

**Default thresholds** (conservative, for standard transformers):

- **Trigger**: 55% (~101K) - well before degradation onset
- **Target**: 45% (~83K) - small gap for frequent, invisible compactions

**Model-specific overrides** (future):

- DeepSeek/Grok: Can trigger at 70%, target 60%
- Gemini: Conservative 50% trigger (LIM worse on 2M)

Compaction runs during idle time (after turns complete), invisible to user.

**Practitioner quote**: "Never exceed 70% in prod—it's the empirical cliff." — Karpathy, 2026

## Structured Summary Format

Based on Factory.ai research: "Structure forces preservation—each section acts as a checklist."

```markdown
## Session Intent

[What the user wants to accomplish overall]

## Current Task

[What's actively being worked on, current state/progress]

## Files Modified

- src/auth.rs: Added JWT middleware (lines 50-80)
- src/config.rs: Fixed race condition in session loading

## Files Read (reference only)

- src/agent/mod.rs
- src/provider/anthropic.rs

## Key Decisions

- Using SQLite over DuckDB for embedding compatibility
- Tiered pruning before summarization (OpenCode pattern)
- Fast model for incremental compaction

## Failed Approaches (CRITICAL - prevents retry loops)

- Attempted direct OmenDB queries, hit async lifetime issues
- Tried full summarization first, lost too much context

## Errors Encountered

- "cannot borrow as mutable" in tool executor - fixed with Arc
- Provider timeout at 30s - increased to 120s

## Next Steps

- Implement token counting
- Add background compaction worker
```

**Not included in summary:**

- Full file contents (agent can re-read)
- Raw tool output (just reference path/command)
- Exploratory discussion (captured in decisions)
- Verbose explanations

## Triggers

| Trigger             | When                        | Action           |
| ------------------- | --------------------------- | ---------------- |
| **Agent-initiated** | Task complete, starting new | Proactive, ideal |
| **Token threshold** | 85% capacity                | Safety net, auto |
| **User command**    | `/compact`                  | Manual override  |

**Proactive > Reactive**: Agent triggers compaction at natural task boundaries. Threshold is a safety net, not the primary mechanism.

## Background Compaction

Compaction runs async, doesn't block agent:

```rust
pub struct BackgroundCompactor {
    request_tx: mpsc::Sender<CompactionRequest>,
    result_rx: mpsc::Receiver<CompactionResult>,
}

impl BackgroundCompactor {
    /// Non-blocking: queue compaction
    pub fn request(&self, messages: Vec<Message>, focus: Option<String>) {
        let _ = self.request_tx.try_send(CompactionRequest { messages, focus });
    }

    /// Check if ready (non-blocking)
    pub fn poll(&mut self) -> Option<StructuredSummary> {
        self.result_rx.try_recv().ok()
    }
}
```

When result ready, swap compacted summary into context. From user perspective: instant.

## Tiered Pruning

Execute before summarization:

| Tier | Action                                                       | Savings |
| ---- | ------------------------------------------------------------ | ------- |
| 1    | Truncate tool outputs >2K to head+tail (~500 each)           | 2-10x   |
| 2    | Remove old tool output content (keep path/command reference) | 5-20x   |
| 3    | Fast-model structured summary of older turns                 | 3-5x    |

**Never modify**: System prefix (cache boundary)

## What to Preserve

**Always in summary:**

- Session intent
- Current task state
- Files modified (with edit summary, not content)
- Key decisions with rationale
- Failed approaches (CRITICAL)
- Error messages encountered

**Reference only (agent re-fetches if needed):**

- Files read
- Command outputs
- Search results

**Drop entirely:**

- Greetings/pleasantries
- Verbose explanations (captured in decisions)
- Superseded code versions
- Raw diffs (keep "edited lines X-Y" reference)

## Scope by Default

Agent doesn't get everything pre-loaded. Instead:

```
Before: Load last 5 files into context (wasteful)
After:  Summary says "Modified src/auth.rs lines 50-80"
        Agent re-reads file IF needed for current task
```

This aligns with research: "Agents must reach for more information explicitly via tools rather than being flooded by default."

## ai/ Directory as Memory

Survives compaction naturally:

- `ai/STATUS.md` - Current state
- `ai/DECISIONS.md` - Architecture decisions
- `tk` tasks - Persisted tracking

This is long-term memory that doesn't consume context.

## Summarization

**Fast model** (Haiku-class):

- Background compaction
- Incremental pruning
- Most compaction operations

**Full model**:

- User-triggered `/compact` with focus
- Handoff to new thread

**Prompt**:

```
Generate a structured summary for conversation continuation.

Use this exact format:
## Session Intent
## Current Task
## Files Modified
## Files Read (reference only)
## Key Decisions
## Failed Approaches
## Errors Encountered
## Next Steps

Rules:
- Each section is a checklist - populate or explicitly mark empty
- For files: path + brief edit description, NOT full content
- ALWAYS preserve failed approaches (prevents retry loops)
- ALWAYS preserve error messages (exact wording matters)
- Decisions need rationale ("chose X because Y")
- Be concise but complete
```

## Implementation

```
src/compaction/
├── mod.rs           # CompactionConfig, public API
├── counter.rs       # Token counting (tiktoken-rs)
├── pruning.rs       # Tiered output pruning
├── summary.rs       # Structured summary generation
├── background.rs    # Async compaction worker
└── tool.rs          # Compact tool for agent
```

## Quality Metrics

| Metric                    | Target       | How                               |
| ------------------------- | ------------ | --------------------------------- |
| Compression ratio         | 3-5x         | tokens_before / tokens_after      |
| Cache hit rate            | >80%         | Monitor provider metrics          |
| Task continuation         | >99%         | Track post-compact success        |
| Failed approach retention | 100%         | Verify in summary                 |
| Summary completeness      | All sections | Check for empty required sections |

## References

- [Evaluating Context Compression | Factory.ai](https://factory.ai/news/evaluating-compression)
- [Compressing Context | Factory.ai](https://factory.ai/news/compressing-context)
- [Effective context engineering | Anthropic](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- `ai/research/context-compaction-2026.md` - Full research

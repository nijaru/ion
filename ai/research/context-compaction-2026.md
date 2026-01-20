# Context Compaction in AI Coding Agents - 2026 State of the Art

**Research Date:** 2026-01-16
**Focus:** Cache-aware compaction, summarization patterns, production implementations, Rust token counting

## Executive Summary

| Strategy                          | Best For                           | Cache Impact            | Complexity |
| --------------------------------- | ---------------------------------- | ----------------------- | ---------- |
| **Tiered Pruning**                | Long sessions, tool-heavy work     | High (preserves prefix) | Medium     |
| **Handoff** (Amp)                 | Task transitions, focused threads  | N/A (fresh context)     | Low        |
| **Incremental Summary** (Factory) | Continuous work, cost optimization | Medium                  | High       |
| **Agent-Driven Compression**      | Relevance-aware, quality-focused   | Varies                  | High       |

**Key Insight (2026):** Production agents are moving away from full-conversation summarization toward:

1. **Filesystem offloading** - Write tool outputs to files, read back on demand
2. **Focused threads** - Encourage task boundaries over long conversations
3. **Cache-aware architecture** - Structure prompts for maximum cache hits

---

## 1. Cache-Aware Compaction Strategies

### The Cache Hit Imperative

Manus (acquired by Meta for $2B) identified cache hit rate as their **most important production metric**:

> "A higher capacity ($/token) model with caching can actually be cheaper than a lower cost model without it."

Claude Code would be cost-prohibitive without caching. Agents must design compaction to preserve cache prefixes.

### Provider-Specific Cache Behavior

| Provider   | Cache Type           | Min Tokens | TTL                | Compaction Impact                  |
| ---------- | -------------------- | ---------- | ------------------ | ---------------------------------- |
| Anthropic  | Explicit breakpoints | 1024-4096  | 5min (1hr at 2x)   | Preserve system + CLAUDE.md prefix |
| DeepSeek   | Automatic prefix     | 64 blocks  | Hours-days         | Any prefix change invalidates      |
| OpenAI     | Automatic prefix     | 1024       | 5-10min            | Hash-based routing                 |
| OpenRouter | Pass-through         | Varies     | Provider-dependent | Follow underlying provider         |

### Cache-Friendly Compaction Architecture

```
Context Window Structure (Cache-Optimized)
==========================================
[CACHED - Rarely Changes]
├── System prompt
├── Tool definitions
├── CLAUDE.md / AGENTS.md
├── MCP server configs
├── Skills/instructions
└── cache_control: ephemeral  <-- Anthropic breakpoint
--------------------------------------------
[SEMI-STABLE - Compacted History]
├── Previous compaction summaries
├── Extracted facts/decisions
└── Task state checkpoints
--------------------------------------------
[VARIABLE - Current Turn]
├── Recent tool outputs (truncatable)
├── Current user message
└── Active file contents
```

**Critical Rule:** Never compact into the cached prefix. Compaction should only affect the variable suffix.

### Preserving Cache During Compaction

```rust
struct CompactionBoundaries {
    /// Offset where cached content ends (never modify before this)
    cache_boundary: usize,
    /// Offset where compactable history begins
    history_start: usize,
    /// Tokens to protect from compaction (recent turns)
    protected_suffix: usize,  // OpenCode: 40K tokens
}

fn compact_preserving_cache(
    context: &mut Context,
    boundaries: &CompactionBoundaries,
) -> Result<()> {
    // Only compact between history_start and (end - protected_suffix)
    let compactable_range = boundaries.history_start..
        (context.len() - boundaries.protected_suffix);

    // Apply tiered pruning only to compactable range
    prune_tool_outputs(&mut context[compactable_range.clone()])?;
    summarize_if_needed(&mut context[compactable_range])?;

    Ok(())
}
```

---

## 2. Summarization Best Practices

### Fast Model vs Full Model

| Approach                      | Latency | Quality | Cost   | Use Case                           |
| ----------------------------- | ------- | ------- | ------ | ---------------------------------- |
| **Fast model** (Haiku, Flash) | ~200ms  | 85%     | Low    | Tool output pruning, incremental   |
| **Full model** (Sonnet, Pro)  | ~1-2s   | 95%     | Medium | Final compaction, critical context |
| **Same model**                | Varies  | 100%    | High   | Consistency-critical workflows     |

**Recommendation:** Use fast models for incremental/background summarization, full model for user-triggered compaction.

### Structured vs Free-Form Summaries

**Structured (Recommended for Tool Outputs):**

```json
{
  "type": "file_read",
  "path": "src/auth.ts",
  "summary": "JWT auth with refresh tokens, 150 lines",
  "key_symbols": ["validateToken", "refreshAuth"],
  "can_restore": true
}
```

**Free-Form (Recommended for Decisions/Discussions):**

```
Session Summary:
- Decided to use SQLite over DuckDB for Bun compatibility
- Implemented JWT auth with refresh token rotation
- Fixed race condition in session persistence
- Next: Add rate limiting to auth endpoints
```

### OpenCode's Compaction Prompt (Production-Tested)

```
Provide a detailed prompt for continuing our conversation above.
Focus on information that would be helpful for continuing the
conversation, including:
- What we did
- What we're doing
- Which files we're working on
- What we're going to do next

Note: The new session will not have access to our conversation.
```

**Key Insight:** Focus on continuation context, not historical record.

### Factory.ai's Incremental Approach

Factory maintains **anchored summaries** updated incrementally:

```
Turn 1-10: Summary S1 (anchored at turn 10)
Turn 11-20: Summary S2 = merge(S1, summarize(turns 11-20))
Turn 21-30: Summary S3 = merge(S2, summarize(turns 21-30))
```

**Benefits:**

- Never re-summarize already-summarized content
- O(1) per-turn cost after threshold
- Predictable token usage

**Thresholds (Factory):**

- T_max: Compression trigger threshold
- T_retained: Post-compression target
- Narrow gap (T_retained ~ T_max): Frequent compression, better recent context
- Wide gap (T_retained << T_max): Less frequent, risk aggressive truncation

---

## 3. Production Agent Implementations

### Claude Code

**Architecture:**

- Trigger: ~70-85% of 200K tokens (configurable, trending lower)
- Method: Full conversation summarization by the same model
- User control: `/compact` command, optional focus hints

**What's Preserved:**

- Key decisions and action items
- Validated code snippets
- Error messages and solutions
- Architecture diagrams
- Recent messages (10-15 exchanges)
- CLAUDE.md contents

**What's Discarded:**

- Detailed back-and-forth discussions
- Failed intermediate attempts
- Full file contents (replaced with references)
- Exploratory dead-ends
- Verbose explanations

**Known Issues (Historical):**

- Infinite retry loops (issue #6004)
- Context corruption after failure (issue #3274)
- Repeating failed approaches post-compaction

**Recent Improvements (Dec 2025):**

- `/stats` command for token visibility
- Instant compact option
- More aggressive protected context (reserving more free space)
- Trend toward earlier compaction thresholds

### Cursor

**Philosophy:** Let agents find context via search rather than preserving everything.

**Key Patterns:**

- Agentic search over files (grep + semantic)
- `@Branch` for git context
- `@Past Chats` for selective history retrieval
- Plans saved to `.cursor/plans/` for cross-session continuity

**Recommendation from Cursor:**

> "Long conversations cause the agent to lose focus. After many turns and summarizations, context accumulates noise."

**When to Start Fresh:**

- Moving to different task/feature
- Agent seems confused or repeats mistakes
- Finished logical unit of work

### Amp (Sourcegraph)

**Major Shift (Oct 2025):** Removed compaction entirely, replaced with **Handoff**.

**Why Amp Abandoned Compaction:**

> "Compaction encourages long, meandering threads, stacking summary on top of summary."

**Handoff Model:**

```
/handoff now implement this for teams as well
/handoff execute phase one of the created plan
/handoff check the rest of the codebase for similar issues
```

1. Specify goal for new thread
2. Agent analyzes current thread for relevant info
3. Generates draft prompt + relevant files for new thread
4. User reviews/edits before submitting

**Key Features:**

- Fork: Duplicate context to branch point
- Restore: Reset to previous message
- Thread References: `@thread-id` to pull selective context
- Edit: Modify previous messages, rerun from that point

**Philosophy:** "200K tokens is plenty" if you keep threads focused.

### OpenCode (sst)

**Tiered Approach:**

1. **Pruning first** - Remove old tool outputs before summarization
2. **Protected window** - 40K tokens recent context always preserved
3. **Compaction last resort** - Full summarization only if pruning insufficient

**Thresholds:**

- PRUNE_PROTECT: 40,000 tokens (retain recent)
- PRUNE_MINIMUM: 20,000 tokens (minimum savings to trigger)
- Protected tools: `["skill"]` - never pruned

**Pruning Algorithm:**

```typescript
// Walk backward through messages
// Skip first 2 turns
// Stop at any message with summary: true
// For each tool output older than protected window:
//   - Mark for pruning if not in protected tools
//   - Set time.compacted timestamp
```

### Manus (Meta)

**Context Offloading:**

- Write old tool results to filesystem
- Only summarize when offloading has diminishing returns
- Agent can read back files if needed

**Proactive Memory:**

- Write plans to files, read back periodically
- Reinforce objectives during long sessions
- Verify work against plan file

### Google ADK

**Tiered Architecture:**

```
Working Context (per-call) <- curated from:
├── Session (event log, permanent)
├── Memory (cross-session, queryable)
└── Artifacts (large blobs, on-demand)
```

**Key Insight:** Separate storage from presentation. Session is permanent record; context is curated view.

---

## 4. What to Preserve vs Summarize

### Always Preserve Verbatim

| Content Type          | Reason                             | Implementation             |
| --------------------- | ---------------------------------- | -------------------------- |
| **Failed approaches** | Prevents retry loops               | Tag with `attempt: failed` |
| **Error messages**    | Exact wording needed for debugging | Store in structured format |
| **File paths/URLs**   | Restorable references              | Keep as breadcrumbs        |
| **Key decisions**     | Architectural continuity           | Extract to facts store     |
| **Recent turns**      | Active working context             | Protected window (40K)     |
| **User constraints**  | Must not be paraphrased            | Explicit preservation      |

### Safe to Summarize

| Content Type            | Compression Ratio | Summary Format                        |
| ----------------------- | ----------------- | ------------------------------------- |
| Exploratory discussions | 10:1              | "Explored X, concluded Y"             |
| Successful tool outputs | 50:1              | Path + summary + can_restore flag     |
| Verbose explanations    | 5:1               | Key points only                       |
| Alternative approaches  | 20:1              | "Considered A, B, chose C because..." |
| Intermediate iterations | 100:1             | Final state only                      |

### Safe to Drop

| Content Type              | Condition                 | Recovery              |
| ------------------------- | ------------------------- | --------------------- |
| Greetings/pleasantries    | Always                    | N/A                   |
| Redundant re-explanations | After first instance      | Reference original    |
| Superseded code versions  | When final version exists | Git history           |
| Verbose formatting        | Always                    | Semantic content only |

### Task Boundaries

**Best Practice:** Compact at natural task boundaries, not mid-task.

```
[Task 1: Auth Implementation] <- Good compaction point
  - Designed JWT flow
  - Implemented refresh tokens
  - Fixed race condition

[Compaction Summary]

[Task 2: Rate Limiting] <- Current work
  - ...
```

---

## 5. Token Counting in Rust

### tiktoken-rs (Already in Cargo.toml)

ion already includes `tiktoken-rs = "0.9.1"`. Usage:

```rust
use tiktoken_rs::{cl100k_base, o200k_base};

// For Claude (uses cl100k_base-like tokenizer)
let bpe = cl100k_base().unwrap();
let tokens = bpe.encode_with_special_tokens("Your text here");
println!("Token count: {}", tokens.len());

// For GPT-4o / newer OpenAI models
let bpe = o200k_base().unwrap();
let tokens = bpe.encode_with_special_tokens("Your text here");
```

### Chat Completion Token Estimation

```rust
use tiktoken_rs::async_openai::get_chat_completion_max_tokens;
use async_openai::types::{ChatCompletionRequestMessage, Role};

let messages = vec![
    ChatCompletionRequestMessage {
        content: Some("System prompt".to_string()),
        role: Role::System,
        ..Default::default()
    },
    ChatCompletionRequestMessage {
        content: Some("User message".to_string()),
        role: Role::User,
        ..Default::default()
    },
];

let max_tokens = get_chat_completion_max_tokens("gpt-4", &messages).unwrap();
```

### Fast Estimation (OpenCode Approach)

For pruning decisions where precision isn't critical:

```rust
/// Fast token estimation (~4 chars per token)
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// More accurate but still fast (word-based)
fn estimate_tokens_words(text: &str) -> usize {
    // Average ~1.3 tokens per word for English
    let word_count = text.split_whitespace().count();
    (word_count as f64 * 1.3) as usize
}
```

### Alternative Crates

| Crate                      | Speed     | Accuracy         | Use Case             |
| -------------------------- | --------- | ---------------- | -------------------- |
| `tiktoken-rs`              | Fast      | Exact for OpenAI | Production counting  |
| `tokenizers` (HuggingFace) | Fast      | Model-specific   | Custom models        |
| `kitoken`                  | Very fast | Exact            | Multi-format support |
| Manual estimation          | Instant   | ~80%             | Pruning decisions    |

---

## 6. Implementation Recommendations for ion

### Phase 1: Basic Infrastructure

```rust
// src/compaction/mod.rs

pub struct CompactionConfig {
    /// Trigger compaction at this % of context limit
    pub trigger_threshold: f32,  // Default: 0.85
    /// Target % after compaction
    pub target_threshold: f32,   // Default: 0.60
    /// Tokens to always protect (recent turns)
    pub protected_suffix: usize, // Default: 40_000
    /// Enable automatic compaction
    pub auto_compact: bool,      // Default: true
}

pub struct TokenBudget {
    pub system_tokens: usize,
    pub memory_tokens: usize,
    pub history_tokens: usize,
    pub output_reserve: usize,
    pub total_available: usize,
}
```

### Phase 2: Tiered Pruning

```rust
pub enum PruningTier {
    /// Tier 1: Truncate large tool outputs (keep head + tail)
    TruncateOutputs { max_per_output: usize, keep_lines: usize },
    /// Tier 2: Remove old tool outputs entirely
    RemoveOldOutputs { protect_turns: usize },
    /// Tier 3: Agent-driven summarization
    Summarize { focus_hint: Option<String> },
    /// Tier 4: Aggressive - drop references, summarize everything
    Aggressive,
}

async fn prune_context(
    context: &mut Context,
    target_tokens: usize,
) -> Result<PruningResult> {
    for tier in [
        PruningTier::TruncateOutputs { max_per_output: 2000, keep_lines: 50 },
        PruningTier::RemoveOldOutputs { protect_turns: 5 },
        PruningTier::Summarize { focus_hint: None },
        PruningTier::Aggressive,
    ] {
        apply_tier(&mut context, tier)?;
        if context.tokens() < target_tokens {
            return Ok(PruningResult::Success { tier_reached: tier });
        }
    }
    Ok(PruningResult::InsufficientSpace)
}
```

### Phase 3: Structured Fact Extraction

```rust
pub struct ExtractedFact {
    pub fact_type: FactType,
    pub content: String,
    pub source_turn: usize,
    pub timestamp: DateTime<Utc>,
}

pub enum FactType {
    Decision { rationale: String },
    Error { resolution: Option<String> },
    FileState { path: PathBuf, summary: String },
    Constraint { must_preserve: bool },
    Milestone { completed: bool },
}

// Extract during compaction, persist to memory
async fn extract_and_persist_facts(
    turns: &[Turn],
    memory: &mut MemorySystem,
) -> Result<Vec<ExtractedFact>> {
    let facts = extract_facts_via_llm(turns).await?;

    for fact in &facts {
        if fact.should_persist() {
            memory.store_fact(fact).await?;
        }
    }

    Ok(facts)
}
```

### Phase 4: Cache-Aware Compaction

```rust
pub struct CacheAwareCompactor {
    /// Token offset where cache ends
    cache_boundary: usize,
    /// Provider-specific cache config
    cache_config: ProviderCacheConfig,
}

impl CacheAwareCompactor {
    pub async fn compact(&self, context: &mut Context) -> Result<()> {
        // Never modify content before cache_boundary
        let compactable = &mut context[self.cache_boundary..];

        // Apply tiered pruning to compactable region only
        self.prune_tiered(compactable).await?;

        // Verify cache prefix unchanged
        debug_assert!(context.prefix_hash() == self.original_prefix_hash);

        Ok(())
    }
}
```

### Phase 5: User Controls

```rust
pub enum CompactionCommand {
    /// Manual compact with optional focus
    Compact { focus: Option<String> },
    /// Start fresh thread (like Amp handoff)
    Handoff { goal: String },
    /// Fork at specific turn
    Fork { turn_id: usize },
    /// View token usage
    Stats,
}

// In TUI command handling
match command {
    CompactionCommand::Compact { focus } => {
        let result = agent.compact_with_focus(focus).await?;
        ui.show_compaction_result(result);
    }
    CompactionCommand::Handoff { goal } => {
        let new_thread = agent.create_handoff(&goal).await?;
        ui.switch_to_thread(new_thread);
    }
    // ...
}
```

---

## 7. Quality Metrics

### Measure These

| Metric                    | Target | How to Measure               |
| ------------------------- | ------ | ---------------------------- |
| Compression ratio         | 3-5x   | tokens_before / tokens_after |
| Information retention     | >90%   | Post-compact Q&A test        |
| Failed approach retention | 100%   | Check for repeated failures  |
| Cache hit rate            | >80%   | Monitor provider metrics     |
| Task continuation rate    | >99%   | Track post-compact success   |

### Testing Compaction Quality

```rust
#[test]
async fn test_compaction_preserves_critical_info() {
    let mut context = create_test_context_with_decision();
    let decision = "Use SQLite over DuckDB for Bun compatibility";

    compact(&mut context).await.unwrap();

    // Verify decision can be recalled
    let response = agent.query("What database decision did we make?").await;
    assert!(response.contains("SQLite") || response.contains("DuckDB"));
}

#[test]
async fn test_compaction_preserves_failed_attempts() {
    let mut context = create_test_context_with_failure();

    compact(&mut context).await.unwrap();

    // Verify failure is recorded
    let response = agent.query("What approaches have we tried?").await;
    assert!(response.contains("failed") || response.contains("didn't work"));
}
```

---

## Sources

### Primary Research

- [Factory.ai: Compressing Context](https://factory.ai/news/compressing-context) - July 2025
- [Amp: Handoff (No More Compaction)](https://ampcode.com/news/handoff) - October 2025
- [OpenCode Context Management](https://deepwiki.com/sst/opencode/2.4-context-management-and-compaction) - January 2026
- [Cursor: Agent Best Practices](https://cursor.com/blog/agent-best-practices) - January 2026

### Claude Code Analysis

- [Claude Code Ultimate Guide: /compact Command](https://deepwiki.com/FlorianBruniaux/claude-code-ultimate-guide/3.2-the-compact-command)
- [Understanding Claude's Conversation Compacting](https://www.ajeetraina.com/understanding-claudes-conversation-compacting-a-deep-dive-into-context-management/)
- [How Claude Code Got Better by Protecting More Context](https://hyperdev.matsuoka.com/p/how-claude-code-got-better-by-protecting)

### Context Engineering

- [Agent Design Patterns](https://rlancemartin.github.io/2026/01/09/agent_design/) - January 2026
- [Mem0: LLM Chat History Summarization Guide](https://mem0.ai/blog/llm-chat-history-summarization-guide-2025) - October 2025
- [Anthropic: Effective Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [Manus: Context Engineering for AI Agents](https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus)

### Token Counting

- [tiktoken-rs GitHub](https://github.com/zurawiki/tiktoken-rs)
- [anysphere/tiktoken-rs](https://github.com/anysphere/tiktoken-rs) - Pure Rust port

### Previous ion Research

- `/Users/nick/github/nijaru/aircher/ai/research/_archive/2025/context-compaction.md`
- `/Users/nick/github/nijaru/aircher/ai/research/prompt-caching-providers-2026.md`

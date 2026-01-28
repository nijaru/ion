# Context Management for AI Agents

Comprehensive research on context compaction, session management, and token budget strategies.

**Updated:** 2026-01-27 (consolidated from context-compaction-2026.md, context-engineering-concepts.md, context-thresholds-2026.md)

---

## Core Concepts

| Term        | Definition                                      |
| ----------- | ----------------------------------------------- |
| **Session** | Persistent log of all interactions              |
| **Context** | Curated payload sent to LLM (subset of session) |
| **Memory**  | Long-term knowledge extracted across sessions   |

**Critical distinction**: Session history ≠ Context. Session is permanent transcript; context is carefully selected.

---

## 1. Compaction Strategies

| Strategy                     | Best For                       | Cache Impact            | Complexity |
| ---------------------------- | ------------------------------ | ----------------------- | ---------- |
| **Tiered Pruning**           | Long sessions, tool-heavy work | High (preserves prefix) | Medium     |
| **Handoff** (Amp)            | Task transitions               | N/A (fresh context)     | Low        |
| **Incremental Summary**      | Continuous work                | Medium                  | High       |
| **Agent-Driven Compression** | Relevance-aware                | Varies                  | High       |

**Key insight (2026)**: Production agents moving toward:

1. Filesystem offloading - Write tool outputs to files
2. Focused threads - Encourage task boundaries
3. Cache-aware architecture - Structure for cache hits

### Cache-Aware Architecture

```
[CACHED - Rarely Changes]
├── System prompt
├── Tool definitions
├── CLAUDE.md / AGENTS.md
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

**Critical Rule:** Never compact into the cached prefix.

### Provider Cache Behavior

| Provider  | Cache Type           | Min Tokens | TTL        |
| --------- | -------------------- | ---------- | ---------- |
| Anthropic | Explicit breakpoints | 1024-4096  | 5min (1hr) |
| DeepSeek  | Automatic prefix     | 64 blocks  | Hours-days |
| OpenAI    | Automatic prefix     | 1024       | 5-10min    |

---

## 2. Production Implementations

### Claude Code

- Trigger: ~70-85% of 200K tokens
- Method: Full conversation summarization by same model
- User control: `/compact` command
- Preserves: Decisions, validated code, error messages, recent 10-15 exchanges

### Amp (Sourcegraph)

**Major shift (Oct 2025)**: Removed compaction, replaced with **Handoff**.

> "Compaction encourages long, meandering threads, stacking summary on summary."

Handoff model:

1. Specify goal for new thread
2. Agent analyzes current thread
3. Generates draft prompt + relevant files
4. User reviews before submitting

### OpenCode (sst)

**Tiered approach:**

1. Pruning first - Remove old tool outputs
2. Protected window - 40K tokens recent context
3. Compaction last resort - Full summarization

### Manus Insights

1. **Filesystem as ultimate context** - Keep paths, drop content
2. **Recitation for attention** - Constantly rewrite todo lists
3. **Keep failures in context** - Model learns from seeing mistakes

---

## 3. Model-Specific Thresholds

| Model           | Context | Degradation Onset | Notes                    |
| --------------- | ------- | ----------------- | ------------------------ |
| Claude Opus 4.5 | 200K    | 65-75% (130-150K) | Weak multi-hop >140K     |
| GPT-5.2         | 400K    | 70-80% (280-320K) | Sharp CoT degradation    |
| Gemini 3 Pro    | 2M      | 60-70% (1.2-1.4M) | 98% NIAH to 1M           |
| DeepSeek v3.2   | 256K    | 80-90% (200-230K) | Best-in-class, MLA helps |
| DeepSeek v4     | 1M      | 85-95% (850-950K) | 92% sustained recall     |

### Production Recommendations

| Use Case                   | Max Fill | Rationale          |
| -------------------------- | -------- | ------------------ |
| Conservative (high-stakes) | ≤60%     | Finance/legal      |
| Balanced (general apps)    | 65-75%   | Standard           |
| Aggressive (research)      | 80%      | DeepSeek/Grok only |

**Key stat**: 92% uptime at <70% vs 76% at >80% (Honeycomb 2026)

### Compaction Triggers

| Condition           | Threshold       | Rationale                  |
| ------------------- | --------------- | -------------------------- |
| % Context Filled    | ≥60-70%         | Latency/hallucination rise |
| Perplexity Spike    | >1.15x baseline | Early degradation signal   |
| Retrieval F1/Recall | <0.90           | Quality dropping           |
| Token Age           | >50% window age | "Lost in middle" proxy     |

---

## 4. What to Preserve vs Summarize

### Always Preserve Verbatim

| Content Type          | Reason                       |
| --------------------- | ---------------------------- |
| **Failed approaches** | Prevents retry loops         |
| **Error messages**    | Exact wording for debugging  |
| **File paths/URLs**   | Restorable references        |
| **Key decisions**     | Architectural continuity     |
| **Recent turns**      | Active working context (40K) |

### Safe to Summarize

| Content Type            | Ratio | Format                    |
| ----------------------- | ----- | ------------------------- |
| Exploratory discussions | 10:1  | "Explored X, concluded Y" |
| Successful tool outputs | 50:1  | Path + summary            |
| Alternative approaches  | 20:1  | "Considered A, chose C"   |

### Safe to Drop

- Greetings/pleasantries
- Redundant re-explanations
- Superseded code versions

---

## 5. Token Counting in Rust

ion uses `bpe-openai` for token counting.

```rust
use tiktoken_rs::cl100k_base;

let bpe = cl100k_base().unwrap();
let tokens = bpe.encode_with_special_tokens("Your text here");
println!("Token count: {}", tokens.len());
```

### Fast Estimation

```rust
/// Fast token estimation (~4 chars per token)
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}
```

---

## 6. Implementation for ion

### Phase 1: Tiered Pruning

```rust
pub enum PruningTier {
    TruncateOutputs { max_per_output: usize },
    RemoveOldOutputs { protect_turns: usize },
    Summarize { focus_hint: Option<String> },
    Aggressive,
}

async fn prune_context(context: &mut Context, target: usize) -> Result<()> {
    for tier in [
        PruningTier::TruncateOutputs { max_per_output: 2000 },
        PruningTier::RemoveOldOutputs { protect_turns: 5 },
        PruningTier::Summarize { focus_hint: None },
    ] {
        apply_tier(&mut context, tier)?;
        if context.tokens() < target { return Ok(()); }
    }
}
```

### Phase 2: Model-Specific Configs

```rust
pub struct CompactionConfig {
    pub trigger_threshold: f32,  // 0.60-0.85 based on model
    pub target_threshold: f32,   // trigger - 0.15
    pub protected_suffix: usize, // 40K tokens
}
```

---

## Sources

### Production Research

- [Factory.ai: Compressing Context](https://factory.ai/news/compressing-context)
- [Amp: Handoff](https://ampcode.com/news/handoff)
- [OpenCode Context Management](https://deepwiki.com/sst/opencode/2.4-context-management-and-compaction)

### Context Engineering

- [Google Whitepaper: Context Engineering](https://www.kaggle.com/whitepaper-context-engineering-sessions-and-memory)
- [Manus: Context Engineering for AI Agents](https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus)
- [Anthropic: Effective Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)

### Benchmarks

- RULER, LongBench, InfiniteBench
- Honeycomb 2026 LLM Ops Report
- Scale AI LongEval 2026

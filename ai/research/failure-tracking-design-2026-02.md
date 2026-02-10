# Failure Tracking (Simple RLM) Design Research

**Date**: 2026-02-10
**Purpose**: Evaluate whether tracking tool failures across compaction improves agent performance
**Status**: Research complete, design proposed

---

## 1. Current Gap: How Failures Get Lost

### Flow Today

```
Tool error occurs
  -> ToolResult { is_error: true, content: error_msg }   (src/agent/tools.rs:58-72)
  -> Pushed into session.messages as ContentBlock          (src/agent/mod.rs:481-484)
  -> Visible to model in conversation history
  -> Compaction triggers at 80% of context window          (src/compaction/mod.rs:20)
  -> Tier 1: Truncate large tool outputs                   (src/compaction/pruning.rs:69-118)
  -> Tier 2: Remove old tool output content entirely       (src/compaction/pruning.rs:148-217)
  -> Tier 3: LLM summarizes, old messages replaced         (src/compaction/summarization.rs:56-117)
  -> Error details lost or compressed into summary
```

### What the Summarizer Preserves

The Tier 3 summarization prompt (`src/compaction/summarization.rs:23-36`) includes:

```
4. ERRORS: Problems encountered and resolutions
```

This is the current safety net. But it depends on the summarization model's judgment about which errors matter. In practice:

- The summarization model sees truncated tool outputs (Tier 1 already shortened them)
- Error context (what the agent tried, why it failed) is scattered across multiple messages
- The summarizer has no structured understanding of error categories
- A brief "ERRORS: edit mismatch on src/main.rs" note loses the specific `old_string` that failed and why

### The Concrete Failure Pattern

1. Agent tries `edit` with stale `old_string` -> error
2. Agent re-reads file, succeeds on retry
3. 50 turns later, compaction fires
4. Summarizer notes "edited src/main.rs" but drops the specific mismatch detail
5. Agent reads different file, builds stale mental model
6. Same edit mismatch pattern repeats -- no memory that this class of error happened before

### Evidence This Matters

**Recovery-Bench** (Letta, August 2025): Models show a 57% relative accuracy decrease when recovering from prior failures. Full action histories actually _hurt_ performance vs. brief summaries -- suggesting raw error context is actively confusing, but structured error summaries help.

**SABER** (ICLR 2026): Each additional deviation in a mutating action reduces success odds by up to 96%. Errors compound -- catching them early is critical.

**SWE-bench spiral analysis** (Surge HQ, September 2025): Agents that spiral into failure share a common trait: they don't recognize when they're repeating a failed strategy. The agent that recovered (Claude) explicitly flagged uncertainty; the one that spiraled (Gemini, 693 lines) never acknowledged mounting errors.

---

## 2. Industry Survey

### Do Any Agents Track Failures Across Compaction?

| Agent           | Failure Tracking               | Across Compaction?                                      | Notes                                                                                                                                                                      |
| --------------- | ------------------------------ | ------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Claude Code** | Errors in conversation history | Partial -- summarization prompt asks for errors section | No structured tracking. GitHub issues confirm context loss after compaction (#13919, #7919). Users report "repeated errors and massive productivity loss" post-compaction. |
| **Codex CLI**   | Errors in conversation history | Context compaction added late 2025                      | No evidence of structured failure memory                                                                                                                                   |
| **Amp**         | No explicit tracking           | N/A                                                     | "Aggressive subtraction" philosophy -- relies on model capability                                                                                                          |
| **Pi-Mono**     | No tracking                    | No compaction (minimal context approach)                | Philosophy: if models need reminding, the prompt is wrong                                                                                                                  |
| **OpenCode**    | No evidence                    | Unknown                                                 | Focus on LSP integration, not failure memory                                                                                                                               |
| **Gemini CLI**  | No evidence                    | 1M context reduces compaction need                      | Large window masks the problem but doesn't solve it                                                                                                                        |
| **Aider**       | Git-native -- can diff/revert  | N/A (no long sessions)                                  | Atomic commits = natural failure recovery via git                                                                                                                          |

**Finding: No coding agent currently implements structured failure tracking across compaction.** This is a genuine gap.

### Relevant Research

| Paper                                     | Key Finding                                                                         | Relevance                                                         |
| ----------------------------------------- | ----------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| **Recovery-Bench** (Letta, 2025)          | 57% accuracy drop on recovery tasks; brief summaries > full histories               | Structured failure summaries help; raw error dumps hurt           |
| **PALADIN** (2025)                        | Recovery rate: 32% -> 90% with structured failure injection                         | Failure taxonomy + exemplars dramatically improve recovery        |
| **Structured Reflection** (Meituan, 2025) | Error-to-correction pairs as training signal; 4 failure categories                  | Categorizing failures makes them actionable                       |
| **SABER** (ICLR 2026)                     | Mutating action errors compound catastrophically (96% odds reduction per deviation) | Tracking mutating failures (edits, writes) is highest priority    |
| **Agent-R** (ByteDance, 2025)             | Iterative self-training on failure trajectories                                     | Models can learn from structured failure data                     |
| **Agent-R1** (USTC, 2025)                 | End-to-end RL for multi-turn agents                                                 | Error signals during training; but we need inference-time benefit |
| **RLM** (Prime Intellect, 2025)           | Context management as first-class capability; Base 24% -> RLM 62%                   | Even without RL training, structured context management helps     |
| **AgenTracer** (2025)                     | Traces failure attribution across multi-agent systems                               | Identifying _which_ component failed matters for recovery         |

### Key Insight from Recovery-Bench

The most surprising finding: **providing full error histories hurts performance.** Models get confused by detailed failure narratives. What helps is a _structured summary_ -- exactly what we'd inject into the system prompt. This validates the "brief categorized records" approach over "dump all errors."

---

## 3. Design Proposal

### 3.1 Data Structure

```rust
/// A categorized failure record for cross-compaction persistence.
#[derive(Debug, Clone)]
pub struct FailureRecord {
    /// Which tool failed
    pub tool_name: String,
    /// Categorized error type for pattern detection
    pub category: FailureCategory,
    /// Brief description (max ~80 chars, model-readable)
    pub description: String,
    /// Monotonic turn counter (not wall-clock -- survives compaction)
    pub turn: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    /// edit tool: old_string didn't match file content
    EditMismatch,
    /// read/write/edit: file path doesn't exist or wrong path
    FileNotFound,
    /// bash: build/compile command returned non-zero
    BuildFailure,
    /// bash: test command returned non-zero
    TestFailure,
    /// Any tool: execution error (timeout, permission, etc.)
    ToolError,
    /// bash: command produced unexpected output suggesting wrong approach
    WrongApproach,
}
```

### 3.2 Storage

In-memory, per session. No persistence to disk -- failure context is session-scoped.

```rust
// In Session or Agent struct
pub struct FailureTracker {
    records: Vec<FailureRecord>,
    turn_counter: usize,
    max_records: usize,  // default: 10
}
```

**Why not persist to disk?** Failure context is tightly coupled to the current task. Yesterday's edit mismatches on a different branch are noise, not signal. Session-scoped keeps it relevant.

**Why in-memory Vec, not a database?** At 10 records max, the overhead is negligible. A Vec is simpler to reason about and zero-dependency.

### 3.3 Classification Logic

Classification happens in `src/agent/tools.rs` after tool execution, using simple pattern matching on the error message and tool name:

```rust
fn classify_failure(tool_name: &str, error_msg: &str) -> Option<(FailureCategory, String)> {
    match tool_name {
        "edit" => {
            if error_msg.contains("old_string not found")
                || error_msg.contains("no match")
                || error_msg.contains("does not match")
            {
                Some((FailureCategory::EditMismatch,
                    // Extract file path from args if available
                    "edit old_string mismatch".into()))
            } else {
                None
            }
        }
        "read" | "write" | "edit" if error_msg.contains("not found")
            || error_msg.contains("No such file") => {
            Some((FailureCategory::FileNotFound, truncate_desc(error_msg)))
        }
        "bash" => classify_bash_failure(error_msg),
        _ => Some((FailureCategory::ToolError, truncate_desc(error_msg))),
    }
}
```

For bash, heuristic detection of build vs. test failures:

```rust
fn classify_bash_failure(error_msg: &str) -> Option<(FailureCategory, String)> {
    // Build failures: compiler errors
    if error_msg.contains("error[E")         // rustc
        || error_msg.contains("error:")       // gcc/clang
        || error_msg.contains("FAILED")       // cargo build
        || error_msg.contains("SyntaxError")  // Python/JS
    {
        return Some((FailureCategory::BuildFailure, truncate_desc(error_msg)));
    }
    // Test failures
    if error_msg.contains("test result: FAILED")
        || error_msg.contains("FAIL ")
        || error_msg.contains("failures:")
    {
        return Some((FailureCategory::TestFailure, truncate_desc(error_msg)));
    }
    // Non-zero exit but no clear category
    if error_msg.contains("exit code") || error_msg.contains("exit status") {
        return Some((FailureCategory::ToolError, truncate_desc(error_msg)));
    }
    None // Not all bash results are failures worth tracking
}
```

### 3.4 Injection into System Prompt

Add a `recent_failures` variable to the minijinja template in `src/agent/context.rs`:

```jinja
{% if recent_failures %}
## Recent Failures

The following errors occurred during this session. Avoid repeating these patterns:
{% for f in recent_failures %}
- [{{ f.category }}] {{ f.tool_name }}: {{ f.description }} (turn {{ f.turn }})
{% endfor %}
{% endif %}
```

This renders as:

```
## Recent Failures

The following errors occurred during this session. Avoid repeating these patterns:
- [EditMismatch] edit: old_string mismatch on src/config.rs (turn 12)
- [BuildFailure] bash: error[E0308] expected &str, found String (turn 15)
- [TestFailure] bash: test_parse_config failed: assertion on line 42 (turn 18)
```

### 3.5 Token Budget

| Component                    | Tokens              |
| ---------------------------- | ------------------- |
| Section header + instruction | ~30                 |
| Per failure record           | ~40-60              |
| Max records (10)             | ~400-600            |
| **Total budget**             | **~500 tokens max** |

At 500 tokens out of a 200k context window, this is 0.25% overhead. Negligible.

**Eviction policy**: FIFO with category dedup. When adding record N+1 and at capacity:

1. If a record with the same category and tool exists, replace it (keep newest)
2. Otherwise, evict the oldest record

This prevents the list from being dominated by a single repeated failure type while keeping the most recent instance of each pattern.

### 3.6 When to NOT Track

Not every error deserves tracking:

- **Successful retries within the same turn**: If the agent immediately re-reads and retries an edit successfully, the error was handled in-context. No record needed.
- **User-cancelled operations**: Not a failure, just an interruption.
- **Expected exploratory failures**: `glob` returning no matches isn't an error pattern.
- **Duplicate within last 2 turns**: If the model is already seeing the error in conversation history, adding it to the tracker is redundant.

---

## 4. Implementation Estimate

### Files Changed

| File                         | Change                                                         | LOC (est.) |
| ---------------------------- | -------------------------------------------------------------- | ---------- |
| `src/agent/failure.rs` (new) | `FailureRecord`, `FailureCategory`, `FailureTracker`           | ~120       |
| `src/agent/tools.rs`         | Call `classify_failure()` on error results, push to tracker    | ~40        |
| `src/agent/context.rs`       | Add `recent_failures` to template + `render_system_prompt()`   | ~25        |
| `src/agent/mod.rs`           | Add `FailureTracker` to `Agent`, wire through `execute_turn`   | ~15        |
| `src/session/mod.rs`         | Optionally store tracker ref on `Session` (or keep on `Agent`) | ~5         |
| Tests                        | Unit tests for classification, eviction, template rendering    | ~100       |
| **Total**                    |                                                                | **~305**   |

### Complexity: Low

- No new dependencies
- No async complexity (tracker is synchronous, protected by existing locks)
- No disk I/O
- Pattern matching on strings is straightforward
- Template change is additive (existing variables unchanged)

### Where the Tracker Lives

Two options:

**Option A: On Agent** (recommended)

- `Agent` already holds `context_manager`, `compaction_config`, etc.
- Tracker survives `Session` replacement (if sessions are swapped)
- Natural place for session-scoped state that outlives compaction

**Option B: On Session**

- Simpler ownership (session is mutable in the agent loop)
- But `Session` is serialized to `SessionStore` -- tracker data would either be lost or need schema change

Recommendation: **Option A.** Add `failure_tracker: Arc<Mutex<FailureTracker>>` to `Agent`.

---

## 5. Expected Benefit

### Primary: Reduced Failure Cycles After Compaction

The main win is breaking the "try -> fail -> compaction -> forget -> try same thing -> fail" loop. This is the most common complaint in Claude Code GitHub issues about compaction (#13919).

**Quantitative estimate**: Based on Recovery-Bench data (57% accuracy drop on recovery tasks), structured failure context should recover a significant fraction of that gap. Conservative estimate: 20-30% reduction in repeated failures post-compaction.

### Secondary Benefits

| Benefit                           | Mechanism                                                                       |
| --------------------------------- | ------------------------------------------------------------------------------- |
| **Faster convergence**            | Model sees "EditMismatch on src/config.rs" and re-reads the file before editing |
| **Better error messages to user** | Tracker provides structured view of what went wrong in session                  |
| **Telemetry foundation**          | Failure categories enable aggregate analysis across sessions                    |
| **Compaction quality signal**     | If failures spike post-compaction, the summarizer is losing important context   |
| **Model comparison**              | Track which models produce more failures of each category                       |

### What This Does NOT Solve

- **Novel failures**: Only helps with pattern recognition, not preventing new errors
- **Fundamental capability gaps**: If the model cannot solve the problem, tracking failures won't help
- **Cross-session learning**: This is in-memory per session, not persistent memory
- **Hallucination spirals**: The SWE-bench analysis shows these are confidence failures, not memory failures

---

## 6. Risks

### 6.1 Token Overhead

**Risk**: 500 tokens wasted when no compaction has occurred and errors are still in conversation history.

**Mitigation**: Only inject the failures section after the first compaction event. Before compaction, errors are visible in conversation history. After compaction, the tracker becomes the memory.

```rust
// In ContextManager::render_system_prompt
let recent_failures = if tracker.has_compacted_at_least_once() {
    tracker.recent_failures()
} else {
    vec![]
};
```

### 6.2 Stale Failure Data

**Risk**: An EditMismatch from turn 5 is still showing at turn 80 when the file has been completely rewritten.

**Mitigation**: The FIFO eviction naturally ages out old records. With max 10 records and a session producing ~1 failure every 5-10 turns, records naturally cycle. Additionally, the dedup-on-category policy means a new EditMismatch replaces the old one.

### 6.3 Over-Cautious Behavior

**Risk**: Model becomes overly defensive after seeing failure records. For example, re-reading every file before every edit even when unnecessary.

**Mitigation**:

1. The prompt framing says "avoid repeating these patterns" not "be extra careful about everything"
2. Limit to 10 records max -- the list never becomes overwhelming
3. Category specificity helps: "EditMismatch on src/config.rs" doesn't imply all edits are risky

### 6.4 False Classification

**Risk**: `classify_bash_failure` heuristics misclassify output (e.g., a grep for "error:" hits the pattern).

**Mitigation**: Only classify `is_error: true` results. Non-error bash output is never classified. The heuristics are intentionally conservative -- missing some errors is better than false positives.

### 6.5 Interaction with Summarization

**Risk**: The Tier 3 summarizer already produces an ERRORS section. Now there are two sources of error information in the system prompt.

**Mitigation**: These serve different purposes and complement each other:

- Summarizer ERRORS: Narrative context about what happened and how it was resolved
- Failure tracker: Structured, categorized, actionable patterns to avoid

If desired, the summarization prompt could be updated to say "Note: recent failures are tracked separately in the system prompt. Focus your ERRORS section on resolved issues and their resolutions."

---

## 7. Comparison to Full RLM Approach

The RLM paper (Prime Intellect) proposes models that manage their own context via a Python REPL. Our approach is much simpler but captures the key insight: **context that survives compaction should be structured, not narrative.**

| Aspect                    | Full RLM                    | Ion Failure Tracking                                    |
| ------------------------- | --------------------------- | ------------------------------------------------------- |
| Training required         | Yes (RL)                    | No                                                      |
| Implementation complexity | High (REPL, sub-LLMs)       | Low (~300 LOC)                                          |
| Context management        | Model-driven                | Heuristic + template                                    |
| Error recovery            | Learned behavior            | Pattern recognition via prompt                          |
| Benefit ceiling           | High (62% vs 24% on CodeQA) | Moderate (estimated 20-30% reduction in failure cycles) |
| Risk                      | Requires RL infrastructure  | Minimal (in-memory, additive)                           |

This is the "80/20" version: capture most of the benefit of persistent failure awareness without any training infrastructure.

---

## 8. Recommendation

**Build it.** The implementation is small (~300 LOC), the risk is low (additive, no breaking changes), and the gap is real (no competitor does this). The research strongly supports that structured failure summaries improve recovery -- Recovery-Bench, PALADIN, and SABER all point in this direction.

**Priority**: Medium. This is a quality-of-life improvement that becomes more valuable as sessions get longer and compaction fires more frequently. It's not blocking any feature work, but it's a differentiator vs. every other coding agent.

**Suggested implementation order**:

1. `FailureRecord` + `FailureCategory` + `FailureTracker` (data structures)
2. Classification logic in `tools.rs` (pattern matching)
3. Template integration in `context.rs` (minijinja)
4. Wire through `Agent` (plumbing)
5. Add post-compaction-only gate
6. Tests

---

## Sources

### Primary Research

- [Recovery-Bench](https://www.letta.com/blog/recovery-bench) -- Letta, August 2025. Agent error recovery benchmark.
- [PALADIN](https://arxiv.org/abs/2509.25238) -- Self-correcting agents for tool failures. Recovery rate 32% -> 90%.
- [SABER](https://openreview.net/forum?id=JuwuBUnoJk) -- ICLR 2026. Mutating action errors compound catastrophically.
- [Structured Reflection](https://arxiv.org/abs/2509.18847) -- Meituan, 2025. Error-to-correction training pairs.
- [Agent-R1](https://arxiv.org/abs/2511.14460) -- USTC. End-to-end RL for multi-turn agents.
- [RLM](https://www.primeintellect.ai/blog/rlm) -- Prime Intellect, 2025. Recursive Language Models.

### Failure Analysis

- [SWE-bench spirals](https://surgehq.ai/blog/when-coding-agents-spiral-into-693-lines-of-hallucinations) -- Surge HQ. How coding agents spiral from small mistakes.
- [Failures in Automated Issue Solving](https://arxiv.org/abs/2509.13941) -- Empirical study of agent failures.
- [AgenTracer](https://arxiv.org/abs/2509.03312) -- Failure attribution in multi-agent systems.
- [SWE-bench evaluation at scale](https://www.ai21.com/blog/scaling-agentic-evaluation-swe-bench/) -- AI21, 200k runs analysis.

### Industry Evidence

- [Claude Code #13919](https://github.com/anthropics/claude-code/issues/13919) -- Skills context lost after compaction.
- [Claude Code #7919](https://github.com/anthropics/claude-code/issues/7919) -- Feature request: preserve messages after compaction.
- [Why LLM Agents Still Fail](https://www.atla-ai.com/post/why-llm-agents-still-fail) -- Atla, May 2025.

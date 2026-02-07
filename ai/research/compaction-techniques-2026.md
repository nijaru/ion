# Context Compaction Techniques for Coding Agents (2026)

**Research Date**: 2026-02-06
**Purpose**: Survey state-of-the-art compaction/summarization approaches across coding agents, academic research, and industry, to inform ion's Tier 3 LLM-based compaction design.
**Current System**: Mechanical pruning (Tier 1: truncate large outputs, Tier 2: remove old tool outputs) with configurable trigger/target thresholds (80%/60%).

---

## Executive Summary

| Approach                                                        | Cost       | Quality  | Latency     | Adoption                               |
| --------------------------------------------------------------- | ---------- | -------- | ----------- | -------------------------------------- |
| **Observation masking** (replace old outputs with placeholders) | Low        | High     | None        | Claude Code, ion, OpenCode             |
| **LLM summarization** (structured prompt to condense)           | Medium     | High     | 2-8s        | Claude Code, Cline, OpenCode           |
| **Hybrid** (mask first, summarize if needed)                    | Low-Medium | Highest  | Conditional | Recommended (JetBrains, this research) |
| **File externalization** (write outputs to files, reference)    | Low        | Lossless | None        | Cursor, Claude Code (microcompaction)  |
| **Neural pruning** (small model filters irrelevant content)     | Low        | High     | <100ms      | SWE-Pruner (research)                  |

**Key finding**: JetBrains Research (2025) demonstrated that simple observation masking matches or exceeds LLM summarization in 4/5 configurations, halves costs, and avoids trajectory elongation. The optimal strategy is **hybrid**: mechanical pruning as the primary mechanism, LLM summarization as fallback for genuinely long contexts.

**Recommendation for ion**: Our existing Tier 1/2 mechanical pruning is already the right foundation. Add Tier 3 as structured LLM summarization using a small/fast model, triggered only when mechanical pruning cannot reach the target threshold.

---

## 1. Claude Code (Anthropic)

**Source**: Reverse-engineered from `@anthropic-ai/claude-code` v2.1.17 ([decodeclaude.com](https://decodeclaude.com/claude-code-compaction/))

### Architecture

Claude Code uses a three-layer compaction system:

| Layer                 | Mechanism                                 | Details                                                                                                                                         |
| --------------------- | ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| **Microcompaction**   | Externalize old tool outputs to disk      | Last 3 tool results stay in context; older ones become `Tool result saved to: /path`. Triggers when >20K tokens saveable, targeting 40K budget. |
| **Auto-compaction**   | 9-section structured summarization        | Triggers at ~78% capacity. Uses the same model (Opus/Sonnet).                                                                                   |
| **Manual `/compact`** | User-triggered with optional focus prompt | Same mechanism, user controls timing and focus.                                                                                                 |

### The 9-Section Summarization Prompt

When compaction triggers, Claude receives a structured prompt requesting:

1. **Primary Request and Intent** -- all explicit user requests
2. **Key Technical Concepts** -- technologies and frameworks discussed
3. **Files and Code Sections** -- specific files examined/modified with code snippets
4. **Errors and Fixes** -- all encountered errors and resolutions
5. **Problem Solving** -- problems solved and ongoing troubleshooting
6. **All User Messages** -- every non-tool-result user message (feedback context)
7. **Pending Tasks** -- explicitly requested work items
8. **Current Work** -- immediate pre-summary activity with filenames and snippets
9. **Optional Next Step** -- next action aligned with recent requests

The prompt emphasizes being "thorough in capturing technical details, code patterns, and architectural decisions."

### Post-Compaction Restoration

After summarization, Claude Code:

- Re-reads up to **5 most recently accessed files** (max 5K tokens each)
- Restores todo lists and plan files
- Preserves SessionStart hook outputs
- Injects continuation message: "Please continue the conversation from where we left it off without asking the user any further questions."

### What Gets Discarded

- Detailed analysis in XML tags (converted to plain text)
- Old tool outputs beyond last 3
- Multiple consecutive newlines (collapsed)

### Thresholds

| Parameter            | Value                           |
| -------------------- | ------------------------------- |
| Context window       | 200K tokens                     |
| Output reserve       | 32K (configurable, max 64K)     |
| Safety buffer        | 13K                             |
| Trigger point        | ~78% utilization                |
| Post-compact check   | Every 5K tokens or 3 tool calls |
| Minimum conversation | 10K tokens before eligible      |

### Background Agents

Background (sub) agents use **delta summarization** -- 1-2 sentence incremental updates rather than full 9-section prompts. This avoids polluting the main conversation.

### Key Design Insight

Claude Code uses the **same large model** for summarization (not a smaller one). The 9-section structured prompt ensures consistent, thorough summaries. This is expensive but high-quality.

---

## 2. Cline (VS Code Agent)

**Source**: [docs.cline.bot/features/auto-compact](https://docs.cline.bot/features/auto-compact), [Cline v3.25 release](https://cline.ghost.io/cline-v3-25/)

### Architecture

| Feature           | Details                                                                                                               |
| ----------------- | --------------------------------------------------------------------------------------------------------------------- |
| **Auto Compact**  | Triggers near context limit. Creates comprehensive summary preserving technical details, code changes, and decisions. |
| **Focus Chain**   | Todo list injected every 6 messages to counteract "lost in the middle" phenomenon.                                    |
| **Deep Planning** | `/deep-planning`: 4-step process (investigate, ask, plan, fresh start) that eliminates exploration-phase pollution.   |

### Summarization Model

Uses the **same model** as the conversation (your configured API provider). Falls back to rule-based truncation for models that do not support summarization well.

### Supported Models for Auto-Compact

Claude 4 series, Gemini 2.5 series, GPT-5, Grok 4. Others fall back to rule-based context truncation.

### Cost Optimization

Leverages prompt caching: "most input tokens are already cached, you're primarily paying for the summary generation (output tokens)."

### Key Design Insight

The **Focus Chain** (periodic todo re-injection) is a clever complement to compaction. It addresses attention drift without summarization cost. Worth considering for ion -- periodically re-inject task state into conversation even before compaction triggers.

### Known Issues

Issue #5616 reports "excessive token burning and context loss" from aggressive auto-compact. This suggests using the same large model for summarization can be wasteful if triggered too aggressively.

---

## 3. OpenCode

**Source**: [deepwiki.com/sst/opencode](https://deepwiki.com/sst/opencode/2.4-context-management-and-compaction)

### Architecture

| Component               | Details                                                                                                                                                   |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Overflow detection**  | Checks after each prompt loop. Configurable via `config.compaction.auto`                                                                                  |
| **Compaction agent**    | Specialized agent with prompt: "Provide a detailed prompt for continuing our conversation... what we did, what we're doing, which files we're working on" |
| **Tool output pruning** | Marks old outputs with `compacted` timestamp. Protected threshold: 40K tokens (recent), minimum savings: 20K tokens.                                      |
| **Skill protection**    | Skill tool outputs never pruned (critical context).                                                                                                       |

### Model Selection

Uses the model from the last user message (i.e., the same model). Can be customized via `config.agent.compaction`.

### Post-Compaction

Creates a `summary: true` assistant message, then injects a synthetic user message to prompt continuation without explicit user input.

### Prompt Caching

Provider-specific cache control:

- Anthropic/OpenRouter: `cacheControl: {type: "ephemeral"}` at message level
- OpenAI: `cache_control: {type: "ephemeral"}` at content level
- Targets system messages (first 2) and recent messages (last 2)

---

## 4. Cursor

**Source**: [cursor.com/blog/dynamic-context-discovery](https://cursor.com/blog/dynamic-context-discovery)

### Approach: Dynamic Context Discovery

Cursor takes a fundamentally different approach -- instead of summarizing, it **externalizes context to files**:

| Technique                 | Details                                                                                                       |
| ------------------------- | ------------------------------------------------------------------------------------------------------------- |
| **Tool outputs to files** | Long tool responses written to filesystem. Agent explores incrementally (e.g., `tail` first, then read more). |
| **Chat history as files** | After summarization, agents can recover details by searching chat history files.                              |
| **Terminal sync**         | Terminal outputs sync to local filesystem for grep-based access.                                              |
| **MCP tool lazy loading** | Only tool names loaded initially. Full descriptions fetched on demand. 46.9% token reduction.                 |

### Key Design Insight

"Files have been a simple and powerful primitive" for context management. Rather than lossy compression, Cursor treats context as an external store the agent can query. This is **lossless** -- nothing is destroyed, just moved out of the primary context window.

The agent's knowledge degrades less because it can always re-read. The tradeoff is that re-reading costs tokens too, but only on demand.

---

## 5. Codex CLI (OpenAI)

**Source**: [github.com/openai/codex](https://github.com/openai/codex), issues #1257, #3416

### Architecture

Codex CLI originally lacked compaction in the Rust rewrite (TypeScript had `/compact`). Key design:

| Aspect                   | Details                                                  |
| ------------------------ | -------------------------------------------------------- |
| **Task types**           | `CompactTask` is a dedicated task type for summarization |
| **Auto-compact trigger** | Configurable threshold (triggers on context overflow)    |
| **Strategy**             | Summarize + truncate oldest items                        |
| **Token tracking**       | Real-time updates after each turn                        |

### Current Status

The Rust CLI added `/compact` support (issue #1257 closed). Context compression via history message summarization was added (issue #3416 closed). Implementation uses a dedicated summarization prompt similar to Claude Code's approach.

---

## 6. Windsurf (Cascade)

**Source**: [byteiota.com](https://byteiota.com/windsurf-cascade-gpt52-codex-gemini-3-jan-2026-2/), product docs

### Approach

Windsurf's Cascade agent uses:

- **Autonomous memory** that learns project tech stack across sessions
- **Real-time action awareness** monitoring file edits, terminal commands, clipboard
- **Planning agent** for long-term strategy

Context management details are not publicly documented in depth. The system appears to rely on large context windows and proprietary context engineering rather than explicit compaction.

---

## 7. Other Agents

| Agent            | Strategy                              | Notes                                                     |
| ---------------- | ------------------------------------- | --------------------------------------------------------- |
| **Gemini CLI**   | Conversation checkpointing            | 1M token context reduces compaction need                  |
| **Pi-Mono**      | Planned (issue #92)                   | Minimalist: <1000 token system prompt, avoids the problem |
| **Crush**        | Session-based, model switching        | No explicit compaction documented                         |
| **Continue.dev** | Plan Mode (read-only) + context rules | Focuses on context engineering over compaction            |
| **aider**        | Chat history summarization            | Summarizes older messages, keeps recent turns             |
| **Zed AI**       | Not publicly documented               | Inline agent, likely relies on provider context           |

---

## 8. Academic/Industry Research

### "The Complexity Trap" (JetBrains Research, 2025)

**Paper**: [arxiv.org/abs/2508.21433](https://arxiv.org/abs/2508.21433)
**Blog**: [blog.jetbrains.com/research](https://blog.jetbrains.com/research/2025/12/efficient-context-management/)

**Key findings**:

| Finding                                       | Detail                                                                                                           |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Observation masking matches LLM summarization | In 4/5 configurations, simple masking (replacing old outputs with placeholders) performed as well or better      |
| Cost savings                                  | Both approaches: >50% reduction. Masking with Qwen3-Coder 480B: 52% cost reduction + 2.6% solve rate improvement |
| Trajectory elongation                         | LLM summarization caused agents to run 15% longer (52 vs 45 turns avg) -- summaries obscure stopping signals     |
| Hidden summarization costs                    | Summary generation consumed >7% of total costs for largest models                                                |
| Optimal window size                           | 10 turns for SWE-agent. Agent-specific tuning required.                                                          |

**Recommendation**: Hybrid approach -- observation masking primary, selective LLM summarization for particularly long contexts.

### ACON: Agent Context Optimization (Microsoft, 2025)

**Paper**: [arxiv.org/abs/2510.00615](https://arxiv.org/abs/2510.00615)

**Key contributions**:

| Aspect                     | Detail                                                                                                                                       |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| **Framework**              | Selective compression when thresholds exceeded (4096 tokens history, 1024 observations)                                                      |
| **Guideline optimization** | Failure-driven: analyzes cases that succeeded without compression but failed with it, to identify missing critical info                      |
| **What to preserve**       | Factual history, action-outcome relationships, evolving state, success preconditions, future decision cues                                   |
| **Results**                | 26-54% memory reduction. Distilled to smaller models retaining 95%+ performance                                                              |
| **Key insight**            | Moderate compression thresholds are optimal. Aggressive compression degrades accuracy. Task-aware guidelines outperform naive summarization. |

### SWE-Pruner: Self-Adaptive Context Pruning (2026)

**Paper**: [arxiv.org/abs/2601.16746](https://arxiv.org/html/2601.16746v1)

**Key contributions**:

| Aspect          | Detail                                                                                                                                            |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Problem**     | Read operations consume 76.1% of tokens in SWE-Bench tasks                                                                                        |
| **Approach**    | 0.6B parameter neural skimmer performs line-level relevance scoring guided by agent's explicit goal hints                                         |
| **Results**     | 23-38% token reduction with <1% performance loss. 87.3% AST correctness (vs near-zero for token-level pruning)                                    |
| **Latency**     | <100ms even at 8K tokens                                                                                                                          |
| **Key insight** | Line-level pruning preserves code structure better than token-level. Task-aware hints ("focus on error handling") dramatically improve relevance. |

### MemGPT: LLMs as Operating Systems (UC Berkeley, 2023-2024)

**Paper**: [arxiv.org/abs/2310.08560](https://arxiv.org/abs/2310.08560)

**Key concepts**:

| Concept                        | Detail                                                                                         |
| ------------------------------ | ---------------------------------------------------------------------------------------------- |
| **Virtual context management** | OS-inspired paging between "main memory" (context window) and "disk" (external storage)        |
| **Memory tiers**               | Working memory (in-context), archival memory (vector DB), recall memory (conversation history) |
| **Primitives**                 | Store, retrieve, summarize, update -- explicit memory management operations                    |
| **Self-directed**              | LLM decides when to page data in/out based on current needs                                    |

**Relevance to ion**: MemGPT's hierarchical approach maps well to our tiered system. Our Tier 1/2 is "mechanical paging," Tier 3 would be "intelligent summarization." Future memory system could implement full MemGPT-style recall/archival tiers.

### HMT: Hierarchical Memory Transformer (2024-2025)

**Paper**: [arxiv.org/abs/2405.06067](https://arxiv.org/abs/2405.06067)

Learned memory tokens that compress earlier context into fixed-size representations. Requires model fine-tuning, so not applicable to API-based agents, but the concept of hierarchical compression (segment -> compress -> retain key info) informs prompt design.

### Breadcrumbs Reasoning (Cornell, 2025)

**Paper**: [arxiv.org/abs/2510.13797](https://arxiv.org/html/2510.13797v1)

Periodically compresses the KV cache during generation using learned compression tokens. Also requires model-level changes, but validates that older reasoning tokens lose informational value over time -- supporting aggressive pruning of old tool outputs.

---

## 9. What to Preserve During Compaction

Cross-referencing all sources, here is a consensus on critical preservation targets:

### Must Preserve (consensus across all agents)

| Category                             | Why                                    | Source                                        |
| ------------------------------------ | -------------------------------------- | --------------------------------------------- |
| **Current task/intent**              | Agent needs to know what it's doing    | All agents                                    |
| **Files read/written/edited**        | Path list enables re-reading           | Claude Code (section 3), OpenCode             |
| **Errors encountered and solutions** | Prevents retry loops                   | Claude Code (section 4), ACON                 |
| **Key decisions made**               | Prevents contradicting earlier choices | Claude Code (section 5), Cline Focus Chain    |
| **Pending/remaining work**           | Task continuity                        | Claude Code (sections 7-8), Cline Focus Chain |
| **User preferences/corrections**     | Behavioral calibration                 | Claude Code (section 6)                       |

### Should Preserve (high value, lower consensus)

| Category                                        | Why                          | Source                              |
| ----------------------------------------------- | ---------------------------- | ----------------------------------- |
| **Code snippets discussed**                     | Context for ongoing work     | Claude Code (section 8)             |
| **Git operations performed**                    | Undo/history context         | ACON (action-outcome relationships) |
| **Architecture/design patterns**                | Consistency across session   | Claude Code (section 2)             |
| **Tool call sequence** (names, not full output) | Understanding what was tried | ACON, SWE-Pruner                    |

### Safe to Discard

| Category                            | Why                                      | Source                          |
| ----------------------------------- | ---------------------------------------- | ------------------------------- |
| **Full tool output content**        | Re-readable from filesystem              | All agents                      |
| **Intermediate reasoning**          | Decisions matter, not deliberation       | JetBrains (observation masking) |
| **Verbose explanations**            | Summary sufficient                       | Claude Code (XML tag stripping) |
| **Duplicate file reads**            | Latest version sufficient                | Common sense                    |
| **Search results already acted on** | Decision preserved, raw data discardable | Cursor (file externalization)   |

---

## 10. Structured vs Narrative Summary

### Comparison

| Aspect                | Structured (sections/metadata)        | Narrative (prose summary)     | Hybrid                                               |
| --------------------- | ------------------------------------- | ----------------------------- | ---------------------------------------------------- |
| **Precision**         | High -- specific fields, paths, lists | Medium -- may omit details    | High                                                 |
| **LLM parseability**  | High -- clear delimiters              | Medium -- requires extraction | High                                                 |
| **Compression ratio** | Medium -- structured overhead         | High -- prose is dense        | Medium-High                                          |
| **Completeness**      | Enforced by template                  | Depends on model quality      | Enforced                                             |
| **Implementation**    | Structured prompt with sections       | Simple "summarize" prompt     | Structured prompt allowing narrative within sections |

### Industry Practice

| Agent           | Format                                                                         |
| --------------- | ------------------------------------------------------------------------------ |
| **Claude Code** | Structured (9 sections)                                                        |
| **OpenCode**    | Narrative with structured hints ("what we did, what we're doing, which files") |
| **Cline**       | Comprehensive summary (narrative with structured elements)                     |
| **Cursor**      | Externalized (file-based, not summary)                                         |

### Recommendation

**Hybrid structured**: Use Claude Code's approach of defined sections, but allow narrative content within each section. This ensures completeness (every section must be addressed) while allowing natural expression of complex relationships.

Proposed sections for ion:

1. **Task State** -- current goal, progress, remaining work
2. **Files Touched** -- paths read, written, edited (list format)
3. **Tool History** -- tool calls made and key outcomes (condensed)
4. **Errors and Solutions** -- problems encountered and how resolved
5. **Decisions** -- architectural/design choices made and rationale
6. **User Preferences** -- corrections, style guidance, constraints
7. **Next Steps** -- immediate action to resume

---

## 11. Cost/Quality Tradeoffs

### Model Size for Summarization

| Approach                           | Cost        | Quality       | Latency | Who Uses It                              |
| ---------------------------------- | ----------- | ------------- | ------- | ---------------------------------------- |
| **Same large model**               | High        | Highest       | 2-8s    | Claude Code, Cline, OpenCode             |
| **Small/fast model** (Haiku-class) | Low (~1/10) | Good          | <1s     | Claude Code sub-agents (delta summaries) |
| **Dedicated fine-tuned model**     | Low         | High for task | <1s     | SWE-Pruner (0.6B skimmer)                |
| **No model** (mechanical only)     | Zero        | N/A           | None    | ion current, observation masking         |

### Cost Analysis

For a 200K context window at 80% utilization (~160K tokens):

| Model            | Input Cost | Output Cost (5K summary) | Total  | Notes                                          |
| ---------------- | ---------- | ------------------------ | ------ | ---------------------------------------------- |
| Opus 4.6         | $2.40      | $0.075                   | ~$2.48 | Same quality, expensive                        |
| Sonnet 4.5       | $0.48      | $0.030                   | ~$0.51 | Good balance                                   |
| Haiku 4.5        | $0.16      | $0.010                   | ~$0.17 | Best cost/quality for structured summarization |
| Gemini 2.5 Flash | $0.012     | $0.020                   | ~$0.03 | Cheapest, good quality                         |

**Note**: Prompt caching significantly reduces input costs. With Anthropic's caching, repeated input tokens cost ~$0.03/MTok (90% reduction). If conversation is already cached, the summarization input cost approaches zero.

### Recommendation for ion

Use a **configurable small/fast model** for Tier 3 summarization, defaulting to the cheapest available model from the active provider. Rationale:

1. **Structured prompt compensates for model size** -- the 7-section template constrains what the model must produce, reducing the need for sophisticated reasoning
2. **JetBrains research** shows summarization quality matters less than completeness -- masking alone is competitive
3. **Prompt caching** makes input costs near-zero for already-cached conversations
4. **Latency matters** -- users notice 5-8s pauses. <1s is imperceptible

Fallback: if no small model available, use the active model. Allow user override via config.

---

## 12. Recommended Design for ion

### Tiered Compaction System

```
Tier 1: Truncate large tool outputs (>2K tokens) to head+tail
  |  Still over target?
Tier 2: Remove old tool output content (keep placeholder)
  |  Still over target?
Tier 3: LLM-based structured summarization
  |  Replace oldest N message groups with summary message
```

### Tier 3 Implementation Plan

**Trigger**: After Tier 1 + Tier 2, if `tokens_after > target_tokens`.

**Model selection priority**:

1. User-configured `compaction.model` (if set)
2. Cheapest model from active provider (Haiku for Anthropic, Flash for Google, etc.)
3. Active session model (fallback)

**Summarization prompt** (7 sections):

```
Summarize the conversation so far for continuation. Be thorough with
technical details. Organize your summary into these sections:

1. TASK STATE: Current goal, progress percentage, remaining work items
2. FILES: List all file paths read, written, or edited (full paths)
3. TOOL HISTORY: Tools called and key outcomes (tool name, key result)
4. ERRORS: Problems encountered and how they were resolved
5. DECISIONS: Architectural or design choices made and why
6. USER GUIDANCE: Any corrections, preferences, or constraints from the user
7. NEXT STEPS: What to do next to continue the work

Focus on information needed to continue without re-asking the user.
Preserve exact file paths, error messages, and code patterns.
```

**Post-compaction**:

- Replace messages[0..cutoff] with a single System or User message containing the summary
- Keep the last `protected_messages` intact
- Inject continuation prompt: "Continue from the summary above. Do not re-ask what the user wants."
- Optionally: re-read the 3 most recently touched files (if file tracking is available)

**Message structure after compaction**:

```
[System prompt]
[Summary message (Tier 3 output)]
[Protected recent messages (last N)]
```

### Configuration

```toml
[compaction]
trigger_threshold = 0.80
target_threshold = 0.60
protected_messages = 12
max_tool_output_tokens = 2000

# Tier 3 LLM summarization
summarization_enabled = true
summarization_model = ""  # empty = auto-select cheapest
```

### Future Enhancements (not Tier 3 scope)

| Enhancement                                  | Value                         | Complexity |
| -------------------------------------------- | ----------------------------- | ---------- |
| **Focus Chain** (periodic task re-injection) | Counteracts attention drift   | Low        |
| **File externalization** (Cursor-style)      | Lossless, re-queryable        | Medium     |
| **Delta summarization** for sub-agents       | Efficient incremental updates | Medium     |
| **MemGPT-style recall memory**               | Cross-session knowledge       | High       |
| **Neural skimmer** (SWE-Pruner-style)        | Input-side compression        | High       |

---

## References

### Agent Implementations

- Claude Code compaction: https://decodeclaude.com/claude-code-compaction/
- Claude Code internals: https://kotrotsos.medium.com/claude-code-internals-lessons-learned-and-whats-next-551092abeb5d
- Cline auto-compact: https://docs.cline.bot/features/auto-compact
- Cline v3.25 Focus Chain: https://cline.ghost.io/cline-v3-25/
- Cline context management: https://docs.cline.bot/prompting/understanding-context-management
- OpenCode compaction: https://deepwiki.com/sst/opencode/2.4-context-management-and-compaction
- Cursor dynamic context: https://cursor.com/blog/dynamic-context-discovery
- Cursor agent best practices: https://cursor.com/blog/agent-best-practices
- Codex CLI /compact issue: https://github.com/openai/codex/issues/1257
- Context engineering overview: https://karun.me/blog/2025/12/31/context-engineering-for-ai-assisted-development/

### Academic Papers

- "The Complexity Trap" (JetBrains/observation masking vs summarization): https://arxiv.org/abs/2508.21433
- ACON (Agent Context Optimization, Microsoft): https://arxiv.org/abs/2510.00615
- SWE-Pruner (self-adaptive context pruning): https://arxiv.org/abs/2601.16746
- MemGPT (hierarchical memory management): https://arxiv.org/abs/2310.08560
- HMT (Hierarchical Memory Transformer): https://arxiv.org/abs/2405.06067
- Breadcrumbs Reasoning (KV cache compression): https://arxiv.org/abs/2510.13797

### Industry Analysis

- JetBrains Research blog: https://blog.jetbrains.com/research/2025/12/efficient-context-management/
- Claude context management guide: https://substratia.io/blog/context-management-guide/
- Anthropic Claude cookbooks compaction: https://deepwiki.com/anthropics/claude-cookbooks/5.3-context-management-and-compaction

---

## Change Log

- 2026-02-06: Initial research covering 7 agents, 6 academic papers, with design recommendations

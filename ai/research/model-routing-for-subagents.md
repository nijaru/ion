# Model Routing for Agent Sub-Tasks

**Date**: 2026-01-16
**Purpose**: Inform model selection strategy for different agent operations
**Status**: Research complete, ready for design decisions

---

## Executive Summary

| Task Type          | Recommended Model   | Rationale                                    |
| ------------------ | ------------------- | -------------------------------------------- |
| Exploration/Search | Small (Haiku-class) | Read-only, speed critical, pattern matching  |
| Research/Synthesis | Inherit from parent | Context continuity, reasoning required       |
| Code Review        | Full (Sonnet-class) | Security/quality needs reasoning depth       |
| Planning           | Full (Sonnet/Opus)  | Multi-step reasoning, architecture decisions |
| Tool Routing       | Embeddings (no LLM) | 94.5% accuracy, eliminates LLM call          |

**Key Finding**: NVIDIA research confirms SLMs are "sufficiently powerful, inherently more suitable, and necessarily more economical" for repetitive, specialized agent tasks.

---

## Evidence by Task Type

### 1. Exploration/Search Tasks

**What Claude Code Does**:

- **Explore subagent**: Uses **Haiku** (fast, low-latency)
- **Tools**: Read-only (denied Write/Edit)
- **Rationale**: "File discovery, code search, codebase exploration"

**GitHub Copilot's Approach**:

- **Embedding-guided tool routing**: 94.5% coverage (vs 87.5% LLM-based, 69% static)
- **No LLM call for routing**: Cosine similarity on tool embeddings
- **Result**: 400ms latency reduction, 2-5pp accuracy improvement

**Research Evidence**:

- NVIDIA (2025): "Most agent tasks involve narrow, repetitive work - classifying intents, extracting data, generating structured outputs"
- SLMs (<10B params) sufficient for: parsing commands, generating structured outputs, producing summaries
- Fine-tuned SLMs outperform generalist LLMs on specialized tasks

**Benchmark Data**:

- Code search (arXiv 2410.22240): Decoder-only LLMs struggle vs specialized embeddings
- Small models (0.4-10B) achieve comparable accuracy on structured tasks

**Recommendation**:

```
Search tasks -> Haiku-class OR embedding-only (no LLM)
Speed: <200ms first token
Cost: ~10x cheaper than Sonnet
```

### 2. Research/Synthesis Tasks

**What Claude Code Does**:

- **Plan subagent**: Inherits model from main conversation
- **Tools**: Read-only
- **Purpose**: "Codebase research for planning"

**Key Insight**: Research tasks need context continuity with the main conversation. Using a different model risks losing nuance in handoff.

**When to Use Smaller Models**:

- Web search result parsing: SLM can extract facts
- Documentation summarization: SLM sufficient
- Multi-source synthesis: Needs full model for reasoning

**Cursor's Approach** (Forum discussion):

```
Real-time research/search -> Gemini 2.5 Flash (fast)
Planning & Reasoning -> Gemini 2.5 Pro (capable)
Coding -> Claude 4 Sonnet
Write Test Cases -> Gemini 2.5 Pro
Debug -> o3 or Auto Mode
```

**Recommendation**:

```
Research gathering -> Haiku-class (fast extraction)
Research synthesis -> Inherit parent (reasoning required)
```

### 3. Code Review Tasks

**Benchmark Evidence** (Augment 2025):

- Evaluated 7 AI code review tools on public benchmark
- **Key finding**: "Reviews were both higher precision and substantially higher recall, driven by a uniquely powerful Context Engine"
- Full codebase reasoning required for meaningful review

**What Works**:

- Bug detection: Large models significantly better (arXiv 2601.10496)
- Security analysis: Requires reasoning across files
- Quality assessment: Needs architectural understanding

**What Doesn't Scale Down**:

- CodeFuse-CR-Bench (2025): Small models fail on "comprehensiveness-aware" review
- "Syntax Is Not Enough" (2025): CodeT5-small (60.5M) achieves 0% exact-match on real repairs despite 94% syntactic validity

**Recommendation**:

```
Code review -> Sonnet-class minimum
Security review -> Opus-class preferred
Quick lint checks -> Can use smaller model
```

### 4. Tool Routing (Meta-Task)

**GitHub Copilot's Solution**:

1. **Embedding-based routing**: Cosine similarity, no LLM
2. **Adaptive clustering**: Group related tools
3. **Virtual tools**: Expandable tool directories

**Performance**:
| Method | Tool Use Coverage | Latency Impact |
|--------|------------------|----------------|
| Default static | 69.0% | Baseline |
| LLM-based (GPT-4.1) | 87.5% | +round trip |
| Embedding-based | 94.5% | Minimal |

**OpenRouter Auto Router**:

- Powered by NotDiamond
- Analyzes: prompt complexity, task type, model capabilities
- Automatically routes to optimal model from pool

**Recommendation**:

```
Tool selection -> Embeddings (no LLM call)
Model routing -> Classifier or embeddings
Avoid: LLM-in-the-loop for routing decisions
```

---

## Speed vs Accuracy Trade-offs

### Benchmark Data (2025-2026)

| Model             | Tokens/sec | SWE-bench | Cost/M input |
| ----------------- | ---------- | --------- | ------------ |
| GPT-5.2           | 187        | ~78%      | $1.75        |
| Claude Opus 4.5   | ~50        | 80.9%     | $5.00        |
| Claude Sonnet 4.5 | ~100       | ~75%      | $3.00        |
| Gemini 3 Flash    | ~200       | ~70%      | $0.50        |
| DeepSeek V3.2     | ~150       | 73.1%     | $0.28        |
| Haiku 4.5         | ~300       | ~60%      | $0.25        |

**Key Insight** (NeurIPS 2025 - "Win Fast or Lose Slow"):

> "Optimal latency-quality balance varies by task, and sacrificing quality for lower latency can significantly enhance downstream performance"

**FPX Framework**: Dynamically selects model size + quantization based on real-time demands

### Cursor 2.0 Composer Model

**Positioning**: Mid-frontier (matches Haiku 4.5, Gemini Flash 2.5)

- **Speed**: 250 tokens/sec, tasks < 30 seconds
- **4x faster** than comparable intelligence models
- **Use case**: Quick iterations, routine edits, incremental changes

**Trade-off Strategy**:

```
Complex architecture -> Opus/GPT-5
Standard coding -> Sonnet
Quick iterations -> Composer/Flash
Exploration -> Haiku
```

---

## What Production Tools Actually Use

### Claude Code (Anthropic)

| Subagent | Model     | Tools     | Purpose              |
| -------- | --------- | --------- | -------------------- |
| Explore  | **Haiku** | Read-only | Fast codebase search |
| Plan     | Inherit   | Read-only | Planning research    |
| General  | Inherit   | All       | Complex multi-step   |

**Key Design**:

- Subagents cannot spawn subagents (no infinite nesting)
- Tool restrictions per subagent
- Model selection based on task characteristics

### GitHub Copilot

| Layer           | Technology        | Purpose                 |
| --------------- | ----------------- | ----------------------- |
| Tool routing    | **Embeddings**    | 94.5% coverage, no LLM  |
| Tool clustering | Cosine similarity | Group related tools     |
| Code completion | Codex variants    | Fast inline suggestions |
| Chat/Agent      | GPT-5/Claude      | Full reasoning          |

**Architecture Insight**:

- Reduced toolset from 40 to 13 core tools
- Virtual tool groups for MCP servers
- Context-aware routing via embeddings

### Cursor

| Task                | Model Selection                        |
| ------------------- | -------------------------------------- |
| Auto mode           | Dynamic routing based on task          |
| Tab completion      | Fast model (Composer)                  |
| Agent mode          | User-selected (Sonnet default)         |
| Background planning | Can use different model than execution |

**Key Feature**: "Background Planning Mode" - use one model to plan, different to execute

---

## Model Routing Research

### RouterEval Benchmark (EMNLP 2025)

**Finding**: "A capable router can significantly enhance performance as the number of candidates increases" (model-level scaling)

**Implication**: Better routing > using bigger model for everything

### Unified Routing + Cascading (ETH Zurich, ICLR 2025)

**Two Strategies**:

1. **Routing**: Single model chosen per query
2. **Cascading**: Sequential models until satisfactory answer

**Key Insight**: Combining both paradigms yields best cost-performance trade-off

### Capability Instruction Tuning (Nanjing, 2025)

**Approach**: Train router to match query capabilities to model strengths

**Result**: Smaller models outperform larger when properly routed

---

## Heterogeneous Agent Systems (NVIDIA Research)

### Core Thesis

> "Small language models are sufficiently powerful, inherently more suitable, and necessarily more economical for many invocations in agentic systems"

### Task Classification

| Task Type             | LLM Needed? | SLM Suitable?    |
| --------------------- | ----------- | ---------------- |
| Intent classification | No          | Yes              |
| Data extraction       | No          | Yes (fine-tuned) |
| Structured output     | No          | Yes              |
| Complex reasoning     | Yes         | No               |
| Novel problems        | Yes         | No               |
| Multi-step planning   | Yes         | Partial          |

### LLM-to-SLM Conversion Algorithm

1. Identify repetitive, low-variation tasks
2. Fine-tune SLM on task-specific data
3. Route complex/novel queries to LLM
4. Monitor and adjust routing thresholds

### Cost Impact

**Example Calculation** (from NVIDIA):

- 1M agent invocations/day
- 80% routine tasks (SLM-suitable)
- SLM: $0.28/M tokens vs LLM: $5.00/M tokens
- **Savings**: ~94% on routine tasks

---

## Recommendations for Aircher

### Immediate Implementation

1. **Explore Subagent**: Use Haiku for read-only file exploration
   - Matches Claude Code's architecture
   - 10x cost reduction
   - Speed improvement critical for UX

2. **Tool Routing**: Implement embedding-based routing
   - GitHub Copilot's 94.5% coverage proves viability
   - Eliminates LLM round-trip for tool selection
   - Use sentence-transformers or custom embeddings

3. **Model Configuration**: Allow per-subagent model selection
   ```yaml
   subagents:
     explore:
       model: haiku
       tools: [read, glob, grep]
     review:
       model: sonnet
       tools: [read, analyze]
     plan:
       model: inherit # from parent session
       tools: [read]
   ```

### Architecture Pattern

```
User Query
    |
    v
[Embedding Router] -- determines task type, no LLM
    |
    +-- Exploration --> Haiku (fast, cheap)
    |
    +-- Research --> Inherit (context continuity)
    |
    +-- Review --> Sonnet (reasoning required)
    |
    +-- Complex --> Opus (deep reasoning)
```

### OpenRouter Integration

Leverage `openrouter/auto` for automatic model selection:

- Powered by NotDiamond routing
- Handles task complexity analysis
- Automatic fallback on model failures

Alternative: Build custom router using:

- Task classifier (embedding-based)
- Cost/latency constraints
- Model capability matrix

### Validation Plan

1. **A/B Test**: Haiku vs Sonnet for exploration tasks
   - Metric: Task completion rate, latency, cost
   - Hypothesis: <5% accuracy loss, 10x cost reduction

2. **Embedding Router Eval**: Compare routing accuracy
   - Metric: Tool Use Coverage (target: >90%)
   - Baseline: Static tool list

3. **End-to-End Benchmark**: SWE-bench with heterogeneous models
   - Metric: Overall accuracy, total cost
   - Compare: Single model vs routed system

---

## Sources

### Research Papers

- [Small Language Models are the Future of Agentic AI](https://arxiv.org/abs/2506.02153) - NVIDIA, 2025
- [RouterEval: Benchmark for Routing LLMs](https://arxiv.org/abs/2503.10657) - EMNLP 2025
- [Unified Routing and Cascading for LLMs](https://arxiv.org/abs/2410.10347) - ETH Zurich, ICLR 2025
- [Win Fast or Lose Slow](https://arxiv.org/abs/2505.19481) - NeurIPS 2025

### Industry Sources

- [GitHub Copilot: Smarter with Fewer Tools](https://github.blog/ai-and-ml/github-copilot/how-were-making-github-copilot-smarter-with-fewer-tools/) - Nov 2025
- [Claude Code Subagents](https://docs.anthropic.com/en/docs/claude-code/sub-agents) - Anthropic
- [OpenRouter Auto Router](https://openrouter.ai/docs/guides/routing/routers/auto-router) - NotDiamond integration
- [NVIDIA: How SLMs are Key to Scalable Agentic AI](https://developer.nvidia.com/blog/how-small-language-models-are-key-to-scalable-agentic-ai/) - Aug 2025

### Benchmarks

- [Augment Code Review Benchmark](https://www.augmentcode.com/blog/we-benchmarked-7-ai-code-review-tools-on-real-world-prs-here-are-the-results) - Dec 2025
- [LLM Latency Benchmark by Use Cases](https://research.aimultiple.com/llm-latency-benchmark/) - AIMultiple, 2026
- [LLM Comparison Guide](https://www.digitalapplied.com/blog/llm-comparison-guide-december-2025) - Dec 2025

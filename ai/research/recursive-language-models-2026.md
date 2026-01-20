# Recursive Language Models (RLMs) Research Summary

**Date**: 2026-01-13
**Purpose**: Research on Recursive Language Models for Aircher integration

## Core Paper

**Title**: Recursive Language Models
**Authors**: Alex L. Zhang, Tim Kraska, Omar Khattab (MIT OASYS)
**arXiv**: 2512.24601v1 (Dec 31, 2025)
**Code**: [github.com/alexzhang13/rlm](https://github.com/alexzhang13/rlm) (MIT license, 963 stars)

---

## What is an RLM?

**Definition**: Thin wrapper around LM that treats long context as an external environment, allowing LM to programmatically examine, decompose, and recursively call itself over snippets.

**Key Difference**:
- Standard: `lm.completion(prompt, model)` - All context in one call
- RLM: `rlm.completion(prompt, model)` - Context as variable, recursive calls

## Architecture

```
User Query + Huge Context (10M tokens)
    │
    ▼
┌─────────────────────────────────┐
│  Root LM (depth=0)         │
│  - Has access to REPL env   │
│  - Context stored as variable  │
└─────────────────────────────────┘
    │
    │ (decides how to process)
    ▼
┌─────────────────────────────────┐
│  Recursive Strategies          │
│  1. Peek: Read first N chars│
│  2. Grep: Search patterns    │
│  3. Partition: Chunk and map │
│  4. Summarize: Condense   │
└─────────────────────────────────┘
    │
    │ (launches sub-LM calls)
    ▼
┌─────────────────────────────────┐
│  Recursive LM (depth=1)      │
│  - Isolated environment     │
│  - Processes subset        │
│  - Returns summary          │
└─────────────────────────────────┘
```

## Key Results

### OOLONG Benchmark (Long-context reasoning)

| Model | Performance | Cost | Notes |
| ------ | ----------- | ----- | ------ |
| GPT-5 (direct) | Baseline | Full context | Suffers context rot |
| GPT-5-mini (direct) | Lower | Full context | Severe degradation |
| RLM(GPT-5-mini) | **+34% over GPT-5** | Similar to GPT-5 | Recursive calls avoid rot |

**Finding**: Recursive LM (mini) outperforms full model by **double** (114% increase).

### BrowseComp-Plus (Deep research)

| Documents | GPT-5 | RLM(GPT-5) |
| --------- | ------ | ------------ |
| 10 | 100% | 100% |
| 50 | 95% | 100% |
| 100 | 85% | 100% |
| 1000 | 60% | **100%** |

**Finding**: RLM maintains performance at 1000 documents, base model degrades.

### Scalability

- **Handles**: 10M+ tokens without degradation
- **Cost**: Recursive overhead ~similar to base model
- **Speed**: Not optimized (blocking calls, no prefix caching)

---

## Emergent Strategies

The paper visualized RLM trajectories, showing self-emergent patterns:

| Strategy | When Used | Example |
| -------- | ---------- | ------- |
| Peek | Initial analysis | Read first 2000 chars to understand structure |
| Grep | Narrow search space | Find lines with specific IDs/keywords |
| Partition + Map | Semantic extraction | Chunk context, label each with sub-LM |
| Summarize | Reduce information | Summarize subsets for root LM |
| One-shot programmatic | Long input/output | Parse git diff, track changes algorithmically |

**Key Insight**: LM learns to interact with its own context, not just process it.

---

## Relationship to Prior Work

| Approach | Focus | Limitation | RLM Advantage |
| --------- | ------ | ----------- | -------------- |
| MemGPT | Managed context | Single memory stream | Environment + recursion |
| ReAct + Retrieval | Task decomposition | Fixed workflow patterns | LM decides decomposition |
| MemWalker | Tree-based summarization | Rigid structure | Flexible REPL interaction |

**Quote from paper**: "RLMs are not agents, nor are they just summarization. Agents decompose from the perspective of a task or problem. RLMs decompose from the perspective of the context."

---

## Critique-Guided Improvement (Related)

**Paper**: "The Lighthouse of Language: Enhancing LLM Agents via Critique-Guided Improvement" (Mar 2025)

**Two-Player Framework**:
```
Actor generates work → Critic reviews → Actor refines → [iterate]
```

**Relevance**: Different from RLM (context vs review), but same pattern of iterative improvement.

---

## Aircher Integration Strategy

### Option A: RLM-Style Context Management

**Use**: For huge file/codebase explorations (10M+ tokens)

**Integration**:
```rust
// src/memory/rlm.rs

pub struct RlmContextManager {
    root_lm: Arc<dyn Provider>,
    environment: ReplEnvironment,
}

impl RlmContextManager {
    pub async fn query(&self, prompt: &str, context: &str) -> Result<String> {
        // Context stored as variable in REPL
        // Root LM decides decomposition
        // Recursive calls via provider
    }
}
```

**When to Use**:
- Deep research across entire codebase
- Analyzing 100+ files for architecture
- Complex git history analysis

### Option B: Conversational Review Loop

**Use**: Iterative refinement of subagent work

**Integration**:
```rust
// src/agent/review.rs

pub struct ReviewLoop {
    main: Arc<dyn Provider>,
    subagent: Arc<dyn Provider>,
    max_iterations: u32,
}

impl ReviewLoop {
    pub async fn refine(&self, task: &str) -> Result<String> {
        let mut work = self.main.generate(task).await?;

        for _ in 0..self.max_iterations {
            let feedback = self.subagent.critique(&work).await?;
            if feedback.is_acceptable() {
                return Ok(work);
            }
            work = self.main.refine(&work, &feedback).await?;
        }
        Ok(work)
    }
}
```

**When to Use**:
- Code generation requiring multiple passes
- Complex refactoring tasks
- Research synthesis

---

## Comparison: RLM vs Review Loop

| Aspect | RLM | Review Loop |
| ------- | --- | ----------- |
| Primary Goal | Process huge context | Improve through feedback |
| Decomposition | LM decides (context-centric) | Fixed pattern (actor/critic) |
| Use Case | 10M+ token contexts | 1-5K token work |
| Complexity | Medium (REPL env) | Medium (loop management) |
| Cost | Multiple sub-LM calls | Multiple iterations |
| State of Art | Yes (Jan 2026) | Yes (CGI, Mar 2025) |

---

## Recommendation

**Hybrid Approach**:
1. Use **RLM-style** for context management (Phase 3.5)
2. Use **Review Loop** for subagent refinement (Phase 4.5)
3. Learn from both patterns for agent optimization (Phase 5.5)

**Why**:
- RLM solves context rot → Good for memory retrieval
- Review loop solves quality drift → Good for code generation
- Both are state-of-art → Strong differentiators

---

## References

- [RLM arXiv Paper](https://arxiv.org/abs/2512.24601)
- [RLM Blog](https://alexzhang13.github.io/blog/2025/rlm/)
- [RLM GitHub](https://github.com/alexzhang13/rlm)
- [CGI Paper](https://arxiv.org/pdf/2503.16024)
- [OOLONG Benchmark](https://arxiv.org/abs/2506.17521)
- [BrowseComp-Plus](https://arxiv.org/abs/2508.06600)

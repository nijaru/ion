# DeepSeek Research Summary (Jan 2026)

**Date**: 2026-01-13
**Purpose**: Key findings from DeepSeek's latest papers relevant to Aircher

## Papers Covered

| Paper | Date | Focus | Relevance |
| ------ | ------ | ------- | ---------- |
| mHC: Manifold-Constrained Hyper-Connections | Dec 31, 2025 | Architecture stability | Indirect (model performance) |
| DeepSeek-R1: Incentivizing Reasoning via RL | Jan 22, 2025 | Reasoning capabilities | Indirect (provider choice) |
| DeepAgent: General Reasoning Agent | Oct 24, 2025 | Agent architecture | Direct (patterns) |

---

## mHC: Manifold-Constrained Hyper-Connections

### Problem Solved

**Context**: Hyper-Connections (ByteDance, 2025) widened residual streams for more expressive power but caused training instability.

**Issue**: Unconstrained residual mixing matrices caused signal explosion/vanishing during deep training, breaking the identity guarantee that made residual connections work.

### Solution: Manifold-Constrained Hyper-Connections

Two constraints on residual mixing matrix:

1. **Non-negative entries** - No signal cancellation
2. **Row/column sums to one** - Doubly stochastic matrix (Sinkhorn-Knopp algorithm)

**Result**: Preserves identity-like behavior globally while maintaining local flexibility.

### Key Results

| Model | MMLU | MATH | Code | BBH |
| ------ | ---- | ---- | ---- | ---- |
| Baseline (no HC) | 71.3 | 57.9 | 79.4 | 79.2 |
| Hyper-Connections (HC) | 72.8 | 60.4 | 80.8 | 81.1 |
| **mHC** | **73.7** | **62.3** | **81.9** | **82.6** |

- **Overhead**: 6-7% FLOPs increase
- **Stability**: Gradient norms follow baseline, HC diverges
- **Scales**: Tested at 3B, 9B, 27B parameters

### Relevance to Aircher

**Impact**: If DeepSeek V4 uses mHC architecture (likely), it's a stronger model for our primary provider.

**Action**: None needed - just better DeepSeek models available via OpenRouter.

---

## DeepSeek-R1: Reasoning via RL

### Architecture

- **DeepSeek-R1-Zero**: Self-play RL, no cold-start data
- **Main pipeline**: Small cold-start data + multi-stage training
- **Distillation**: Smaller models (DeepSeek-R1-Distill-Q, -K, -Lite)

### Key Innovations

1. **Self-reflection** - Model critiques its own outputs
2. **"Aha moments"** - Emergent reasoning strategies
3. **Multi-stage training** - Cold-start → RL → distillation

### Performance

Rivals top-tier closed-source models on:
- Mathematics (AIME 2024, MATH)
- Coding (Codeforces, HumanEval)
- STEM reasoning

### Relevance to Aircher

**Action**: Already using DeepSeek V3.2/V4 as primary models.

**Connection**: Self-reflection pattern aligns with our planned conversational review subagents.

---

## DeepAgent: General Reasoning Agent

### Architecture

```
DeepAgent = Reasoning Core + Scalable Toolsets
```

**Key Features**:

1. **Multi-turn reasoning** - Iterative tool use
2. **Tool abstractions** - Unified interface for tools
3. **Context management** - Explicit state tracking
4. **Hierarchical planning** - Task decomposition

### Design Patterns

| Pattern | Description | Aircher Alignment |
| -------- | ----------- | ------------------- |
| Tool Registry | Centralized tool discovery | Same (ToolRegistry) |
| Reasoning Loop | Multi-step until completion | Same (AgentLoop) |
| State Tracking | Explicit context object | Same (Session) |
| Tool Abstraction | Unified tool trait | Same (Tool trait) |

### Relevance to Aircher

**Confirmation**: Our architecture decisions are validated by DeepSeek's agent patterns.

---

## Summary

| Finding | Action |
| ------- | ------ |
| DeepSeek models are improving rapidly | Use DeepSeek V4 as primary when available |
| mHC architecture stabilizes larger models | Expect better DeepSeek models soon |
| Self-reflection is proven (DeepSeek-R1) | Conversational review subagents are well-founded |
| DeepAgent confirms our patterns | No architecture changes needed |
| Tool abstraction is standard | Our Tool trait is correct |

---

## References

- [mHC Paper](https://arxiv.org/abs/2512.24880)
- [DeepSeek-R1 Paper](https://arxiv.org/abs/2501.12948)
- [DeepAgent Paper](https://arxiv.org/abs/2510.21618)
- [DeepSeek mHC Blog](https://deepseek.ai/blog/deepseek-mhc-manifold-constrained-hyper-connections)

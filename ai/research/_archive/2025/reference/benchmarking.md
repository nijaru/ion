# Benchmark Integration Plan

**Purpose**: Establish empirical performance baselines for Aircher against industry-standard agent benchmarks

**Updated**: 2025-10-29

## Overview

To validate Aircher's SOTA claims and competitive positioning, we must run standardized benchmarks that measure agent performance objectively. This document outlines the integration plan for Terminal-Bench and SWE-bench.

## Available Benchmarks

### 1. Terminal-Bench (Primary Target)
**Why**: Terminal-specific agent benchmark with current leaderboard

**Source**: https://www.tbench.ai/
**Repository**: https://github.com/BlocBlocX/terminal-bench

**Current SOTA**:
- **Ante** (Antigma Labs): 60.3% ± 1.1 (Claude Sonnet 4.5)
- **Droid** (Factory): 58.8% ± 0.9 (Claude Opus 4.1) ← **TARGET TO BEAT**
- **Claude Code**: 43.2% ± 1.3 (Claude Opus 4)

**Dataset**:
- **T-Bench-Core-v0**: 80 core terminal tasks
- Covers: file operations, code editing, git workflows, debugging, refactoring

**Why This Matters**:
- Aircher is terminal/ACP-focused, this benchmark aligns with our use case
- Beating Factory Droid's 58.8% would establish SOTA status
- Published leaderboard provides credibility

### 2. SWE-bench (Industry Standard)
**Why**: Most widely recognized coding agent benchmark

**Source**: https://www.swebench.com/
**Repository**: https://github.com/princeton-nlp/SWE-bench

**Current SOTA**:
- **Grok 4**: 75%
- **GPT-5**: 74.9%
- **Claude Opus 4.1**: 74.5%

**Datasets** (choose based on resources):
1. **SWE-bench Verified** (500 tasks) ← **RECOMMENDED START**
   - Human-filtered quality instances
   - Balanced difficulty
   - Faster to run than Full

2. **SWE-bench Lite** (300 tasks)
   - Cost-effective subset
   - Good for rapid iteration

3. **SWE-bench Full** (2,294 tasks)
   - Comprehensive evaluation
   - Resource-intensive

4. **SWE-bench Multimodal** (517 tasks)
   - Visual elements (screenshots, UI)
   - Future consideration if we add vision

5. **SWE-bench-Live** (1,565 tasks, monthly updates)
   - Real-world freshness
   - 164 repositories

**Why This Matters**:
- Industry-standard benchmark (most cited in papers)
- Matching 75% SOTA would be significant achievement
- Large dataset provides statistical significance

### 3. Terminal-Bench Registry (Bonus)
**Source**: Terminal-Bench provides unified harness for multiple benchmarks

**Available via Registry**:
- SWE-bench Verified (500 tasks)
- AppWorld (domain-specific tasks)
- DevEval (development workflows)
- EvoEval (code evolution tasks)

**Advantage**: Single CLI interface for multiple benchmarks

## Integration Architecture

### High-Level Flow
```
Terminal-Bench/SWE-bench Test Harness
    ↓ (JSON-RPC or CLI invocation)
Aircher Agent (ACP-native)
    ↓
Agent Core (with memory systems)
    ↓
Tool Execution (read, write, edit, bash, etc.)
    ↓
Results returned to harness
    ↓
Benchmark scoring
```

### Integration Points

**Option 1: ACP Integration** (Recommended)
- Terminal-Bench/SWE-bench act as ACP client
- Invoke Aircher via stdio JSON-RPC
- Minimal changes to Aircher (already ACP-native)
- Advantage: Tests production ACP interface

**Option 2: Direct API Integration**
- Benchmark harness calls Aircher Rust API directly
- Requires creating benchmark-specific entry point
- Advantage: Faster execution (no JSON-RPC overhead)
- Disadvantage: Not testing production interface

**Recommendation**: Start with Option 1 (ACP) since it validates our production interface

## Implementation Steps

### Phase 1: Terminal-Bench Integration (Week 9 Days 1-3)

**Day 1: Setup**
1. Install Terminal-Bench CLI:
   ```bash
   # From Terminal-Bench repository
   git clone https://github.com/BlocBlocX/terminal-bench
   cd terminal-bench
   npm install  # or equivalent install command
   ```

2. Configure Aircher as agent backend:
   ```bash
   # Register Aircher in Terminal-Bench config
   tbench register-agent \
     --name aircher \
     --command "cargo run --release -- --acp" \
     --protocol acp
   ```

3. Verify connection:
   ```bash
   tbench test-connection --agent aircher
   ```

**Day 2: Baseline Run**
1. Run subset of tasks (e.g., 10 tasks) to verify integration:
   ```bash
   tbench run --agent aircher --tasks 10 --dataset core-v0
   ```

2. Monitor execution:
   - Check logs for errors
   - Verify tool calls working correctly
   - Ensure ACP communication stable

3. Debug issues:
   - Fix any protocol mismatches
   - Resolve tool execution failures
   - Handle edge cases

**Day 3: Full Evaluation**
1. Run full T-Bench-Core-v0 (80 tasks):
   ```bash
   tbench run --agent aircher --dataset core-v0 --output results/aircher-baseline.json
   ```

2. Collect metrics:
   - Overall success rate (target: >43.2% to beat Claude Code)
   - Per-task performance
   - Tool usage statistics
   - Token consumption
   - Execution time per task

3. Generate report:
   ```bash
   tbench report --input results/aircher-baseline.json --format markdown
   ```

### Phase 2: SWE-bench Integration (Week 9 Days 4-5)

**Day 4: Setup**
1. Install SWE-bench:
   ```bash
   git clone https://github.com/princeton-nlp/SWE-bench
   cd SWE-bench
   pip install -e .
   ```

2. Download SWE-bench Verified dataset (500 tasks):
   ```bash
   python -m swebench.harness.download_data --dataset verified
   ```

3. Configure Aircher adapter:
   - Create adapter script that invokes Aircher via ACP
   - Map SWE-bench task format to Aircher prompts
   - Handle repository setup/teardown

**Day 5: Baseline Run**
1. Run small subset (e.g., 20 tasks):
   ```bash
   python -m swebench.harness.run_evaluation \
     --agent aircher \
     --dataset verified \
     --max_workers 4 \
     --num_tasks 20 \
     --output results/aircher-swebench-sample.json
   ```

2. Analyze results:
   - Success rate
   - Common failure modes
   - Tool usage patterns
   - Context management effectiveness

3. Identify improvements:
   - Which tools need enhancement
   - Where memory helps (or doesn't)
   - LSP integration impact

### Phase 3: Analysis & Optimization (Week 9 Days 6-7)

**Day 6: Results Analysis**
1. Compare Aircher vs baselines:
   ```markdown
   | Metric                    | Aircher | Claude Code | Factory Droid | Target  |
   |---------------------------|---------|-------------|---------------|---------|
   | Terminal-Bench (%)        | ?       | 43.2        | 58.8          | >58.8   |
   | SWE-bench Verified (%)    | ?       | ~30*        | ~45*          | >50     |
   | Avg tools per task        | ?       | ~10         | ~6            | <6      |
   | Context efficiency        | ?       | Moderate    | High          | High    |
   | Token usage (relative)    | ?       | 1.4x        | 1.0x          | <1.2x   |

   *Estimated from public data
   ```

2. Identify strengths:
   - Where Aircher outperforms (file operations? code analysis?)
   - Where memory systems provide advantage
   - Which tasks benefit from LSP integration

3. Identify weaknesses:
   - Task types with low success rate
   - Missing tools or capabilities
   - Integration issues

**Day 7: Create Improvement Plan**
1. Prioritize fixes:
   - High-impact, low-effort improvements first
   - Tool enhancements needed
   - Memory system tuning

2. Document findings:
   - Create `ai/research/benchmark-results-baseline.md`
   - Include detailed failure analysis
   - Propose optimizations

3. Update roadmap:
   - Adjust Week 10 plan based on results
   - Set realistic targets for paper

## Metrics to Track

### Success Metrics
- **Overall Pass Rate**: % of tasks completed successfully
- **Tool Call Efficiency**: Average tools per task (target: <6)
- **Context Efficiency**: % of context utilized effectively
- **Token Usage**: Total tokens per task (compare vs Claude Code baseline)
- **Execution Time**: Average time per task

### Quality Metrics
- **Code Quality**: Does generated code match project patterns?
- **Error Recovery**: % of tasks recovered from errors
- **Memory Hit Rate**: % of queries answered from memory (vs re-searching)
- **LSP Self-Correction**: % of errors caught by LSP before execution

### Breakdown Metrics
- **By Task Type**: File ops, debugging, refactoring, feature implementation
- **By Language**: Rust, Python, TypeScript, etc.
- **By Complexity**: Simple, medium, complex tasks

## Expected Baseline Performance

### Conservative Estimates (Before Optimization)
- **Terminal-Bench**: 35-45% (between Claude Code's 43.2% and Factory Droid's 58.8%)
  - Rationale: Core architecture implemented, memory not fully tuned
- **SWE-bench Verified**: 25-35%
  - Rationale: General coding tasks, less focus on terminal workflows

### Optimistic Targets (After Week 9-10 Optimization)
- **Terminal-Bench**: 50-60% (competitive with Factory Droid)
  - Path: Memory optimization, tool refinements, LSP tuning
- **SWE-bench Verified**: 40-50%
  - Path: Better code generation prompts, pattern learning

### SOTA Targets (Stretch Goals)
- **Terminal-Bench**: >60% (beat current SOTA)
  - Would require: All systems working optimally + novel advantages
- **SWE-bench Verified**: >75% (match Grok 4)
  - Would require: Significant breakthroughs (unlikely in 10-week timeline)

## Benchmark Data Storage

### Directory Structure
```
aircher/
├── benchmarks/
│   ├── terminal-bench/
│   │   ├── results/
│   │   │   ├── baseline-2025-11-01.json
│   │   │   ├── optimized-2025-11-08.json
│   │   │   └── final-2025-11-15.json
│   │   ├── analysis/
│   │   │   ├── failure-modes.md
│   │   │   ├── tool-usage.md
│   │   │   └── memory-impact.md
│   │   └── configs/
│   │       └── aircher-tbench.yaml
│   ├── swebench/
│   │   ├── results/
│   │   │   ├── verified-baseline.json
│   │   │   └── verified-optimized.json
│   │   └── analysis/
│   │       ├── task-breakdown.md
│   │       └── comparison-claude-code.md
│   └── reports/
│       ├── week9-baseline.md
│       └── week10-final.md
```

### Results Format
```json
{
  "benchmark": "terminal-bench-core-v0",
  "agent": "aircher",
  "version": "0.1.0-week9",
  "timestamp": "2025-11-01T00:00:00Z",
  "configuration": {
    "model": "claude-opus-4.1",
    "memory_enabled": true,
    "lsp_enabled": true,
    "max_steps": 50
  },
  "results": {
    "total_tasks": 80,
    "passed": 38,
    "failed": 42,
    "pass_rate": 0.475,
    "avg_tools_per_task": 5.2,
    "avg_tokens_per_task": 8500,
    "avg_time_per_task_seconds": 45
  },
  "by_category": {
    "file_operations": {"passed": 12, "total": 15, "rate": 0.80},
    "debugging": {"passed": 8, "total": 20, "rate": 0.40},
    "refactoring": {"passed": 10, "total": 25, "rate": 0.40},
    "feature_implementation": {"passed": 8, "total": 20, "rate": 0.40}
  },
  "failures": [
    {
      "task_id": "tbench-015",
      "category": "debugging",
      "error": "Failed to identify root cause",
      "tools_used": ["read_file", "search_code", "analyze_code"],
      "context_tokens": 12000
    }
  ]
}
```

## Integration with Research Paper

### Data to Include in Paper

**Section 4: Evaluation**
- Benchmark setup and configuration
- Dataset descriptions (Terminal-Bench Core-v0, SWE-bench Verified)
- Evaluation metrics

**Section 5: Results**
- Performance comparison table:
  ```markdown
  | System        | Terminal-Bench | SWE-bench | Avg Tools | Memory |
  |---------------|----------------|-----------|-----------|--------|
  | Aircher       | X.X%          | Y.Y%      | Z.Z       | Yes    |
  | Factory Droid | 58.8%         | ~45%*     | ~6        | No     |
  | Claude Code   | 43.2%         | ~30%*     | ~10       | No     |

  *Estimated from available data
  ```

- Ablation study (with/without memory, with/without LSP)
- Failure mode analysis

**Section 6: Discussion**
- Where memory helps most (file discovery? pattern recognition?)
- Where LSP prevents errors (type errors? syntax mistakes?)
- Task types where Aircher excels vs struggles
- Comparison to baselines

### Figures and Tables
1. **Figure 1**: Pass rate by task category (bar chart)
2. **Figure 2**: Tool usage distribution (histogram)
3. **Figure 3**: Memory cache hit rate over time (line graph)
4. **Table 1**: Overall benchmark comparison
5. **Table 2**: Ablation study results

## Risk Mitigation

### Risk: Integration Issues with Benchmark Harnesses
- **Mitigation**: Start with small task subsets (10-20 tasks)
- **Fallback**: Manual task execution if automated harness fails

### Risk: Poor Initial Performance (<30%)
- **Mitigation**: Focus on highest-impact improvements first
- **Acceptance**: Document honestly, position as "early results, optimization ongoing"

### Risk: Benchmark Harness Incompatibility
- **Mitigation**: Create adapter layer that maps benchmark format to Aircher ACP
- **Fallback**: Run tasks manually and report results with transparency

### Risk: Resource Constraints (API costs, compute time)
- **Mitigation**:
  - Start with Verified/Lite datasets (smaller)
  - Use cheaper models for baseline (Haiku vs Opus)
  - Run in phases (subset → full)
- **Budget**: Estimate $500-1000 for full evaluation runs

## Timeline Summary

| Week | Phase | Tasks | Deliverable |
|------|-------|-------|-------------|
| Week 9 Day 1-3 | Terminal-Bench | Setup, baseline run, analysis | Terminal-Bench score |
| Week 9 Day 4-5 | SWE-bench | Setup, sample run, analysis | SWE-bench baseline |
| Week 9 Day 6-7 | Analysis | Compare, identify gaps, plan fixes | Improvement roadmap |
| Week 10 Day 1-3 | Optimization | Implement top fixes, re-run | Final scores |
| Week 10 Day 4-7 | Paper | Write results section, create figures | Empirical validation |

## Success Criteria

### Minimum Viable Results (for paper publication)
- ✅ Terminal-Bench score >43.2% (beat Claude Code)
- ✅ SWE-bench Verified score >25% (reasonable baseline)
- ✅ Evidence that memory reduces tool calls (ablation study)
- ✅ Honest documentation of failures and limitations

### Competitive Results (strong positioning)
- ✅ Terminal-Bench score >50% (approach Factory Droid)
- ✅ SWE-bench Verified score >35%
- ✅ Clear advantage in specific task categories (file ops, code search)
- ✅ Memory provides measurable benefits (>20% tool reduction)

### SOTA Results (stretch goal)
- ✅ Terminal-Bench score >58.8% (beat Factory Droid)
- ✅ SWE-bench Verified score >50%
- ✅ Novel contributions validated (memory, LSP, hybrid architecture)
- ✅ Multiple task categories where Aircher is best-in-class

## Next Steps

1. **Week 9 Day 1** (Immediately after Week 8 complete):
   - Install Terminal-Bench CLI
   - Register Aircher as agent
   - Run first 10 tasks

2. **Week 9 Day 2**:
   - Debug integration issues from Day 1
   - Run full Terminal-Bench Core-v0 (80 tasks)
   - Generate initial report

3. **Week 9 Day 3**:
   - Analyze Terminal-Bench results
   - Identify quick wins for improvement
   - Begin SWE-bench setup

4. **Week 9 Day 4-5**:
   - Run SWE-bench Verified subset (50-100 tasks)
   - Compare results across benchmarks
   - Prioritize optimizations

5. **Week 9 Day 6-7**:
   - Implement high-impact fixes
   - Re-run failing tasks
   - Document findings

**Goal**: By end of Week 9, have empirical data to support (or refine) competitive claims in research paper.

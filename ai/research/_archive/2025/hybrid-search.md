# Hybrid Search Architecture Design

## Problem Statement

**Current State**: Separate search paths - ripgrep (regex) OR ChromaDB (semantic)
- No integration between fast regex and semantic search
- Unknown if vector search provides value for smaller models (not validated empirically)
- Changes require full benchmark validation (slow iteration cycle)

**Goal**: Implement validated hybrid search that reduces testing cycles through:
1. Clear fallback conditions (no ambiguity)
2. Feature flags for A/B testing
3. Metrics collection for empirical validation

## Design Principles (from Claude Code research)

1. **Regex-first**: Fast text search as primary path (Claude Code pattern)
2. **Semantic fallback**: Vector search only when regex insufficient
3. **Empirical validation**: Measure actual benefit before committing
4. **Incremental adoption**: Feature flags allow safe rollout

## Architecture

### 4-Layer Hybrid Search

```python
# Layer 1: Fast text search (ripgrep) - PRIMARY
candidates = ripgrep_search(query, max_results=100)

# Layer 2: Semantic filtering (ChromaDB) - FALLBACK
if should_use_semantic(candidates, query):
    candidates = chromadb_rerank(candidates, query, top_k=20)

# Layer 3: Structural context (Knowledge Graph) - ENHANCEMENT
if needs_structure_info:
    context = knowledge_graph_expand(candidates)

# Layer 4: Type information (LSP) - ENHANCEMENT
if needs_type_info:
    context = lsp_enhance(context)
```

### Fallback Conditions

**Use semantic search when**:
```python
def should_use_semantic(candidates: list, query: str) -> bool:
    # Too many results (ambiguous)
    if len(candidates) > 50:
        return True

    # Too few results (may have missed semantic matches)
    if len(candidates) < 5:
        return True

    # Natural language query (not regex pattern)
    if is_natural_language(query):
        return True

    # Explicit semantic query markers
    if query.startswith("semantic:") or query.startswith("find similar:"):
        return True

    return False
```

## Implementation Plan

### Phase 1: Feature Flags + Metrics (1h)

**Goal**: Enable A/B testing without code changes

```python
# config/flags.py
class SearchFeatureFlags:
    # Global flag to enable/disable hybrid search
    HYBRID_SEARCH_ENABLED: bool = False  # Default: off

    # Fallback thresholds
    MAX_RESULTS_BEFORE_SEMANTIC: int = 50
    MIN_RESULTS_BEFORE_SEMANTIC: int = 5

    # A/B testing
    FORCE_REGEX_ONLY: bool = False  # For baseline comparison
    FORCE_SEMANTIC_ONLY: bool = False  # For semantic-only comparison
```

**Metrics to collect**:
```python
class SearchMetrics:
    query: str
    regex_results_count: int
    semantic_used: bool
    semantic_results_count: int
    total_time_ms: float
    regex_time_ms: float
    semantic_time_ms: float | None
    final_results_count: int
```

### Phase 2: Unified Search Tool (2h)

**Goal**: Single entry point with automatic fallback

```python
# tools/search.py
class SearchTool(BaseTool):
    def __init__(self, bash_tool, memory_integration, flags: SearchFeatureFlags):
        self.regex_search = SearchFilesTool(bash_tool)
        self.semantic_search = memory_integration.search_similar_code
        self.flags = flags
        self.metrics = []

    async def execute(self, query: str, **kwargs) -> ToolOutput:
        start_time = time.time()

        # Phase 1: Regex search (always run)
        regex_start = time.time()
        regex_results = await self.regex_search.execute(
            pattern=query,
            max_results=self.flags.MAX_RESULTS_BEFORE_SEMANTIC,
            **kwargs
        )
        regex_time = time.time() - regex_start

        # Phase 2: Semantic fallback (conditional)
        semantic_results = None
        semantic_time = None

        if self.flags.HYBRID_SEARCH_ENABLED and \
           self.should_use_semantic(regex_results, query):
            semantic_start = time.time()
            semantic_results = self.semantic_search(
                query=query,
                n_results=20,
                **kwargs
            )
            semantic_time = time.time() - semantic_start

        # Combine results
        final_results = self.merge_results(regex_results, semantic_results)

        # Record metrics
        self.record_metrics(
            query=query,
            regex_results=len(regex_results.data),
            semantic_used=semantic_results is not None,
            regex_time=regex_time,
            semantic_time=semantic_time,
            total_time=time.time() - start_time,
        )

        return ToolOutput(
            success=True,
            data=final_results,
            metadata={
                "regex_count": len(regex_results.data),
                "semantic_used": semantic_results is not None,
                "total_time_ms": (time.time() - start_time) * 1000,
            }
        )
```

### Phase 3: Validation (1h benchmark run)

**A/B Test Plan**:

1. **Baseline (regex-only)**:
   ```bash
   FORCE_REGEX_ONLY=true ./scripts/quick-validation.sh hybrid-baseline
   ```

2. **Hybrid**:
   ```bash
   HYBRID_SEARCH_ENABLED=true ./scripts/quick-validation.sh hybrid-enabled
   ```

3. **Semantic-only**:
   ```bash
   FORCE_SEMANTIC_ONLY=true ./scripts/quick-validation.sh semantic-only
   ```

**Success Criteria**:
- Hybrid accuracy ≥ Regex-only accuracy (no regression)
- If hybrid > regex-only by ≥5pp → keep semantic search
- If hybrid ≈ regex-only (< 5pp) → disable semantic search by default

**Metrics to compare**:
```python
{
    "accuracy": 0.5,  # Task success rate
    "avg_search_time_ms": 120.5,  # Performance
    "semantic_fallback_rate": 0.23,  # How often semantic triggered
    "tokens_used": 85000,  # Cost
}
```

## Integration Points

### Agent Tool List Update

```python
# agent/__init__.py
from aircher.tools.search import SearchTool

# Replace SearchFilesTool with SearchTool
self.tools = [
    SearchTool(
        bash_tool=bash_tool,
        memory_integration=memory,
        flags=search_flags,
    ),
    ReadFileTool(),
    WriteFileTool(),
    BashTool(),
]
```

### Configuration

```python
# config/search.py
@dataclass
class SearchConfig:
    # Feature flags
    hybrid_enabled: bool = False  # Default: off until validated

    # Fallback thresholds
    max_results_before_semantic: int = 50
    min_results_before_semantic: int = 5

    # Performance limits
    regex_timeout_ms: int = 5000
    semantic_timeout_ms: int = 10000

    # A/B testing overrides
    force_regex_only: bool = False
    force_semantic_only: bool = False
```

## Expected Impact

**Development Velocity**:
- ✅ Single benchmark run validates entire feature (not incremental changes)
- ✅ Feature flags enable quick rollback without code changes
- ✅ Metrics guide decisions (empirical, not guesswork)

**Performance**:
- **Best case** (semantic helps): +5-10pp accuracy improvement
- **Worst case** (semantic neutral): 0pp change, disable by default
- **Risk mitigation**: Feature flags prevent regression

**Token Savings**:
- Regex-first reduces unnecessary semantic queries
- Only fallback when needed (estimated 20-30% of queries)
- Expected: 10-15% token reduction vs always-semantic

## Rollout Plan

1. **Week 1**: Implement feature flags + metrics collection (1h)
2. **Week 1**: Implement SearchTool (2h)
3. **Week 1**: Run A/B validation (1h benchmark)
4. **Week 1**: Analyze metrics, decide on default (30min)
5. **Week 2**: Default to winner, keep flags for future tuning

## Success Metrics

**Primary**: Accuracy improvement ≥ 5pp OR no regression with better performance
**Secondary**: Reduced testing cycles (1 validation vs 3+ incremental tests)
**Tertiary**: Clear decision framework (enable/disable based on data)

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Semantic search adds latency | Timeout limits + async execution |
| Accuracy regression | Feature flags allow instant rollback |
| Integration complexity | Single tool interface, backward compatible |
| Unclear benefit | A/B testing with clear success criteria |

## References

- ai/research/claude-code-architecture.md (lines 44-91) - Regex-first rationale
- ai/DECISIONS.md (2025-11-20) - Hybrid search architecture decision
- src/aircher/tools/file_ops.py:303 - Current SearchFilesTool
- src/aircher/memory/integration.py:158 - Current semantic search

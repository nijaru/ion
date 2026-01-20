# Codebase Intelligence: Local Indexing vs Cloud Services

**Research Date**: 2025-11-20
**Question**: How to optimize codebase knowledge without Sourcegraph's managed infrastructure?

---

## TL;DR Recommendations

**For Aircher** (multi-provider, local-first):

1. **Local indexing**: Zoekt (fast, proven, incremental)
2. **LSP integration**: Auto-fix + formatting (free, instant)
3. **Prompt caching**: Provider-specific strategies (OpenAI auto, Anthropic explicit)
4. **Spec-first**: Break into phases (plan validation → execution loop → integration)

---

## 1. Local Codebase Indexing (Zoekt)

### What is Zoekt?

**Origin**: Created by Han-Wen Nienhuys at Google, now maintained by Sourcegraph (1.1k stars)
**Tech**: Trigram-based text search (indexed, 50-100x faster than grep)
**Production Use**: GitLab Exact Code Search, Sourcegraph backend, actively maintained
**Verified**: Real tool used in production, not experimental

### Setup for Aircher

```bash
# Install
go install github.com/sourcegraph/zoekt/cmd/zoekt@latest
go install github.com/sourcegraph/zoekt/cmd/zoekt-git-index@latest

# Index local repository
zoekt-git-index -index ~/.aircher/index /path/to/repo

# Search
zoekt 'def authenticate' ~/.aircher/index
```

### Integration Pattern

```python
# src/aircher/tools/zoekt_search.py
import subprocess
import json

class ZoektSearch:
    def __init__(self, index_path: str = "~/.aircher/index"):
        self.index_path = os.path.expanduser(index_path)

    def search(self, query: str, repo: str = None) -> list[dict]:
        """Fast trigram search"""
        cmd = ["zoekt", "-json", query, self.index_path]

        result = subprocess.run(cmd, capture_output=True, text=True)
        matches = json.loads(result.stdout)

        # Rank by relevance
        return self._rank_results(matches)

    def reindex(self, repo_path: str):
        """Incremental reindex (seconds for changed files)"""
        subprocess.run([
            "zoekt-git-index",
            "-index", self.index_path,
            "-incremental",  # Only reindex changed files
            repo_path
        ])
```

### Performance

**Indexing**:
- Initial: ~2-3 seconds per 1000 files
- Incremental: Seconds for changed files
- Storage: ~10% of repo size

**Search**:
- Query time: <100ms for most repos
- Scales to millions of files

**Comparison**:
```
grep -r "authenticate" .    →  5-10 seconds
zoekt "authenticate"         →  50-100ms (50-100x faster)
```

### How Zoekt Combines with Current Aircher Architecture

**Current Aircher Stack**:
- ✅ tree-sitter → Knowledge Graph (code structure, relationships)
- ✅ ChromaDB → Vector Search (semantic similarity)
- ❌ LSP → Not yet integrated (precise symbols, types)
- ❌ Fast text search → Using slow ripgrep

**Hybrid Multi-Layer Search Pattern**:

```python
# User Query: "Where is authentication implemented?"

# LAYER 1: Fast Text Filter (Zoekt)
zoekt_results = zoekt.search("authenticate")  # <100ms, 50 files

# LAYER 2: Semantic Ranking (ChromaDB) - Filter candidates
embeddings = [chromadb.search(file) for file in zoekt_results[:20]]
top_semantic = embeddings[:10]  # Most semantically relevant

# LAYER 3: Structural Analysis (tree-sitter + Knowledge Graph)
for file in top_semantic:
    nodes = knowledge_graph.get_nodes(file)  # Functions, classes
    relationships = knowledge_graph.get_edges(file)  # Calls, imports

# LAYER 4: Precise Symbols (LSP - when available)
if lsp_available:
    definitions = lsp.get_definitions("authenticate")
    references = lsp.get_references("authenticate")

# RESULT: Fast initial filter → Semantic ranking → Structural context → Precise types
```

**Why This Works**:
1. **Zoekt**: Fast initial filter (text layer) - eliminates 90% of irrelevant files in <100ms
2. **ChromaDB**: Semantic understanding (meaning layer) - ranks by relevance
3. **Knowledge Graph**: Structural context (graph layer) - understands relationships
4. **LSP**: Precise symbols (type layer) - exact definitions and references

**Zoekt's Unique Role**:
- **Exact text matching**: ChromaDB is semantic, might miss exact identifier names
- **Speed**: 50-100x faster than ripgrep for initial filtering
- **Incremental**: Reindex changed files in seconds, not minutes

**Example Where Zoekt Helps**:
```
Task: "Find all files that import 'anthropic'"

WITHOUT Zoekt:
- ripgrep: 2-5 seconds for large repo
- ChromaDB alone: Might miss exact "import anthropic" if embedding is generic

WITH Zoekt:
- Zoekt: <100ms to find all files with "import anthropic"
- ChromaDB: Rank those files by semantic relevance to query
- Result: Fast + accurate
```

**Decision**: Keep Zoekt in the plan, integrate AFTER LSP (easier win first)

---

## 2. LSP Auto-Fix Integration

### Current State

**No LSP integration yet** - this is a net-new feature to implement.

### LSP Capabilities

**Code Actions** (Quick Fixes):
- Auto-import missing symbols
- Fix syntax errors
- Organize imports
- Apply suggested fixes

**Formatting**:
- Format on save
- Format selection
- Format on type (e.g., closing brace)

**Source Actions**:
- `source.organizeImports`: Clean up imports
- `source.fixAll`: Apply all auto-fixes
- `source.formatDocument`: Full file format

### Integration Pattern

```python
# terminal_bench_adapter/aircher_agent.py

class LSPAutoFix:
    def __init__(self, lsp_client):
        self.lsp = lsp_client

    def before_code_change(self, file_path: str, new_content: str) -> dict:
        """Run LSP checks BEFORE modifying file"""

        # 1. Get diagnostics (syntax errors, type errors)
        diagnostics = self.lsp.get_diagnostics(file_path)

        if diagnostics.has_errors():
            # Don't proceed - show LLM the errors
            return {
                "proceed": False,
                "errors": diagnostics.errors,
                "suggestion": "Fix LSP errors before proceeding"
            }

        # 2. Apply auto-fixes
        fixes = self.lsp.get_code_actions(file_path, filter="quickfix")
        if fixes:
            for fix in fixes:
                self.lsp.apply_code_action(fix)

        # 3. Format code
        formatted = self.lsp.format_document(file_path)

        return {
            "proceed": True,
            "auto_fixed": len(fixes),
            "formatted": formatted
        }

    def chain_format_fix(self, file_path: str):
        """Chain: format → fix → validate"""

        # Format first (clean up style)
        self.lsp.format_document(file_path)

        # Apply all auto-fixes
        self.lsp.execute_command("source.fixAll", file_path)

        # Organize imports
        self.lsp.execute_command("source.organizeImports", file_path)

        # Final validation
        diagnostics = self.lsp.get_diagnostics(file_path)

        return {
            "clean": not diagnostics.has_errors(),
            "remaining_issues": diagnostics.warnings
        }
```

### Benefits

**Token savings**:
```
WITHOUT LSP:
User: "Add authentication"
LLM: [writes code]
LLM: "Let me check if it's valid..." [uses tokens]
LLM: "Found syntax error, fixing..." [uses more tokens]

WITH LSP:
User: "Add authentication"
LLM: [writes code]
LSP: "Syntax error on line 5" [instant, free]
LLM: "Fixing line 5..." [minimal tokens]
```

**Estimation**: Saves 20-30% of tokens on code tasks

---

## 3. Prompt Caching Strategies

### Provider Comparison

| Feature | OpenAI | Anthropic | Gemini |
|---------|--------|-----------|--------|
| **Activation** | Automatic (>1024 tokens) | Explicit (`cache_control`) | Explicit (CachedContent) |
| **Min Tokens** | 1,024 | ~2,048 | 32,768 |
| **TTL** | 5-10min (up to 1hr off-peak) | 5min (1hr extended beta) | 1hr (custom available) |
| **Pricing** | 50% discount on cached input | 90% discount on cached input | Storage charges + usage discount |
| **Implementation** | Zero code changes | Mark cache boundaries | Create cache first |

### Aircher Strategy (Multi-Provider)

**Option 1: Provider-Agnostic Wrapper**

```python
# src/aircher/models/caching.py

class CacheablePrompt:
    """Adapter for multi-provider caching"""

    def __init__(self, provider: str):
        self.provider = provider
        self.cache_strategy = self._get_strategy(provider)

    def build_prompt(self, system: str, history: list, context: str):
        """Build prompt with provider-specific caching"""

        if self.provider == "openai":
            # Automatic - just ensure >1024 tokens
            return self._build_openai(system, history, context)

        elif self.provider == "anthropic":
            # Explicit cache_control markers
            return self._build_anthropic_cached(system, history, context)

        elif self.provider == "gemini":
            # Context caching API
            return self._build_gemini_cached(system, history, context)

        else:
            # No caching - standard prompt
            return self._build_standard(system, history, context)

    def _build_anthropic_cached(self, system, history, context):
        """Anthropic: Mark static content for caching"""

        return {
            "system": [
                {
                    "type": "text",
                    "text": system,
                    "cache_control": {"type": "ephemeral"}  # Cache system prompt
                },
                {
                    "type": "text",
                    "text": f"Repository context:\n{context}",
                    "cache_control": {"type": "ephemeral"}  # Cache repo context
                }
            ],
            "messages": history  # Don't cache messages (dynamic)
        }

    def _build_openai(self, system, history, context):
        """OpenAI: Automatic caching, optimize order"""

        # Put static content first (better cache hits)
        full_system = f"{system}\n\nRepository Context:\n{context}"

        return {
            "system": full_system,
            "messages": history
        }
```

**Option 2: Selective Caching by Task Type**

```python
class SmartCaching:
    def should_cache(self, task_type: str, content_size: int) -> bool:
        """Cache decision based on task + provider"""

        if self.provider == "openai":
            # Auto-cache, just ensure min size
            return content_size > 1024

        elif self.provider == "anthropic":
            # Cache for long sessions (5min TTL)
            return task_type in ["code_review", "refactoring", "debugging"]

        elif self.provider == "gemini":
            # Cache for very large context (32K min + storage cost)
            return content_size > 32000

        return False
```

### Cost Analysis

**Scenario**: Terminal-Bench task with 50K token repo context, 10 iterations

**Without caching**:
```
OpenAI:   10 requests × 50K = 500K input tokens × $2.50/M = $1.25
Anthropic: 10 requests × 50K = 500K input tokens × $3/M   = $1.50
```

**With caching** (8 cache hits after 2 cold):
```
OpenAI:   (2 × 50K) + (8 × 25K) = 300K tokens × $2.50/M = $0.75 (40% savings)
Anthropic: (2 × 50K) + (8 × 5K)  = 140K tokens × $3/M   = $0.42 (72% savings)
```

**Recommendation**: Enable caching for OpenAI (auto) and Anthropic (explicit), defer Gemini (high min threshold).

---

## 4. Spec-First Loop Scope

### Reality Check

**Full Factory Droid pattern** (14-19h estimated):
- ✅ Spec generation: 8-10h
- ✅ Autonomous loop: 4-6h
- ✅ Integration: 2-3h

**This is realistic but large**. Consider phased approach:

### Phase 1: Spec Generation Only (4-6h)

Generate execution plan upfront, but keep current loop:

```python
def perform_task_with_plan(self, task: str):
    # Generate spec FIRST
    spec = self._generate_task_spec(task)

    # Validate completeness
    if not self._validate_spec(spec):
        spec = self._refine_spec(spec)

    # Execute with CURRENT loop (not autonomous yet)
    # But: Use spec as guidance for each iteration
    for step in spec["steps"]:
        thought = f"Working on: {step['description']}"
        # ... existing loop logic
```

**Benefit**: Plan-driven execution without full rewrite
**Effort**: 4-6h
**Impact**: +3-5pp (better planning)

### Phase 2: Add Validation Gates (2-3h)

After each step, validate BEFORE continuing:

```python
for step in spec["steps"]:
    result = execute_step(step)

    # NEW: Validate after execution
    validation = self._validate_step_result(step, result)

    if not validation["passed"]:
        self._logger.warn(f"Step failed validation: {validation['issues']}")
        # But continue (not autonomous yet)
```

**Benefit**: Catch failures earlier
**Effort**: 2-3h
**Impact**: +2-3pp (fewer compounding errors)

### Phase 3: Autonomous Self-Correction (8-10h)

Full Factory Droid pattern with retry:

```python
for step in spec["steps"]:
    max_retries = 3

    while not step.complete and retries < max_retries:
        result = execute_step(step)
        validation = validate_step(step, result)

        if validation["passed"]:
            step.complete = True
        else:
            # Autonomous correction
            step = self_correct_step(step, validation["errors"])
            retries += 1
```

**Benefit**: Full autonomous correction
**Effort**: 8-10h (includes retry logic, error handling)
**Impact**: +5-7pp (eliminates iteration waste)

### Recommended Phased Rollout

**v14** (After v13): LSP auto-fix + Phase 1 spec generation (6-9h)
- Expected: +5-8pp → 35-38% accuracy

**v15**: Phase 2 validation gates (2-3h)
- Expected: +2-3pp → 37-41% accuracy

**v16**: Phase 3 autonomous correction (8-10h)
- Expected: +5-7pp → 42-48% accuracy

**Total effort**: 16-22h over 3 releases vs 14-19h in one big bang

---

## Summary Recommendations

### 1. Hybrid Multi-Layer Search Architecture

**The Pattern** (all layers work together):
```
User Query
    ↓
LAYER 1: Zoekt (fast text filter) → 90% reduction in <100ms
    ↓
LAYER 2: ChromaDB (semantic ranking) → Top 10 most relevant
    ↓
LAYER 3: Knowledge Graph (structural context) → Relationships
    ↓
LAYER 4: LSP (precise symbols) → Exact types and definitions
```

**Implementation Order**:
1. **v14**: LSP auto-fix integration (4-6h) - FIRST (easier, immediate token savings)
2. **v15**: Zoekt fast search (2-3h) - SECOND (complements LSP)
3. **Existing**: ChromaDB + Knowledge Graph (already working)

**Why This Order**:
- LSP auto-fix: Immediate 20-30% token savings, easier integration
- Zoekt: Faster context retrieval, combines with all layers

**Defer**:
- ❌ Sourcegraph API (external dependency, not needed with Zoekt)
- ❌ Custom SCIP index (Zoekt + LSP provides same value, simpler)

### 2. Spec-First Loop

**Phased approach** (total 16-22h):
1. v14: Spec generation + LSP auto-fix (6-9h)
2. v15: Validation gates (2-3h)
3. v16: Autonomous correction (8-10h)

**Alternative**: Full implementation (14-19h) if v13 results are strong

### 3. LSP Integration

**YES - High priority** (4-6h):
- Auto-fix before code execution
- Chain format → fix → validate
- Free token savings (20-30%)

### 4. Prompt Caching

**Multi-provider strategy**:
- OpenAI: Enable (automatic, zero code)
- Anthropic: Enable (explicit cache_control markers)
- Gemini: Defer (32K min + storage costs)
- Expected savings: 40-70% on cached input

---

## Next Steps (After v13)

**High Priority** (10-15h - Hybrid search architecture):
1. **LSP auto-fix integration** (4-6h) - FIRST
   - Chain format → fixAll → organizeImports → validate
   - 20-30% token savings, immediate impact
2. **Zoekt fast search** (2-3h) - SECOND
   - Layer 1 in hybrid architecture
   - Combines with ChromaDB + Knowledge Graph + LSP
3. **Spec generation Phase 1** (4-6h) - THIRD
   - Plan-driven execution (Factory Droid pattern)

**Medium Priority** (2-3h):
4. **Validation gates Phase 2** (2-3h)
   - Validate after each step in spec

**Deferred** (not critical yet):
5. Anthropic/OpenAI prompt caching (per user: "dont need to worry about caching so much yet")
6. Autonomous correction Phase 3 (8-10h - large effort)

---

## References

- Zoekt: https://github.com/sourcegraph/zoekt
- LSP Spec 3.17: https://microsoft.github.io/language-server-protocol/
- Prompt Caching Comparison: https://www.prompthub.us/blog/prompt-caching-with-openai-anthropic-and-google-models
- Factory Droid Architecture: `ai/research/factory-droid-architecture.md`

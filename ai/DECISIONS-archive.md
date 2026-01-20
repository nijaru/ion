# Aircher Decisions

## 2025-12-19: General-Purpose Memory Framework + Coding Archetype

**Context**: Refining strategy after analyzing framework vs product tradeoffs
**Decision**: Build a **general-purpose memory framework** (open source) with **coding archetype** as primary focus
**Rationale**:

### Why General-Purpose (Not Code-Only)

| Limitation of Code-Only   | Reality                                               |
| ------------------------- | ----------------------------------------------------- |
| Artificially constraining | Memory for agents is a massive, untapped market       |
| Competing on features     | Coding agent market is saturated (10+ funded players) |
| No ecosystem potential    | Can't build platform with single use case             |

### Why Framework + Archetype

| Advantage                   | Impact                                         |
| --------------------------- | ---------------------------------------------- |
| Framework is horizontal     | Works for coding, research, support, any agent |
| Archetype is vertical       | Coding archetype proves it works               |
| Open source builds adoption | Developers trust it, community grows           |
| Monetize convenience        | Paid tiers for binaries, sync, enterprise      |

### Competitive Positioning

| Aspect       | Letta        | Mem0        | **aircher**        |
| ------------ | ------------ | ----------- | ------------------ |
| Architecture | Server-based | Cloud-first | **Single binary**  |
| Deployment   | Run server   | API calls   | **Import library** |
| Privacy      | Self-host    | Cloud       | **100% local**     |
| Complexity   | High         | Medium      | **Low**            |
| Target       | Enterprise   | Startups    | **Developers**     |

Letta/Mem0 are cloud-first because that's their business model. We're local-first because it's genuinely better for many use cases, and they can't follow us there.

### Structure

```
aircher/
├── memory/           # FRAMEWORK (open source, MIT)
│   ├── episodic      # What happened
│   ├── semantic      # What we know
│   ├── relational    # How things connect
│   └── contextual    # Semantic search
│
└── archetypes/
    ├── coding/       # PRIMARY (our focus)
    ├── research/     # FUTURE
    └── [community]/  # ECOSYSTEM
```

### Business Model

| Tier       | Price      | What                         |
| ---------- | ---------- | ---------------------------- |
| Open Core  | Free (MIT) | Framework + coding archetype |
| Individual | $49 once   | Binary + updates + support   |
| Team       | $199 once  | 5 seats + git sync           |
| Enterprise | Custom     | SSO, audit, compliance, SLA  |

Add-ons:

- Cloud sync: $5/user/month
- Premium archetypes: $29-99 each
- Marketplace: 30% cut

### Tradeoffs

| Pro                         | Con                                     |
| --------------------------- | --------------------------------------- |
| Large market (all agents)   | More to build than single product       |
| Ecosystem potential         | Need adoption before enterprise revenue |
| Open source builds trust    | Code can be forked                      |
| Local-first differentiation | Letta can't easily follow               |

**Evidence**: Market analysis, Letta architecture constraints, monetization research
**Impact**: Framework-first development, coding archetype as primary use case

---

## 2025-12-19: Database Selection (bun:sqlite over alternatives)

**Context**: Evaluating SQLite vs Turso/libSQL vs Limbo vs DuckDB for memory storage
**Decision**: Use bun:sqlite for all relational storage
**Rationale**:

| Database     | Type           | Pros                             | Cons            | Verdict    |
| ------------ | -------------- | -------------------------------- | --------------- | ---------- |
| bun:sqlite   | Embedded       | Zero deps, built-in, 3-6x faster | No vectors      | **Winner** |
| Turso/libSQL | SQLite fork    | Sync, vectors (beta)             | Server for sync | Overkill   |
| Limbo        | SQLite rewrite | Rust, modern                     | Alpha, risky    | Too early  |
| DuckDB       | OLAP           | Great for analytics              | Wrong workload  | Wrong fit  |

**Memory workload is OLTP** (append events, query recent). bun:sqlite is ideal.

---

## 2025-12-19: Memory Architecture (10-Type Model)

**Context**: Designing comprehensive memory system for AI agents
**Decision**: Implement 10 memory types based on cognitive science + practical agent needs
**Rationale**:

### Memory Types

| Type          | Purpose         | Storage        | Priority |
| ------------- | --------------- | -------------- | -------- |
| Working       | Current focus   | In-memory      | v1       |
| Episodic      | What happened   | SQLite         | v1       |
| Contextual    | Semantic search | OmenDB         | v1       |
| Relational    | Connections     | SQLite + Graph | v1       |
| Semantic      | Entities        | SQLite         | v1.5     |
| Procedural    | Patterns        | SQLite         | v1.5     |
| User          | Preferences     | SQLite         | v1.5     |
| Prospective   | Goals           | SQLite         | v2       |
| Meta          | Self-knowledge  | SQLite         | v2       |
| Environmental | World model     | In-memory      | v2       |

### Why 10 Types?

1. **Cognitive science alignment** - Maps to human memory systems
2. **Complete coverage** - Handles all agent memory needs
3. **Prioritized** - Essential types first, future types later
4. **Archetype-customizable** - Each domain configures differently

See: [design/memory-architecture.md](design/memory-architecture.md)

---

## 2025-12-19: Archetype Pattern

**Context**: How to make memory framework domain-agnostic
**Decision**: Implement Archetype pattern - framework provides primitives, archetypes provide domain semantics
**Rationale**:

```typescript
// Framework provides
interface Memory {
  episodic: EpisodicStore;
  contextual: VectorStore;
  // ...
}

// Archetype customizes
interface Archetype {
  eventTypes: string[];     // Domain-specific events
  entityTypes: string[];    // Domain-specific entities
  extractors: {...};        // How to extract knowledge
  contextStrategy: {...};   // How to assemble context
}
```

### Benefits

1. **Separation of concerns** - Framework stable, archetypes evolve
2. **Domain flexibility** - Coding, support, research agents all different
3. **Easy extension** - Add new archetypes without changing framework
4. **Clean API** - Users think in domain terms, not primitives

See: [design/archetypes.md](design/archetypes.md)

---

## 2025-12-03: Python → Bun/TypeScript Migration

**Context**: Anthropic acquired Bun (Dec 2025). Industry shift to JS-based TUI agents.
**Decision**: Complete rewrite from Python to Bun/TypeScript
**Rationale**:

### Key Drivers

| Factor       | Python              | Bun                                   | Winner  |
| ------------ | ------------------- | ------------------------------------- | ------- |
| Distribution | pip/uv, pyinstaller | `bun build --compile` (single binary) | **Bun** |
| Startup      | 100-300ms           | 20-50ms                               | **Bun** |
| SQLite perf  | DuckDB (good)       | bun:sqlite 3-6x faster                | **Bun** |
| Ecosystem    | Stable              | Anthropic owns Bun                    | **Bun** |
| ACP/MCP SDKs | Custom impl         | Official JS SDKs                      | **Bun** |
| Memory libs  | NetworkX, ChromaDB  | Orama (vectors), bun:sqlite (events)  | **Bun** |

### Industry Evidence

| Agent          | Language   | Runtime |
| -------------- | ---------- | ------- |
| Claude Code    | TypeScript | **Bun** |
| OpenCode (SST) | TypeScript | **Bun** |
| Cline          | TypeScript | Node    |
| Gemini CLI     | TypeScript | Node    |
| Codex CLI      | TypeScript | Node    |
| Aider          | Python     | Python  |

Only Aider is Python. All IDE/TUI agents are TypeScript.

### Stack Changes

| Component       | Python          | TypeScript                 |
| --------------- | --------------- | -------------------------- |
| Runtime         | Python 3.13     | Bun                        |
| Agent framework | LangGraph       | Vercel AI SDK v5           |
| Episodic memory | DuckDB          | bun:sqlite (built-in)      |
| Vector memory   | ChromaDB        | Orama (OmenDB later)       |
| Knowledge graph | NetworkX        | Graphology (post-MVP)      |
| TUI             | (wait for Toad) | ACP-first (TUI post-MVP)   |
| ACP             | Custom          | @agentclientprotocol/sdk   |
| Tree-sitter     | py-tree-sitter  | web-tree-sitter (post-MVP) |

### Migration Strategy

1. Tag current Python as `v0.0.1-python`
2. Complete rewrite (no incremental migration)
3. Maintain feature parity checklist
4. Python code archived for reference

### Tradeoffs

| Pro                        | Con                            |
| -------------------------- | ------------------------------ |
| Single binary distribution | Rewrite effort (2-4 weeks)     |
| Ecosystem alignment        | Lose some Python libraries     |
| 3-6x memory performance    | Learning curve for TS patterns |
| Official SDK support       | OmenDB JS bindings pending     |

### What We Keep

- Memory architecture concepts (3-layer)
- Context engineering patterns (consolidation, provenance)
- Agent modes (READ/WRITE/ADMIN)
- Learned context design (.aircher/learned/)
- Project scoping approach

**Evidence**: ai/research/context-engineering-concepts.md, OpenCode source analysis
**Implementation**: ai/design/bun-migration.md

---

## 2025-11-21: Adaptive Spec Pattern (Beat Factory Droid)

**Context**: Factory Droid leads Terminal-Bench at 58.8% with spec-first execution
**Decision**: Implement Adaptive Spec Pattern that leverages our unique advantages
**Rationale**:

- **Factory Droid weakness**: No memory, no pre-validation, static specs
- **Our advantages**: 3-layer memory, LSP pre-validation, Knowledge Graph, hybrid search
- **Strategy**: Don't copy Factory Droid - improve on it with our capabilities

**Pattern Comparison**:

```
Factory Droid:  Task → Spec → Execute → Validate → Retry
Adaptive Spec:  Task → Memory Query → Informed Spec → Pre-Validate → Execute → Learn → Adapt
```

**Key Innovations**:

1. **Memory-informed spec generation**: Query past tasks, failures, codebase before planning
2. **LSP pre-validation**: Catch errors BEFORE execution, not after (30% fewer retries)
3. **Adaptive refinement**: Spec evolves during execution with new knowledge
4. **Memory-informed correction**: Use proven solutions, not blind retries
5. **Confidence-based execution**: Right validation depth for each step

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Leverages existing memory systems | More complex than Factory Droid |
| LSP catches errors pre-execution | Requires memory queries each task |
| Adaptive > static specs | More state to manage |
| Learning across sessions | Higher implementation effort |

**Expected Impact**:

- Target: >60% Terminal-Bench (vs Factory Droid's 58.8%)
- Retry reduction: >30% from LSP pre-validation
- Memory hit rate: >50% for similar tasks

**Implementation**: ai/research/adaptive-spec-pattern.md (full design)
**Timeline**: 19-27 hours across 5 sub-phases

---

## 2025-11-20: Hybrid Search (Regex-First + Semantic Fallback)

**Context**: Code search strategy for agent tool execution
**Decision**: Regex-first search (ripgrep) with conditional semantic fallback (ChromaDB)
**Rationale**:

- **Claude Code uses regex-only** (GrepTool, not vector search) - proven at 43.2% accuracy
- Regex fast path (10ms avg) handles most queries effectively
- Semantic fallback for edge cases: >50 results (ambiguous), <5 results (missed), natural language
- Feature flags enable A/B testing without code changes
- Metrics collection for empirical validation

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Fast primary path (ripgrep) | Complexity of dual systems |
| Semantic for hard queries | Memory integration required |
| A/B testable architecture | Fallback logic maintenance |
| Empirically validated design | Metrics collection overhead |

**Implementation**:

- `SearchTool`: Unified interface replacing `SearchFilesTool`
- Environment flags: `AIRCHER_SEARCH_HYBRID_ENABLED`, `FORCE_REGEX_ONLY`, `FORCE_SEMANTIC_ONLY`
- Metrics: JSONL persistence for per-query analysis
- Integration: Agent + all sub-agents use `SearchTool`

**Evidence**:

- ai/design/hybrid-search.md (architecture)
- ai/research/claude-code-architecture.md (Claude Code uses regex)
- scripts/validate_hybrid_search.py (validation)

**Next**: Terminal-Bench validation to measure accuracy impact

---

## 2025-11-12: Python over Rust for Agent Backend

**Context**: Architecture migration decision after Rust prototype analysis
**Decision**: Python with LangGraph framework
**Rationale**:

- Development velocity: Python ecosystem 3-5x faster for agent development
- LangGraph: Production-validated agent framework with built-in state management
- Library ecosystem: Rich AI/ML tooling (sentence-transformers, ChromaDB, etc.)
- Team expertise: Strong Python skills vs learning Rust async patterns

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Faster development cycles | Lower raw performance |
| Rich AI ecosystem | GIL limitations for CPU work |
| LangGraph state management | Memory overhead |
| Easier testing/debugging | Deployment complexity |

**Evidence**: ai/research/python_vs_rust_analysis.md
**Commits**: Phase 2 complete setup

---

## 2025-11-12: Multi-Database Strategy

**Context**: Memory system architecture design
**Decision**: SQLite + DuckDB + ChromaDB
**Rationale**:

- **SQLite**: Proven, embedded, ACID compliance for session data
- **DuckDB**: Columnar analytics, complex queries for episodic memory
- **ChromaDB**: Specialized vector database with embedding support

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Right tool for each job | Complexity of 3 systems |
| Optimized performance | Data synchronization overhead |
| Proven technologies | Learning curve |

**Evidence**: ai/research/database_strategy_analysis.md

---

## 2025-11-12: Custom ACP Implementation

**Context**: agent-protocol Python package conflicts with pydantic v2
**Decision**: Implement custom ACP protocol
**Rationale**:

- Avoid dependency conflicts with modern pydantic
- Full control over protocol features and extensions
- Better integration with our architecture
- Learning opportunity for protocol implementation

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| No dependency conflicts | More implementation work |
| Custom extensions possible | Maintenance responsibility |
| Full control | Need to ensure compatibility |

**Evidence**: Dependency resolution failures with agent-protocol package

---

## 2025-11-12: READ/WRITE + --admin Mode System

**Context**: Safety and usability requirements
**Decision**: READ/WRITE modes with optional --admin flag
**Rationale**:

- **READ**: Safe exploration, file reading only
- **WRITE**: File modifications with confirmation
- **--admin**: Full access without confirmations (overrides config)

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Intuitive terminology | Admin flag adds complexity |
| Clear permission model | Need --no-admin override |
| Progressive trust | Implementation overhead |

**Evidence**: User experience analysis, comparison with Claude Code/opencode patterns

---

## 2025-11-12: Modern Python Tooling

**Context**: Development environment setup
**Decision**: uv + ruff + ty + vulture + pytest
**Rationale**:

- **uv**: Fast package manager, resolves dependencies 10-100x faster
- **ruff**: Rust-based linting/formatting, 50-100x faster than traditional tools
- **ty**: Type checking from uv creators (replaces mypy)
- **vulture**: Dead code detection
- **pytest**: De facto testing standard with asyncio support

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Modern, fast tooling | Learning curve for team |
| Integrated workflows | Potential compatibility issues |
| Best practices | Dependency on newer tools |

**Evidence**: Modern Python development best practices research

---

## 2025-11-12: Phased TUI Approach (Updated 2025-12-01)

**Context**: Frontend architecture decision - build vs wait for Toad
**Decision**: Wait for Toad (Will McGugan), no custom frontend
**Rationale**:

- **Toad is ACP-ready**: Already implements Agent Client Protocol (Toad Report #2, Nov 2025)
- **Python/Textual stack**: Same ecosystem as aircher, zero context switching
- **Expert maintainer**: Will McGugan (Rich/Textual creator, 5+ years terminal UI expertise)
- **Open source committed**: "Could not be successful unless open source" - Will McGugan
- **Corporate sponsorship**: OpenHands as sponsor, indicates longevity

**Why NOT build our own**:

- **React/Ink bugs**: Claude Code has [active TUI crash issues](https://github.com/anthropics/claude-code/issues/10313)
- **Ink limitations**: 30 FPS cap, ~50MB memory baseline, React reconciliation overhead
- **Wrong stack**: TypeScript would require full context switch from Python
- **Duplicated effort**: McGugan already solving the hard TUI problems

**Why NOT other options**:

- **OpenTUI (Zig+TS)**: Early stage (5.6k stars), massive learning curve, wrong stack
- **BubbleTea (Go)**: Mature but Go doesn't fit AI ecosystem (no LangChain, ChromaDB, etc.)
- **Custom Textual**: Could prototype (~200 LOC) but Toad will be better

**Stack consistency**: Python everywhere

- Agent backend: Python (LangGraph, memory systems)
- Frontend: Toad (Python/Textual) when released
- Protocol: ACP (language-agnostic, already implemented)
- Hot paths: Rust if ever needed (tree-sitter, tiktoken already Rust)

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Zero frontend effort | Wait for Toad release |
| Expert-maintained UI | No control over timeline |
| Same Python stack | Dependency on external project |
| ACP-native | Private preview currently ($5K) |

**Timeline**: Toad in active development (Toad Report #3, Nov 17 2025), open source "when ready"
**Evidence**: willmcgugan.github.io/announcing-toad/, Toad Reports #1-3, our research (2025-12-01)

---

## 2025-11-12: Python 3.13+ Minimum

**Context**: Python version selection
**Decision**: Target Python 3.13+ instead of 3.12+
**Rationale**:

- **Performance**: 3.13+ has significant improvements
- **Dependencies**: All major deps support 3.13+
- **Future-proof**: 3.14 available, most libs compatible
- **Modern syntax**: Latest language features

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Latest performance | Reduced compatibility |
| Modern syntax | Fewer supported systems |
| Future-proof | Newer runtime requirements |

**Evidence**: Dependency compatibility analysis, Python 3.13 performance benchmarks

---

## 2025-11-12: Bash/Code Tools over MCP

**Context**: Tool architecture philosophy
**Decision**: Simple bash/code tools instead of MCP servers
**Rationale**:

- **Context efficiency**: 225 tokens vs 13-18k tokens for MCP
- **Composability**: Tools can be chained, results saved to files
- **Flexibility**: Easy to modify/extend tools
- **Performance**: Direct execution vs protocol overhead

**Evidence**: pi-mono browser tools analysis, MCP token usage benchmarks

---

## 2025-11-12: Modern Tools Integration

**Context**: Tool selection for agent operations
**Decision**: Assume modern tools, fallback to standard tools
**Rationale**:

- **Performance**: ripgrep, fd, sd significantly faster
- **User experience**: Most developers have these tools
- **Fallback**: Graceful degradation to grep, find, sed
- **Structured data**: nushell for complex data processing

**Tool Strategy**:

- **Essential**: ripgrep, fd, sd, jq (assume, fallback available)
- **Optional**: ast-grep, nushell, bat, delta (detect, use if available)
- **Python-based**: tree-sitter, PyYAML, toml (always available)

**Evidence**: Modern tooling performance benchmarks, developer tooling surveys

---

## 2025-11-12: Python/Mojo Long-term Stack

**Context**: Language stack evolution planning
**Decision**: Python now, Mojo integration later
**Rationale**:

- **Current**: Python ecosystem unmatched for AI/ML
- **Performance**: Mojo for critical paths when 1.0 released
- **Interop**: Mojo-Python interop is excellent
- **Timeline**: Mojo 1.0 expected summer 2025

**Migration Strategy**:

- **Phase 3-4**: Pure Python development
- **Phase 5+**: Identify performance bottlenecks
- **Phase 6+**: Mojo for critical components
- **Package Manager**: Stick with uv, can integrate with pixi later

**Evidence**: Mojo development roadmap, Python-Mojo interop analysis

---

## 2025-11-13: LangGraph over Pydantic AI

**Context**: Framework evaluation after Week 1-2 memory system implementation
**Decision**: Continue with LangGraph, avoid hybrid approach
**Rationale**:

- **Multi-agent requirements**: Week 3-4 roadmap requires CodeReading, CodeWriting, ProjectFixing sub-agents
- **State management**: 3-layer memory system needs LangGraph's sophisticated checkpointing and persistent state
- **Already invested**: Memory systems fully integrated with LangGraph workflows
- **Production readiness**: Built-in fault tolerance, durable execution, human-in-the-loop
- **Complexity fits**: Graph-based architecture handles context pruning, model routing, dynamic decisions
- **Maintenance**: Single framework simpler than hybrid LangGraph + Pydantic AI approach

**Pydantic AI Strengths Considered**:

- Type safety and IDE support
- Performance (faster than LangGraph in benchmarks)
- Simpler API for single agents
- OpenTelemetry observability

**Why Not Pydantic AI**:

- Not designed for complex multi-agent orchestration
- Migration cost too high at this stage
- Less mature state management for complex workflows
- Would need custom orchestration layer

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Built for multi-agent systems | More complex API |
| Sophisticated state management | Slower performance |
| Already integrated | Larger learning curve |
| Production-grade features | More abstraction layers |

**Future Considerations**:

- Evaluate Pydantic models for LLM output validation
- Consider Pydantic Logfire for observability (Week 5)
- Use type hints following Pydantic patterns

**Evidence**: Web research, framework comparison analysis, roadmap requirements
**Impact**: Week 3-6 development continues with LangGraph

---

## 2025-11-14: Custom ACP Protocol Implementation

**Context**: Week 5 - Need ACP compatibility for editor integration (Zed, Neovim, etc.)
**Decision**: Implement custom JSON-RPC stdio transport with full ACP server
**Rationale**:

- **Protocol core already existed**: `src/aircher/protocol/__init__.py` had message types (ACPRequest, ACPResponse, ACPSession)
- **Avoid dependency conflicts**: `agent-protocol` package conflicts with Pydantic v2
- **Full control**: Custom implementation allows optimization and customization
- **Minimal surface area**: Only need stdio transport + JSON-RPC handlers
- **Clean integration**: Reuse existing agent, memory, and model router systems

**Implementation** (730 lines total):

- **StdioTransport** (125 lines): Async message loop, JSON-RPC 2.0 compliance
- **ACPServer** (315 lines): Method handlers for initialize, session._, agent._, tool.\*
- **CLI serve command**: `aircher serve --model gpt-4o --enable-memory`
- **14 comprehensive tests**: 100% pass rate, no regressions

**Alternative Considered - agent-protocol package**:

- ❌ Pydantic v1 dependency (conflicts with our v2 codebase)
- ❌ Additional abstraction layer
- ❌ Less control over protocol details
- ✅ Standard implementation (but not needed for stdio use case)

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| No dependency conflicts | Maintain custom protocol code |
| Full control & optimization | Potential spec drift |
| Clean agent integration | Need to track ACP spec changes |
| Minimal code (730 lines) | No community package support |

**Capabilities Advertised**:

- Sessions: ✅ Full support (create, get, end)
- Tools: ✅ All 5 tools (read_file, write_file, list_directory, search_files, bash)
- Streaming: ⚠️ Not implemented (deferred - complex SSE implementation)
- Cost tracking: ✅ Automatic in all responses

**Testing**:

- 14 unit tests for protocol components
- 180 total tests passing (166 previous + 14 ACP)
- Integration testing deferred (needs real ACP client setup)

**Evidence**: ai/research/acp-integration-analysis.md, protocol conflicts during dependency analysis
**Impact**: Aircher is now ACP-compatible and can be used by Zed, Neovim, and other ACP-enabled editors
**Commits**: `02833d1` - ACP protocol with JSON-RPC stdio transport

---

## 2025-12-18: Framework Selection (AI SDK v5 direct, not Mastra)

**Context**: Evaluating TS agent frameworks for Phase 8
**Decision**: Use Vercel AI SDK v5 directly, skip Mastra
**Rationale**:

Original decision (Mastra) was based on incomplete research:

- We assumed AI SDK lacked orchestration - **wrong**: v5 has `maxSteps`, `prepareStep`, `stopWhen`
- We valued Mastra's built-in memory - **unnecessary**: we're building custom memory anyway
- Mastra adds 270kb+ to bundle and creates framework coupling

**Why AI SDK v5 is sufficient**:

```typescript
// AI SDK v5 native agent pattern
await generateText({
  model,
  tools,
  maxSteps: 20,
  prepareStep: async () => ({
    messages: [{ role: "system", content: await memory.getContext() }],
  }),
});
```

**Mastra overhead we avoid**:

| Mastra Feature  | Why Not Needed                    |
| --------------- | --------------------------------- |
| Built-in memory | We have libsql + Orama            |
| Workflows       | AI SDK maxSteps sufficient        |
| Observability   | Add OpenTelemetry later if needed |
| MCP support     | We implement ACP directly         |

**Lesson learned**: Verify assumptions before adopting frameworks. "Built-in X" is only valuable if you're not building custom X.

---

## 2025-12-18: Episodic Memory (bun:sqlite over libsql)

**Context**: Choosing embedded database for event storage
**Decision**: Use bun:sqlite (built-in) instead of libsql
**Rationale**:

| Factor       | bun:sqlite        | libsql        |
| ------------ | ----------------- | ------------- |
| Dependencies | 0 (built-in)      | 1-2 packages  |
| Performance  | 3-6x faster reads | Good          |
| Encryption   | No                | Yes (AES-256) |
| Remote sync  | No                | Yes (Turso)   |

**Why bun:sqlite wins for CLI tools**:

- Zero dependencies (built into Bun runtime)
- Fastest SQLite driver for JavaScript
- CLI tools don't need encryption (local files, user controls access)
- CLI tools don't need remote sync (single machine)
- Simpler synchronous API, perfect for CLI

**When to reconsider libsql**:

- Need encryption at rest for compliance
- Add cloud sync features (Turso integration)
- Cross-runtime support (Node/Deno compatibility)

```typescript
import { Database } from "bun:sqlite";
const db = new Database("aircher.db");
db.run("PRAGMA journal_mode = WAL;"); // Recommended for production
```

---

## 2025-12-18: Vector Memory (OmenDB dogfooding)

**Context**: Choosing vector database for semantic code search
**Decision**: Use OmenDB for dogfooding, Orama as fallback
**Rationale**:

**OmenDB** (`omendb@0.0.11`): Rust-based HNSW vector DB with JS bindings

- Maintainer wants real-world testing to find/fix bugs
- Research project tolerates some instability
- Abstraction interface allows swap to Orama if blocked

**Fallback strategy**:

```typescript
// Abstract vector storage behind interface
interface VectorStore {
  index(content: string, metadata: object): Promise<void>;
  search(query: string, k?: number): Promise<SearchResult[]>;
}

// OmenDB implementation (primary)
class OmenDBStore implements VectorStore { ... }

// Orama fallback if OmenDB blocks progress
class OramaStore implements VectorStore { ... }
```

**Embedding options**:

- Local: Ollama models (15-50ms, zero cost, privacy)
- API: OpenAI text-embedding-3-small ($0.02/M tokens)

---

## 2025-12-18: TUI Strategy (ACP-first)

**Context**: Toad released as ACP host, OpenTUI available for custom TUI
**Decision**: Support both via ACP protocol abstraction
**Rationale**:

```
User options:
  toad → Uses Toad TUI (Will McGugan's professional terminal UI)
  aircher --tui → Uses built-in OpenTUI
  aircher serve → Headless ACP server (for Zed, Neovim, etc.)
```

- ACP is the abstraction layer - any frontend works
- Toad is mature (v0.5.1, AGPL-3.0, 12+ agents supported)
- OpenTUI gives full control when needed
- Minimal extra work to support both

**Evidence**: Toad release research, ACP protocol docs

---

## 2025-12-18: Memory Enhancement (Letta-inspired self-edit)

**Context**: Letta AI (MemGPT) released with self-editing memory
**Decision**: Add agent self-edit tools to our 3-layer memory
**Rationale**:

Current 3-layer architecture:

- libsql → Episodic (tool calls, file interactions)
- OmenDB → Vector (semantic code retrieval)
- Graphology → Knowledge graph (code structure) ← **our differentiator**

Enhancement from Letta: Let agent actively manage memory:

```typescript
Agent tools:
  memory_note(fact) → Add to .aircher/learned/
  memory_search(query) → Query episodic + vector
  forget(id) → Mark memory as stale
```

**Key insight**: Letta lacks knowledge graph. Our tree-sitter code structure graph is unique.

**Evidence**: Letta docs, MemGPT paper, benchmarking analysis

---

## 2025-12-18: Language Confirmation (Bun/TypeScript)

**Context**: Re-evaluated Python vs Bun vs Rust after ecosystem research
**Decision**: Confirm Bun/TypeScript as correct choice
**Rationale**:

| Factor           | Python             | Bun/TS                           | Rust              |
| ---------------- | ------------------ | -------------------------------- | ----------------- |
| Startup          | 100-300ms          | **8ms**                          | ~10ms             |
| Distribution     | pip                | **Single binary**                | **Single binary** |
| Agent frameworks | LangGraph (mature) | Mastra (production)              | Rig (early)       |
| Type safety      | Gradual            | **Native (94% LLM error catch)** | Compile-time      |
| ACP/MCP SDKs     | Community          | **Official**                     | None              |
| Dev velocity     | Fast               | **Fast**                         | Slow              |

Rust rejected: Rig framework too early, slower development velocity outweighs performance gains for our scale.

Python rejected: Distribution complexity, no official ACP SDK, industry moving to TypeScript.

**Evidence**: Performance benchmarks, framework surveys, GitHub Octoverse 2025

---

## 2025-12-12: Model Default Update (DeepSeek V3.2 Speciale)

**Context**: Grok 4.1 Fast free tier ended. Need new default model.
**Decision**: DeepSeek V3.2 Speciale as default
**Rationale**:

- Same price as standard V3.2, optimized for agentic use
- Cost-effective for development phase
- Good tool calling and code generation

**Model Hierarchy**:

| Tier     | Model                           | Use Case                |
| -------- | ------------------------------- | ----------------------- |
| Default  | deepseek/deepseek-v3.2-speciale | Primary, agentic tasks  |
| Standard | deepseek/deepseek-v3.2          | If Speciale unavailable |
| Quality  | anthropic/claude-opus-4.5       | Complex reasoning       |
| Code     | openai/gpt-5.1-codex-max        | Code specialist         |

**Previous**: Grok 4.1 Fast was free until Dec 3rd 2025, now paid.
**Evidence**: OpenRouter pricing, user confirmation

---

## 2025-11-19: Prioritize MCP and Multi-Session Support

**Context**: SOTA analysis of 8 leading TUI agents (Codex, OpenCode, Gemini CLI, etc.)
**Decision**: Prioritize Model Context Protocol (MCP) and Multi-Session execution
**Rationale**:

- **MCP**: Emerging standard with 90% expected adoption. Critical for ecosystem compatibility.
- **Multi-Session**: Key productivity differentiator (OpenCode, Zed) allowing parallel tasks.
- **Sandboxing**: Reduced approval fatigue (Claude Code) - medium priority.

**Implementation Plan**:

- **Phase 1 (Week 9)**: Complete empirical validation (Terminal-Bench)
- **Phase 2 (Week 10-11)**: MCP Support (client/server) + Skills System
- **Phase 3 (Week 12+)**: Multi-session support, sandboxing

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Ecosystem compatibility | Implementation complexity |
| Higher productivity | State management challenges |
| Future-proofing | Resource diversion from core |

**Evidence**: ai/research/tui-agents-sota-2025.md

## 2025-11-19: Model Selection Strategy for Development

**Context**: Cost control during aircher development and testing
**Decision**: x-ai/grok-4.1-fast (FREE) as primary, escalate only when necessary
**Rationale**:

- **Development phase**: Avoid costs until aircher is feature complete and stable
- **FREE primary**: x-ai/grok-4.1-fast (OpenRouter, 2M context, tool calling)
- **Escalation path**:
  1. vLLM on fedora RTX 4090 (free, unlimited, better perf than Ollama)
  2. minimax/minimax-m2 ($0.26/$1.02 per M - cheapest paid)
  3. moonshotai/kimi-k2-thinking ($0.45/$2.35 per M - better quality)
- **AVOID until stable**: Frontier models (gpt-5.1-codex $1.25/$10, gemini-3-pro $2/$12, claude-4.5-sonnet $3/$15)

**Configuration Strategy**:

- Ollama/vLLM hosts configurable via env vars (OLLAMA_BASE_URL, VLLM_BASE_URL)
- Default: localhost, override to fedora tailscale (100.93.39.25) for better performance
- vLLM preferred over Ollama for performance when available

**Models Available**:
| Model | Cost | Context | Use Case |
|-------|------|---------|----------|
| x-ai/grok-4.1-fast | FREE | 2M | Primary (watch usage limits) |
| vllm/qwen3-coder-30b | FREE | 16K | Fallback, unlimited |
| ollama/qwen3-coder:30b | FREE | 256K | Alternative if vLLM down |
| ollama/deepseek-r1:70b | FREE | 64K | Reasoning tasks |
| minimax/minimax-m2 | $0.26/$1.02/M | 200K | Cheapest paid |
| moonshotai/kimi-k2-thinking | $0.45/$2.35/M | 262K | Better quality |
| openai/gpt-5.1-codex | $1.25/$10/M | 400K | DO NOT USE (expensive) |
| google/gemini-3-pro-preview | $2/$12/M | 1M | DO NOT USE (expensive) |
| anthropic/claude-4.5-sonnet | $3/$15/M | 1M | DO NOT USE (most expensive) |

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Zero cost during development | Grok may have usage limits |
| Fast iteration | Need to monitor free tier usage |
| Multiple fallback options | Config complexity for fedora setup |
| Clear cost escalation path | Manual switching if limits hit |

**Evidence**: OpenRouter free tier analysis, fedora RTX 4090 availability
**Impact**: Development costs = $0 until aircher stable, then migrate to best-performing paid model
**Implementation**: src/aircher/models/**init**.py with detailed comments and module docstring

---

## 2025-11-20: Hybrid Search Architecture (Regex-First)

**Context**: Claude Code research reveals they use regex (GrepTool) over vector databases
**Decision**: Implement hybrid search with **regex-first** strategy
**Rationale**:

- **Claude Code finding**: "Claude already understands code structure deeply enough to craft sophisticated regex patterns"
- **Our challenge**: Supporting smaller models (Qwen3-Coder 30B, local 7B) needs more scaffolding
- **Hypothesis**: Vector search compensates for smaller models, but regex should be primary for speed

**Architecture** (4-layer hybrid):

```python
# Layer 1: Fast text search (Zoekt/ripgrep) - PRIMARY
candidates = zoekt_search(query, max_results=100)

# Layer 2: Semantic filtering (ChromaDB) - FALLBACK
if len(candidates) > 50 or confidence < 0.8:
    candidates = chromadb_rerank(candidates, query, top_k=20)

# Layer 3: Structural context (Knowledge Graph)
context = knowledge_graph_expand(candidates, include_imports=True)

# Layer 4: Type information (LSP)
if needs_type_info:
    context = lsp_enhance(context, include_references=True)
```

**Validation Plan**:

- A/B test: Regex-only vs Hybrid search on Terminal-Bench
- Measure: Accuracy, tokens used, execution time
- Decision: Keep vector search only if empirical benefit >5pp accuracy

**Tradeoffs**:
| Pro | Con |
|-----|-----|
| Fast path (regex like Claude Code) | More complex pipeline |
| Semantic fallback for hard queries | Need to tune thresholds |
| Best of both worlds | 4 layers = debugging harder |
| Compensates for smaller models | ChromaDB overhead when used |

**Evidence**: ai/research/claude-code-architecture.md (Anthropic engineering posts)
**Impact**: Zoekt integration becomes HIGH priority (Week 10)

---

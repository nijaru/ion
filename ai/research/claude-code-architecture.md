# Claude Code Architecture - Official Anthropic Engineering Insights

**Date**: 2025-11-20
**Sources**: Anthropic engineering blog, Pragmatic Engineer analysis, reverse engineering posts
**Purpose**: Understand production agent architecture to inform Aircher design decisions

---

## Core Architecture

### Agent Loop (Simplicity Through Constraint)

**Design Philosophy**: Classic agent loop with intentional simplicity

```python
# Conceptual core loop
while(tool_call):
    execute_tool()
    feed_results_back()
    repeat()
```

**Key Characteristics**:
- **Single main thread** with one flat list of messages
- **No swarms**, no multiple agent personas competing for control
- **Explicit choice** for debuggability and reliability
- **Intentionally low-level** and unopinionated (close to raw model access)

**Quote**: "At Claude Code's heart beats a classic agent loop that embodies simplicity through constraint"

### Why This Matters for Aircher

**Comparison**:
- **Claude Code**: Simple loop, flat message list, direct tool execution
- **Aircher**: LangGraph state machine, 6-node workflow, conditional edges, multi-agent

**Trade-offs**:
| Approach | Pros | Cons |
|----------|------|------|
| Simple loop (Claude) | Easy to debug, predictable, fast | Limited capabilities, manual state |
| State machine (Aircher) | Rich features, automatic state, multi-agent | Complex to debug, more overhead |

**Conclusion**: Our complexity is **justified** because:
1. Memory persistence (Claude has none)
2. Local model support (needs more scaffolding)
3. Advanced features (context management, cost tracking)
4. Target: Beat Claude Code's 43.2% accuracy

---

## Search Strategy: Regex Over Vector Databases

### The Surprising Choice

**Decision**: Claude Code uses **GrepTool** (regex) instead of vector databases

**Rationale**:
> "Claude already understands code structure deeply enough to craft sophisticated regex patterns"

**Implementation**: Full regex-powered search utility, no semantic search

### Implications for Aircher

**Current Aircher Architecture**:
1. **ChromaDB**: Vector search with sentence-transformers
2. **Knowledge Graph**: Tree-sitter extraction, NetworkX
3. **DuckDB**: Episodic memory, learned patterns

**Question**: Is ChromaDB overkill if regex is sufficient?

**Analysis**:
- **Anthropic's advantage**: Frontier models (Claude 3.7+) with deep code understanding
- **Our challenge**: Supporting smaller models (Qwen3-Coder 30B, local 7B)
- **Hypothesis**: Vector search helps **compensate** for smaller models

**Recommended Approach** (Hybrid):
```
User Query → Zoekt (text filter) → ChromaDB (semantic) → Knowledge Graph (structure) → LSP (types)
```

**Rationale**:
1. **Zoekt**: Fast text search (like ripgrep), primary filter
2. **ChromaDB**: Semantic fallback when text search fails
3. **Knowledge Graph**: Structural understanding (imports, dependencies)
4. **LSP**: Type information, precise references

**Validation Plan**:
- A/B test: Regex-only vs Hybrid search on Terminal-Bench
- Measure: Accuracy, tokens used, execution time
- Decision: Keep vector search only if empirical benefit >5pp accuracy

---

## Subagents: Delegation Without Recursion

### Architecture

**Design**: Full agents with **limited tool sets** and **no spawning**

**Key Rule**: Subagents **cannot spawn other subagents** (prevents infinite nesting)

**Use Cases**:
- Research tasks (file exploration, codebase understanding)
- Parallel processing (multiple independent subtasks)
- Plan mode (read-only exploration)

**Implementation**:
- Inherit all tools from main thread by default
- Or specify individual tools as comma-separated list
- Automatic usage in plan mode (Shift+Tab twice)

### Aircher Comparison

**Current Aircher Subagents**:
- CodeReading (safe exploration)
- CodeWriting (file modifications)
- ProjectFixing (error recovery)

**Alignment**: ✅ Similar pattern (limited tool sets, no infinite nesting)

**Difference**: We use LangGraph nodes, they use direct delegation

---

## Context Management: Compact Feature

### Automatic Summarization

**Problem**: Long-running agents hit context limits

**Solution**: "Compact feature" automatically summarizes previous messages when context limit approaches

**Implementation**: Built into Claude Agent SDK harness

**Timing**: Triggered near context limit (not specified, likely 80-90%)

### Aircher Comparison

**Current Aircher**:
- ContextWindow with 5-factor relevance scoring
- Intelligent pruning at 80% capacity (120k/150k tokens)
- Episodic memory summarization

**Advantage**: ✅ More sophisticated (relevance scoring vs simple summarization)

**Risk**: ⚠️ More complex to debug

**Validation**: Ensure compaction doesn't hurt accuracy in benchmarks

---

## Sandboxing: 84% Reduction in Permission Prompts

### Architecture

**Components**:
1. **Filesystem isolation**: Network isolation via unix domain socket
2. **Proxy server**: Domain filtering outside sandbox
3. **Automatic decisions**: Allow safe ops, block malicious, ask only when needed

**Result**: 84% reduction in permission prompts in internal usage

**Quote**: "Sandboxing safely reduces permission prompts by 84%"

### Aircher Comparison

**Current Aircher**:
- READ/WRITE modes with confirmation
- --admin flag for no confirmations
- No sandboxing yet

**Priority**: Medium (nice-to-have after core features work)

**Implementation Plan** (Deferred):
- Week 12+: Docker-based sandboxing
- Network proxy with domain filtering
- Automatic safe operation detection

---

## Skills System: Prompt Expansion

### Architecture

**Design**: Modify agent behavior via markdown files, not executable code

**Mechanism**:
1. Load SKILL.md file
2. Expand to detailed instructions
3. Inject as new user messages into conversation context

**Benefits**:
- No code execution (safe)
- Easy to modify/extend
- Context modification without restart

**Example Use Cases**:
- Domain-specific coding patterns
- Project-specific conventions
- Team workflows

### Aircher Comparison

**Current Aircher**: No skills system yet

**Priority**: Medium (nice-to-have for customization)

**Implementation Plan** (Deferred):
- Week 11-12: .aircher/skills/ directory
- Markdown-based skill definitions
- Dynamic loading via prompt expansion

---

## MCP Protocol: Dual Nature

### Architecture

**Unique Capability**: Claude Code acts as **both** MCP server and client

**As MCP Server**:
- Exposes tools: Bash, Read, Write, Edit, GrepTool, GlobTool
- Other clients (Claude Desktop, Cursor, Windsurf) can invoke remotely
- One-shot agent-in-agent pattern

**As MCP Client**:
- Consumes external MCP servers
- Access to databases, APIs, external tools
- 90%+ expected ecosystem adoption (OpenAI, Google confirmed)

**Repository**: github.com/steipete/claude-code-mcp

### Aircher Comparison

**Current Aircher**:
- ✅ ACP protocol (JSON-RPC stdio)
- ❌ No MCP support yet

**Priority**: HIGH (ecosystem compatibility)

**Implementation Plan** (Updated):
- Week 10-11: MCP client support
- Week 12+: MCP server capability
- Integration with Claude Desktop, Cursor, Windsurf

**Evidence**: ai/DECISIONS.md 2025-11-19 decision prioritizes MCP

---

## Development Origins: The Accidental Product

### Story

**Timeline**:
- Sept 2024: Boris Cherny joins, builds prototypes with Claude 3.6
- Nov 2024: Sid Bidasaria joins, forms initial team
- Day 1: 20% of Engineering team using it
- Day 5: **50% of Engineering team** using it

**Quote**: "Claude Code was never supposed to become a product—it started as a scrappy prototype"

**Original Use Case**: CLI tool to state what music engineer was listening to at work

**Internal Debate**: Almost kept internal as competitive advantage

**Release Decision**: "Spread like wildfire" → public release

### Lesson for Aircher

**Key Insight**: Simple, useful tools get adopted organically

**Application**:
1. Focus on core value (memory, local models, transparency)
2. Make it work well for developers first
3. Polish comes after utility is proven

---

## Key Design Principles (Extracted)

### 1. Simplicity First
- Single main thread > complex swarms
- Regex > vector databases (for frontier models)
- Direct execution > abstraction layers

### 2. Debuggability
- Flat message list (easy to inspect)
- Classic agent loop (predictable)
- No infinite nesting (subagents can't spawn subagents)

### 3. Developer Tools
**Quote**: "Claude needs the same tools programmers use every day"

**Implication**: File ops, terminal, search, edit, test, debug

### 4. Progressive Enhancement
- Start simple (stdio)
- Add features based on need (sandboxing, MCP, skills)
- Low-level and unopinionated (let users build on top)

---

## Implications for Aircher Design

### What to Keep
✅ **Memory systems**: Claude Code has none, this is our differentiator
✅ **Cost tracking**: Transparency matters
✅ **Multi-agent**: Justified for complex workflows
✅ **Local models**: Avoid rate limits, unlimited usage

### What to Reconsider
⚠️ **Vector search**: Validate empirically vs regex-only
⚠️ **LangGraph complexity**: Consider simpler loop for debugging
⚠️ **Compaction logic**: Ensure doesn't hurt accuracy

### What to Add
🆕 **MCP support**: HIGH priority (ecosystem compatibility)
🆕 **Skills system**: MEDIUM priority (customization)
🆕 **Sandboxing**: MEDIUM priority (reduce prompts)
🆕 **Regex-first search**: Add Zoekt/ripgrep layer before vector search

### Architecture Changes Needed

**Search Strategy** (NEW):
```python
# Layer 1: Fast text search (Zoekt/ripgrep)
candidates = zoekt_search(query, max_results=100)

# Layer 2: Semantic filtering (ChromaDB) - only if needed
if len(candidates) > 50 or confidence < 0.8:
    candidates = chromadb_rerank(candidates, query, top_k=20)

# Layer 3: Structural context (Knowledge Graph)
context = knowledge_graph_expand(candidates, include_imports=True)

# Layer 4: Type information (LSP)
if needs_type_info:
    context = lsp_enhance(context, include_references=True)
```

**Benefits**:
- Fast path: regex-only (like Claude Code)
- Fallback: vector search for hard queries
- Best of both: speed + semantic understanding

---

## Performance Claims Analysis

### Sandboxing: 84% Reduction

**Claim**: "84% reduction in permission prompts"
**Context**: Internal Anthropic usage
**Validation**: Official engineering blog post

**Credibility**: ✅ HIGH (first-party data, reasonable magnitude)

**Application**: Sandboxing can significantly reduce friction

### Ecosystem Adoption: 90%+

**Claim**: "90%+ expected MCP adoption"
**Evidence**:
- OpenAI (ChatGPT) - March 2025
- Google (Gemini) - April 2025
- Block, Apollo, Zed, Replit, Codeium, Sourcegraph

**Credibility**: ✅ HIGH (multiple confirmed adopters)

**Application**: MCP is not optional, it's critical for ecosystem compatibility

---

## References

**Official Anthropic**:
- [Claude Code Sandboxing](https://www.anthropic.com/engineering/claude-code-sandboxing) - Architecture details
- [Building Agents with Claude Agent SDK](https://www.anthropic.com/engineering/building-agents-with-the-claude-agent-sdk) - Core loop
- [Claude Code Best Practices](https://www.anthropic.com/engineering/claude-code-best-practices) - Usage patterns

**Third-Party Analysis**:
- [How Claude Code is Built](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built) - Origins story
- [Behind-the-scenes of the master agent loop](https://blog.promptlayer.com/claude-code-behind-the-scenes-of-the-master-agent-loop/) - Architecture
- [Reverse Engineering Claude Code](https://www.reidbarber.com/blog/reverse-engineering-claude-code) - Technical deep dive
- [Understanding Claude Code's Full Stack](https://alexop.dev/posts/understanding-claude-code-full-stack/) - MCP + Skills + Subagents

**Documentation**:
- [Subagents - Claude Code Docs](https://docs.claude.com/en/docs/claude-code/sub-agents)
- [Agent SDK Overview](https://docs.claude.com/en/docs/agent-sdk/overview)
- [Connect Claude Code to MCP](https://docs.anthropic.com/en/docs/claude-code/mcp)

---

## Action Items

**Immediate** (Based on this research):
1. Add Zoekt/ripgrep layer before ChromaDB in search pipeline
2. A/B test regex vs hybrid search on Terminal-Bench
3. Validate compaction doesn't hurt accuracy in v13 results

**Short-term** (Week 10-11):
4. Implement MCP client support (HIGH priority)
5. Consider skills system for project customization

**Medium-term** (Week 12+):
6. Evaluate sandboxing implementation
7. Consider MCP server capability
8. Measure debugging complexity of LangGraph vs simple loop

**Research Questions**:
- Does vector search improve accuracy for smaller models? (Validate empirically)
- Does LangGraph complexity hurt debuggability vs simple loop? (Measure time-to-fix bugs)
- Can we match Claude Code's 43.2% with simpler architecture? (Benchmark)

---

**Conclusion**: Claude Code succeeds through **simplicity and focus**. Aircher's complexity (memory, multi-agent, vector search) is justified **only if** it delivers empirically better results. Validate everything.

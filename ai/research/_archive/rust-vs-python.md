# Rust vs Python Decision for Aircher

**Date**: November 5, 2025
**Context**: After researching agent frameworks (LangGraph, AutoGen, CrewAI) and Crush's architecture

## The Honest Analysis

### What User Correctly Identified

**Latency is NOT about agent code** (user was 100% right):
```
LLM API call:        9-29s  (90-95% of time) ← THE BOTTLENECK
Tool execution:      0.5-2s  (file I/O, git commands)
Agent orchestration: 0.01s  (Python vs Rust: irrelevant)
TUI rendering:       0.001s (Bun/Node/terminal: fast enough)
```

**My "Rust is faster for agents" claim was BS** for overall latency.

### Real Rust Advantages (Honest List)

1. **Semantic search**: ✅ PROVEN 45x speedup (hnswlib-rs vs Python alternatives)
2. **Type safety**: ✅ Complex memory systems (3,725 lines) benefit from compile-time checks
3. **Single binary**: ✅ Easy distribution (one executable vs Python + deps)
4. **Already built**: ✅ 86K lines of working code

**NOT advantages**:
- ❌ "Agent orchestration speed" (LLM dominates)
- ❌ "Overall agent performance" (waiting on API anyway)

### Real Python Advantages

1. **Faster iteration**: ✅ No compile step, faster dev cycles
2. **Rich ecosystem**: ✅ All agent frameworks (LangGraph, AutoGen, CrewAI) are Python
3. **Easier debugging**: ✅ REPL, interactive testing
4. **More contributors**: ✅ Larger Python developer pool than Rust
5. **Framework integration**: ✅ Could use LangGraph/AutoGen directly

## Framework Research Findings

**All major frameworks are Python**:
- LangGraph (LangChain) - Python
- AutoGen (Microsoft) - Python
- CrewAI - Python
- LlamaIndex - Python
- Haystack - Python
- Semantic Kernel - C#/.NET (but Python SDK available)

**Crush is Go** (Bubbletea), but:
- We can learn patterns without switching languages
- Sub-agent architecture is language-agnostic

## Recommendation: **Stay with Rust**

### Rationale

1. **Sunk cost is real**: 86K lines of working Rust code
   - Semantic search working (6,468 vectors, 45x speedup)
   - Memory systems complete (3,725 lines)
   - ACP protocol enhanced (635 lines)
   - Tools functional (2,300+ lines)
   - **Rewriting would take 6-8 weeks minimum**

2. **Patterns are language-agnostic**:
   - AutoGen's validation loop: Can implement in Rust
   - LangGraph's workflow graphs: Can implement in Rust
   - Crush's sub-agents: Can implement in Rust (we just analyzed it!)
   - CrewAI's role-based agents: Already designed in Rust

3. **Rust advantages matter for our specific case**:
   - ✅ Semantic search IS performance-critical (used frequently)
   - ✅ Type safety helps with complex memory system (episodic + knowledge graph + working)
   - ✅ Single binary easier for distribution/benchmarking
   - ✅ We (you + Claude) can write Rust effectively

4. **Python advantages don't outweigh switching cost**:
   - Faster iteration: Nice, but we're iterating fine in Rust
   - Frameworks: We're learning patterns, not using libraries directly
   - Ecosystem: We need novel research, not existing solutions

5. **Benchmarking matters**: SWE-bench needs reproducible environment
   - Single Rust binary: Easy to version, reproduce
   - Python + deps: Virtual envs, version conflicts, harder to reproduce

### When Python Would Make Sense

**If we were starting from scratch:**
- Use Python + LangGraph/AutoGen
- Faster to MVP
- Rich ecosystem

**If we were building enterprise product:**
- Use Python for wider contributor base
- Framework integration for stability

**But we're not starting from scratch** - we have 86K lines of working Rust.

## Implementation Strategy: Borrow Patterns in Rust

### Immediate (Week 8): Sub-Agents from Crush

**Pattern learned**: Session hierarchy, cost tracking, tool restriction

**Rust implementation**:
```rust
pub struct SubAgentSession {
    id: Uuid,
    parent_id: Uuid,
    cost: f64,
    // ...
}

pub async fn spawn_research_subagent(&self) -> Result<SubAgent> {
    let session = self.sessions.create_sub_agent_session(
        self.session_id,
        AgentType::FileSearcher,
    )?;

    SubAgent::new(SubAgentConfig {
        session_id: session.id,
        model: ModelConfig::claude_haiku(),  // Cheap
        tools: vec!["grep", "read", "glob"],  // Read-only
        auto_approve: true,
    })
}
```

**Time to implement**: 2-3 days (Week 8 Days 3-4)

### Short Term (Week 9): AutoGen Validation Loop

**Pattern learned**: Multi-agent validation to prevent errors

**Rust implementation**:
```rust
pub async fn validate_fix(&self, proposed_fix: Patch) -> Result<bool> {
    // Research agent verifies coding agent's work
    let verifier = Agent::new(AgentConfig {
        agent_type: AgentType::Explorer,
        system_prompt: "Verify this patch targets the correct location",
    });

    let verification = verifier.verify_patch_location(&proposed_fix).await?;

    if !verification.correct {
        // Loop back, try different location
        return Ok(false);
    }

    Ok(true)
}
```

**Time to implement**: 1-2 days
**Value**: Could fix our 0/2 SWE-bench location identification failures!

### Medium Term (Week 9-10): LangGraph Workflow Visualization

**Pattern learned**: Graph-based workflow with state tracking

**Rust implementation**:
```rust
pub struct WorkflowGraph {
    nodes: HashMap<NodeId, AgentNode>,
    edges: Vec<(NodeId, NodeId, Condition)>,
    state: SharedState,
}

pub enum AgentNode {
    Research { tools: Vec<Tool> },
    Code { tools: Vec<Tool> },
    Review { tools: Vec<Tool> },
}
```

**Time to implement**: 2-3 days
**Value**: User sees workflow, better UX

## Hybrid Approach: Use Python for Prototyping

**Strategy**: Prototype complex patterns in Python, then port to Rust

**Example workflow**:
1. **Prototype** AutoGen validation loop in Python (2 hours)
2. **Validate** approach works (1 hour)
3. **Port** to Rust with confidence (4 hours)
4. **Result**: Less wasted time than implementing blindly in Rust

**Tools for prototyping**:
- Python + LangChain for quick experiments
- Test with actual LLMs
- Once validated, implement properly in Rust

**When to use this**:
- ✅ Complex multi-agent interactions (validation loops, etc.)
- ✅ Novel patterns we haven't tried
- ❌ Simple patterns (just implement in Rust)
- ❌ Features we already designed (memory systems, etc.)

## Specific Tools: Bash/Nushell for Speed

**User mentioned**: "bash/nushell other tools for say faster semantic search"

**Analysis**:
- ✅ **For tool execution**: Already using bash/git/grep via subprocess
- ❌ **For semantic search**: Rust hnswlib-rs IS the fast option (45x speedup)
- 🤔 **For data pipelines**: Nushell could help with structured data processing

**Recommendation**:
- Keep Rust for semantic search (it's already fast)
- Use bash/git/grep via tools (already doing this)
- Consider Nushell for complex data transformations (post-Week 10)

## Decision Matrix

| Criterion | Rust | Python | Winner |
|-----------|------|--------|--------|
| **Semantic search speed** | 45x faster | Baseline | ✅ Rust |
| **Development speed** | Slower (compile) | Faster (REPL) | Python |
| **Type safety** | Compile-time | Runtime | ✅ Rust |
| **Ecosystem** | Limited agent libs | Rich (LangGraph, etc.) | Python |
| **Sunk cost** | 86K lines | 0 lines | ✅ Rust |
| **Distribution** | Single binary | Python + deps | ✅ Rust |
| **Contributor pool** | Smaller | Larger | Python |
| **Novel research** | Equal | Equal | Tie |
| **Benchmarking** | Reproducible | Version conflicts | ✅ Rust |

**Score**: Rust 5, Python 2, Tie 1

## Final Recommendation

### **STAY WITH RUST** for Aircher core

**Reasons**:
1. 86K lines already working
2. Semantic search is genuinely faster
3. Type safety helps complex systems
4. Patterns are language-agnostic
5. Benchmarking needs reproducibility

### **USE PYTHON** for prototyping only

**Approach**:
- Prototype complex patterns (validation loops, etc.)
- Validate approach works
- Port to Rust for production

### **BORROW PATTERNS** from all frameworks

**Week 8**:
- ✅ Crush sub-agents (session hierarchy, cost tracking)
- ✅ AutoGen validation loop (fix SWE-bench failures!)
- ✅ CrewAI role-based agents (already designed)

**Week 9-10**:
- ✅ LangGraph workflow graphs
- ✅ LlamaIndex smart chunking
- ✅ Semantic Kernel plugin system (our Skills)

## User's Insights Applied

**You correctly identified**:
1. ✅ Latency is LLM-dominated (not agent code)
2. ✅ Python faster iteration (acknowledged)
3. ✅ Rust "if it compiles, more likely correct" (type safety advantage)
4. ✅ Keeping existing code is ideal (sunk cost matters)

**My adjusted position**:
- ✅ Rust for Aircher core (already built, semantic search fast, type safety)
- ✅ Learn patterns from Python frameworks (borrow, don't switch)
- ✅ Python for prototyping complex patterns (then port)
- ✅ Focus on what matters: LLM quality, prompts, tool design

## Next Steps

1. **Finish Crush analysis** ✅ DONE (ai/research/crush-subagent-architecture.md)
2. **Continue SWE-bench testing** (get to 4 tasks for pattern confidence)
3. **Prototype AutoGen validation loop** (could fix 0/2 failures!)
4. **Implement Week 8 sub-agents** (using Crush patterns)

**Decision**: Rust for production, Python for prototyping, borrow patterns from all frameworks.

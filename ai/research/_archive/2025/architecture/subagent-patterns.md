# Crush Sub-Agent Architecture Analysis

**Date**: November 5, 2025
**Source**: https://github.com/charmbracelet/crush (v0.15.0+)
**Feature**: Agentic Fetch - Sub-agents for web search and content analysis

## Overview

Crush (by Charm, built with Bubbletea in Go) implements sub-agents specifically for web content fetching and analysis. This is a production-validated pattern we can adapt for Aircher's Week 8 research sub-agent implementation.

## Key Architecture Patterns

### 1. Sub-Agent Spawning Pattern

**File**: `internal/agent/agentic_fetch_tool.go` (lines 158-168)

```go
// Create new SessionAgent with limited tools
agent := NewSessionAgent(SessionAgentOptions{
    LargeModel:           small,  // Use small model for both
    SmallModel:           small,
    SystemPromptPrefix:   smallProviderCfg.SystemPromptPrefix,
    SystemPrompt:         systemPrompt,
    DisableAutoSummarize: c.cfg.Options.DisableAutoSummarize,
    IsYolo:               c.permissions.SkipRequests(),
    Sessions:             c.sessions,
    Messages:             c.messages,
    Tools:                fetchTools,  // ← Limited tool set
})
```

**Key insight**: Sub-agents are **full agents** with their own:
- Model (uses small model for cost optimization)
- System prompt (specialized for the task)
- Tool set (limited to relevant tools only)
- Session (child session under parent)

### 2. Tool Restriction for Sub-Agents

**File**: `internal/agent/agentic_fetch_tool.go` (lines 150-156)

```go
fetchTools := []fantasy.AgentTool{
    webFetchTool,         // Fetch additional URLs
    tools.NewGlobTool(tmpDir),    // Find files
    tools.NewGrepTool(tmpDir),    // Search content
    tools.NewViewTool(...),       // Read files
}
// NO bash, edit, write tools - read-only!
```

**Key insight**: Sub-agents get **minimal necessary tools** only:
- ✅ Web fetch (to follow links)
- ✅ Glob/grep (to analyze fetched content)
- ✅ View (to read files)
- ❌ NO bash (can't execute commands)
- ❌ NO edit/write (read-only, safe)

**Matches our Plan mode design!** (from ai/SYSTEM_DESIGN_2025.md)

### 3. Cost Optimization: Small Model for Sub-Agents

**File**: `internal/agent/agentic_fetch_tool.go` (line 159)

```go
agent := NewSessionAgent(SessionAgentOptions{
    LargeModel:  small,  // Use small model for both
    SmallModel:  small,
    // ...
})
```

**Key insight**: **Use cheap model for sub-agents**
- Main agent: Large/expensive model (Sonnet, Opus)
- Sub-agents: Small/cheap model (Haiku)
- Rationale: Research tasks don't need complex reasoning

**Matches our model router design!** Target 40% cost reduction via smart routing.

### 4. Session Hierarchy & Cost Tracking

**File**: `internal/agent/agentic_fetch_tool.go` (lines 170-213)

```go
// Create child session under parent
agentToolSessionID := c.sessions.CreateAgentToolSessionID(
    validationResult.AgentMessageID,
    call.ID
)
session, err := c.sessions.CreateTaskSession(
    ctx,
    agentToolSessionID,
    validationResult.SessionID,  // ← Parent session ID
    "Fetch Analysis"
)

// Run sub-agent
result, err := agent.Run(ctx, SessionAgentCall{
    SessionID: session.ID,  // Child session
    // ...
})

// Roll up costs to parent
parentSession.Cost += updatedSession.Cost
```

**Key insight**: **Parent-child session tracking**:
- Each sub-agent gets own session ID
- Session hierarchy: parent → child(ren)
- Costs accumulate to parent (user sees total)
- Enables debugging: can trace which sub-agent did what

### 5. Auto-Approval for Sub-Agents

**File**: `internal/agent/agentic_fetch_tool.go` (line 176)

```go
c.permissions.AutoApproveSession(session.ID)
```

**Key insight**: **Sub-agents don't interrupt user**
- Main agent: User approval required for risky operations
- Sub-agents: Auto-approved (tools are safe, read-only)
- User already approved the research task, no need to re-approve each fetch

**Matches our hybrid design**: Plan mode sub-agents operate autonomously.

### 6. Temporary Workspace Isolation

**File**: `internal/agent/agentic_fetch_tool.go` (lines 99-103)

```go
tmpDir, err := os.MkdirTemp(c.cfg.Options.DataDirectory, "crush-fetch-*")
if err != nil {
    return fantasy.NewTextErrorResponse(...), nil
}
defer os.RemoveAll(tmpDir)  // Clean up after
```

**Key insight**: **Sub-agents work in isolated temp directory**
- Each sub-agent gets own temp workspace
- Fetched content saved to temp files
- Cleanup automatic (defer statement)
- Prevents pollution of main working directory

### 7. Large Content Handling

**File**: `internal/agent/tools/web_fetch.go` (lines 44-68)

```go
hasLargeContent := len(content) > LargeContentThreshold

if hasLargeContent {
    // Save to temp file
    tempFile, err := os.CreateTemp(workingDir, "page-*.md")
    // Write content to file
    tempFile.WriteString(content)
    // Tell agent to use view/grep tools to analyze
    result.WriteString("Use the view and grep tools to analyze this file.")
} else {
    // Small content: inline in response
    result.WriteString(content)
}
```

**Key insight**: **Smart content handling**
- Small content (<threshold): Inline in context
- Large content (>threshold): Save to file, provide path
- Sub-agent uses view/grep tools to analyze large files
- Prevents context overflow

## Comparison with Our Design

| Aspect | Crush Implementation | Our Week 8 Design | Match? |
|--------|---------------------|-------------------|--------|
| **Sub-agent trigger** | Explicit tool call (agentic_fetch) | Plan mode research tasks | ✅ Similar |
| **Tool restriction** | Read-only (fetch, glob, grep, view) | Plan mode tools (grep, read, glob, LSP) | ✅ Match |
| **Model selection** | Small model for sub-agents | Haiku for sub-agents | ✅ Match |
| **Session tracking** | Parent-child hierarchy | Planned but not detailed | ✅ Learn from Crush |
| **Cost tracking** | Roll up to parent | Need to implement | 📝 TODO |
| **Auto-approval** | Yes for sub-agents | Planned (Plan mode autonomous) | ✅ Match |
| **Workspace isolation** | Temp directory per sub-agent | Not planned | 🤔 Consider |
| **Max concurrent** | Not limited in code | Max 10 (Claude Code pattern) | ⚠️ We should limit |

## What We Should Adopt for Aircher

### High Priority (Week 8 Implementation)

1. **Session Hierarchy Pattern** ✅
   ```rust
   pub struct SubAgentSession {
       id: Uuid,
       parent_session_id: Uuid,  // Link to parent
       agent_type: AgentType,     // FileSearcher, etc.
       created_at: DateTime<Utc>,
       cost: f64,                 // Roll up to parent
   }
   ```

2. **Cost Tracking Roll-Up** ✅
   ```rust
   // After sub-agent completes
   let parent_session = sessions.get(parent_id)?;
   parent_session.cost += sub_agent_session.cost;
   sessions.save(parent_session)?;
   ```

3. **Auto-Approval for Sub-Agents** ✅
   ```rust
   // Sub-agents in Plan mode auto-approve
   if agent_mode == AgentMode::Plan && is_subagent {
       permissions.auto_approve(session_id);
   }
   ```

4. **Small Model Selection** ✅
   ```rust
   // Already designed in model router
   match agent_type {
       AgentType::FileSearcher => ModelConfig::claude_haiku(),
       AgentType::PatternFinder => ModelConfig::claude_haiku(),
       // Main agent uses Sonnet/Opus
   }
   ```

### Medium Priority (Week 8-9)

5. **Temporary Workspace Isolation** 🤔
   ```rust
   let tmp_dir = tempfile::tempdir_in(&config.data_dir)?;
   let sub_agent = SubAgent::new(SubAgentConfig {
       working_dir: tmp_dir.path(),  // Isolated
       // ...
   });
   // Cleanup automatic via Drop trait
   ```

   **Value**: Prevents sub-agents from polluting main workspace
   **Cost**: Complexity, need to handle file paths carefully

6. **Large Content Handling** ✅
   ```rust
   const LARGE_CONTENT_THRESHOLD: usize = 50_000; // chars

   if content.len() > LARGE_CONTENT_THRESHOLD {
       let temp_file = write_to_temp(&content)?;
       format!("Content saved to: {}\nUse view/grep to analyze", temp_file)
   } else {
       content  // Inline
   }
   ```

### Low Priority (Post-Week 10)

7. **Concurrent Sub-Agent Limits**
   ```rust
   const MAX_CONCURRENT_SUBAGENTS: usize = 10;

   if active_subagents.len() >= MAX_CONCURRENT_SUBAGENTS {
       return Err("Too many concurrent sub-agents");
   }
   ```

## Web Search Integration Insights

**Crush uses**: Direct HTTP fetch + HTML-to-Markdown conversion

**We should use**: Specialized search APIs
- **Brave Search API**: Structured search results
- **Exa API**: Code-specific search
- **Direct fetch**: As fallback (like Crush)

**Why better than Crush's approach**:
- Search APIs pre-filter/rank results
- Structured JSON output (easier to parse)
- Better for "find X" tasks vs "analyze URL Y"

## Implementation Timeline for Aircher

### Week 8 Days 3-4: Research Sub-Agents

**Day 3: Core Infrastructure**
- [ ] SubAgentSession struct with parent tracking
- [ ] SubAgent spawning logic (max 10 concurrent)
- [ ] Cost roll-up to parent session
- [ ] Auto-approval for sub-agent sessions

**Day 4: Web Search Integration**
- [ ] Brave Search API tool (for sub-agents)
- [ ] Exa API tool (for code search)
- [ ] Large content handling (file vs inline)
- [ ] Result aggregation from multiple sub-agents

**Day 5-7: Testing & Integration**
- [ ] Test: Spawn 5 parallel sub-agents for research
- [ ] Measure: Speedup vs sequential (target 90% improvement)
- [ ] Verify: Costs roll up correctly
- [ ] Validate: Memory prevents duplicate research

## Code Examples for Aircher

### 1. Spawning Research Sub-Agents

```rust
pub async fn execute_research_task(&self, query: &str) -> Result<ResearchResults> {
    // Check episodic memory first (prevent duplicate research)
    if let Some(cached) = self.memory.find_similar_research(query, 0.85).await? {
        if cached.timestamp.elapsed() < Duration::from_secs(3600) {
            return Ok(ResearchDecision::UseCache(cached.results));
        }
    }

    // Break into parallel subtasks
    let subtasks = self.decompose_research_query(query).await?;

    // Spawn sub-agents (max 10)
    let handles: Vec<_> = subtasks.into_iter()
        .take(10)
        .map(|task| {
            let parent_session = self.session_id.clone();
            let config = SubAgentConfig {
                agent_type: AgentType::FileSearcher,
                model: ModelConfig::claude_haiku(),  // Cheap model
                tools: vec!["grep", "read", "glob"],  // Read-only
                parent_session_id: parent_session,
                auto_approve: true,  // No user interruption
            };

            tokio::spawn(async move {
                let sub_agent = SubAgent::new(config);
                sub_agent.research(task).await
            })
        })
        .collect();

    // Aggregate results
    let results = join_all(handles).await?;

    // Roll up costs
    let total_cost: f64 = results.iter().map(|r| r.cost).sum();
    self.session.cost += total_cost;

    // Save to episodic memory
    self.memory.record_research_session(query, &results).await?;

    Ok(ResearchResults::new(results))
}
```

### 2. Session Hierarchy Tracking

```rust
pub struct SessionService {
    sessions: HashMap<Uuid, Session>,
}

impl SessionService {
    pub fn create_sub_agent_session(
        &mut self,
        parent_id: Uuid,
        agent_type: AgentType,
    ) -> Result<Uuid> {
        let session = Session {
            id: Uuid::new_v4(),
            parent_id: Some(parent_id),  // ← Hierarchy
            agent_type,
            created_at: Utc::now(),
            cost: 0.0,
            status: SessionStatus::Active,
        };

        let id = session.id;
        self.sessions.insert(id, session);
        Ok(id)
    }

    pub fn roll_up_cost(&mut self, session_id: Uuid) -> Result<()> {
        let session = self.sessions.get(&session_id)
            .ok_or(anyhow!("Session not found"))?;

        if let Some(parent_id) = session.parent_id {
            let cost = session.cost;
            let parent = self.sessions.get_mut(&parent_id)
                .ok_or(anyhow!("Parent session not found"))?;
            parent.cost += cost;
        }

        Ok(())
    }
}
```

## Key Takeaways

1. **Sub-agents are full agents** with limited tools and cheap models
2. **Session hierarchy** enables cost tracking and debugging
3. **Auto-approval** for sub-agents prevents user interruption
4. **Tool restriction** ensures safety (read-only for research)
5. **Cost optimization** via small models (40% reduction target achievable)
6. **Temporary workspaces** keep sub-agent work isolated

## Validation of Our Hybrid Architecture

Crush's implementation **validates multiple patterns** from our Week 8 design:
- ✅ Plan mode with read-only tools
- ✅ Small model for cost optimization
- ✅ Auto-approval for research tasks
- ✅ Limited tool sets for focused agents

**Confidence**: High that our hybrid architecture is on the right track.

## Next Steps

1. **Implement session hierarchy** (Week 8 Day 3)
2. **Add Brave/Exa search APIs** (Week 8 Day 4)
3. **Test parallel sub-agents** (Week 8 Day 5)
4. **Measure 90% speedup** (Week 8 Day 6)

---

**References**:
- Crush source: https://github.com/charmbracelet/crush
- Agentic fetch release: https://github.com/charmbracelet/crush/releases/tag/v0.15.0
- Our hybrid design: ai/SYSTEM_DESIGN_2025.md
- Week 8 plan: docs/ROADMAP.md

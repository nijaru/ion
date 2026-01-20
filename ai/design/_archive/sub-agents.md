# Sub-Agents Design

**Date**: 2026-01-16
**Status**: Design Complete
**Purpose**: Define which tasks use sub-agents vs skills, and model selection

## Core Principle

Sub-agents are for **context isolation**, not **behavior specialization**.

| Pattern       | Use Case                           | Context         |
| ------------- | ---------------------------------- | --------------- |
| **Sub-Agent** | Large expansion → small synthesis  | Isolated window |
| **Skill**     | Different behavior, same knowledge | Main window     |

## Evidence-Based Criteria

### When to Use Sub-Agents

Sub-agents work when: `INPUT (large) → PROCESS → OUTPUT (small)`

- Exploration: Read 50 files → "found X in 3 locations"
- Research: 20 web searches → 2-page synthesis
- Review: Build + tests → "failed: 3 errors in auth.rs"

### When to Use Skills

Skills work when: `INPUT (context) → PROCESS → OUTPUT (context-dependent)`

- Developer: Needs implicit decisions from conversation
- Designer: Needs full problem context for architecture
- Refactor: Needs to understand existing patterns

**Key failure mode** (Cognition's Flappy Bird): Two sub-agents making independent style decisions produce incompatible outputs. Actions carry implicit decisions.

## Sub-Agent Types

| Type           | Purpose                     | Model | Tools                     |
| -------------- | --------------------------- | ----- | ------------------------- |
| **Explorer**   | Find files, search patterns | Fast  | Glob, Grep, Read          |
| **Researcher** | Web search, doc synthesis   | Full  | WebSearch, WebFetch, Read |
| **Reviewer**   | Build, test, analyze        | Full  | Bash, Read, Glob, Grep    |

### Model Selection

Binary choice—fast or full:

```rust
enum SubAgentModel {
    Fast,  // Haiku-class (exploration)
    Full,  // Inherit from main agent
}
```

**Explorer** uses Fast because:

- Iterative (5-10 searches per task)
- Pattern matching, not reasoning
- Claude Code validates this with Haiku

**Researcher/Reviewer** use Full because:

- Synthesis quality degrades with smaller models
- Review accuracy drops to ~0% for real bugs with small models
- Single invocation, cost not multiplied

## Skill Types

| Skill         | Purpose               | Tool Restrictions          |
| ------------- | --------------------- | -------------------------- |
| **developer** | Code implementation   | All tools                  |
| **designer**  | Architecture planning | Read-only + Write for docs |
| **refactor**  | Code restructuring    | All tools                  |

Skills modify behavior in the main context via prompt injection and optional tool restrictions.

## Architecture

```
User Query
    │
    ▼
┌─────────────────┐
│ Main Agent      │ ◄── User's configured model
│ + Active Skill  │     (behavior modification)
└────────┬────────┘
         │
    [Agent Decision] ◄── Agent decides when to spawn
         │
    ┌────┴────┬──────────┐
    ▼         ▼          ▼
Explorer   Researcher   Reviewer
(Fast)     (Full)       (Full)
    │         │          │
    └────┬────┴──────────┘
         ▼
    Summary returned to main context
```

## Implementation

### Sub-Agent Spawn

```rust
pub struct SubAgent {
    pub agent_type: SubAgentType,
    pub prompt: String,
    pub model: SubAgentModel,
    pub tools: Vec<String>,
}

pub enum SubAgentType {
    Explorer,
    Researcher,
    Reviewer,
}

impl SubAgentType {
    pub fn model(&self) -> SubAgentModel {
        match self {
            Self::Explorer => SubAgentModel::Fast,
            _ => SubAgentModel::Full,
        }
    }

    pub fn tools(&self) -> &[&str] {
        match self {
            Self::Explorer => &["glob", "grep", "read"],
            Self::Researcher => &["web_search", "web_fetch", "read"],
            Self::Reviewer => &["bash", "read", "glob", "grep"],
        }
    }
}
```

### Skill Loading

```rust
// ~/.config/ion/skills/developer.skill.md
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub model_override: Option<String>,
    pub prompt: String,
}

// Skill activates by injecting prompt into main context
// No context isolation - full conversation history preserved
```

## Comparison to Claude Code

| Aspect        | Claude Code      | ion                 |
| ------------- | ---------------- | ------------------- |
| Explorer      | Haiku, read-only | Fast, read-only     |
| Research      | Via Task tool    | Dedicated sub-agent |
| Review        | Via Task tool    | Dedicated sub-agent |
| Skills        | Custom agents    | SKILL.md files      |
| Model routing | Per-agent config | Binary (fast/full)  |

## Key Decisions

1. **Skills over sub-agents for behavior**: Developer, designer, refactor are skills (same context)
2. **Sub-agents for isolation only**: Explorer, researcher, reviewer (context would explode)
3. **Binary model selection**: Fast (explorer) or Full (everything else)
4. **No complex routing**: Agent decides when to spawn, not embedding classifier

## References

- `ai/research/agent-comparison-2026.md` - Pi-Mono vs Claude Code analysis
- `ai/research/model-routing-for-subagents.md` - Model selection research
- Cognition blog: "Don't Build Multi-Agents" - Context isolation tradeoffs
- UBC paper: "When Single-Agent with Skills Replace Multi-Agent Systems"

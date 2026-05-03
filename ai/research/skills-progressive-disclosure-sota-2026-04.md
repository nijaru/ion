---
date: 2026-04-05
summary: Polished synthesis of skill formats, progressive disclosure, meta-skills, skill routing, and supply chain security.
status: active
---

# Skills & Progressive Disclosure — Synthesis

**Date:** 2026-04-05
**Sources:** agentskills.io Spec, Claude Code, Google ADK, Hermes Agent, SkillRouter (arxiv:2603.22455), SkillsBench (arxiv:2602.12670), SKILL0 (arxiv:2604.02268), SoK: Agentic Skills (arxiv:2602.20867).

## Answer First

**Progressive disclosure is the industry standard for agent knowledge.** The open `agentskills.io` specification (YAML frontmatter + Markdown body) has won the ecosystem, adopted by 40+ products. It uses a three-tier loading model (L1 metadata → L2 instructions → L3 resources) to save ~90% of baseline context tokens. Furthermore, "Self-Extending Agents" (meta-skills that write new skills based on experience) and Body-Aware Skill Routing are the current frontiers. Canto natively supports the SKILL.md format and progressive disclosure, but it needs an intelligent routing layer for large skill libraries and robust supply-chain security hooks to prevent malicious skill execution.

---

## 1. SOTA Landscape — Academic & Industry Research (2026)

| Concept/Paper | Key Contribution | Relevance to Canto |
|---------------|------------------|--------------------|
| **agentskills.io Spec** | Standardized `SKILL.md` format. Frontmatter (name, description, allowed-tools) + Markdown body. | Critical. Canto already uses this; we must strictly adhere to the spec validation rules. |
| **SkillRouter (Mar 2026)** | Skill routing at scale (80K skills). Found that skill *body* (not just metadata) is decisive for accurate routing (29-44pp drop without it). | High. Canto needs an interface for intelligent skill retrieval, as L1-only routing scales poorly. |
| **SkillsBench (Feb 2026)** | Curated skills improve pass rates by 16.2pp. Self-generated skills offer *zero* benefit without validation. Focused skills (2-3 modules) beat comprehensive docs. | Medium. Product policy: Ion must validate self-generated skills before committing them to the registry. |
| **SKILL0 (Apr 2026)** | RL-based skill internalization. Agents learn to operate without runtime retrieval of the skill text. | Low for Canto framework, but represents the long-term future of model training vs. runtime context. |
| **SoK: Agentic Skills** | Comprehensive taxonomy of skill lifecycle. Highlights ClawHavoc supply chain attack (1,200 malicious skills). | High. Validation of the need for trust-tiered execution and strict `allowed-tools` policies in SKILL.md. |

---

## 2. Production Systems Summary

### Claude Code & Progressive Disclosure
- **Tier 1 (L1):** Name and description loaded into the system prompt at startup (~80 tokens/skill).
- **Tier 2 (L2):** Full `SKILL.md` body loaded via a tool (e.g., `read_skill`) when the LLM decides it needs it based on the L1 description.
- **Tier 3 (L3):** Referenced files (`scripts/`, `references/`) loaded on demand.
- **Subagent Preloading:** Claude Code supports preloading specific skills directly into a subagent's system prompt to save tool-calling turns.

### Google ADK & Hermes (Self-Extension)
- **Google ADK (Pattern 4):** Uses a "Meta Skill" (a skill-creator) that teaches the agent how to generate new `SKILL.md` files that follow the spec.
- **Hermes:** Employs a closed learning loop. After complex workflows, the agent uses a `skill_manage` tool to codify its experience into a new skill, which is hot-reloaded into the registry for future sessions.

---

## 3. Convergent Architecture

The modern skill system treats skills as **Agent Knowledge**, distinct from MCP (which is Agent-to-Tool connectivity).

```
┌────────────────────────────────────────────────────────┐
│                   Skill Registry                       │
│  Scans multiple paths (Global, Project, Personal)      │
└───────────────────────┬────────────────────────────────┘
                        │
                        ▼
┌────────────────────────────────────────────────────────┐
│                   Progressive Disclosure               │
│  Inject L1 Metadata into System Prompt                 │
│  Expose `read_skill(name)` tool to LLM                 │
└───────────────────────┬────────────────────────────────┘
                        │ (LLM calls read_skill)
                        ▼
┌────────────────────────────────────────────────────────┐
│                   L2 Instruction Load                  │
│  Fetch SKILL.md body. Apply `allowed-tools` security.  │
└───────────────────────┬────────────────────────────────┘
                        │
                        ▼
┌────────────────────────────────────────────────────────┐
│                   Self-Extension Loop                  │
│  Expose `manage_skill(create/update)` tool. Validate   │
│  against spec. Hot-reload registry.                    │
└────────────────────────────────────────────────────────┘
```

---

## 4. Canto vs. Ion Split

### Principle: Canto provides the spec-compliant registry and loader; Ion provides the marketplace and self-extension nudges.

### What Belongs in Canto (Framework)

| Component | Package | Description | Priority |
|-----------|---------|-------------|----------|
| **Registry & Loader** | `skill/` | Spec-compliant SKILL.md parser, L1 prompt generator, and L2/L3 file loaders. (Mostly exists via `agentskills` library). | P0 |
| **Intelligent Router** | `skill/` | Interface for body-aware skill retrieval (like SkillRouter) when the registry exceeds the L1 token budget. | P1 |
| **`manage_skill` Tool** | `skill/` | Framework tool allowing the agent to create/update skills, writing them to disk and hot-reloading the registry. | P1 |
| **Skill Preloader** | `agent/` | Ability to explicitly preload specified skills into a subagent's context on spawn (avoiding the L1->L2 hop). | P1 |
| **Skill Security Hooks** | `skill/` | Validate signatures/sources of 3rd-party skills to prevent supply-chain attacks (ClawHavoc). | P2 |

### What Belongs in Ion (Product)

| Concern | Description |
|---------|-------------|
| **Self-Extension Nudges** | System prompts that instruct the agent to use `manage_skill` after successfully solving a novel problem. |
| **Marketplace Integration** | CLI commands (`ion skill install`) to fetch skills from awesome-claude-skills or skills.sh. |
| **Trust Policy** | Defining which directories (e.g., global vs project) are trusted, and warning the user if a project contains unverified skills. |

---

## 5. Target Canto Skill Architecture

```go
// package skill

// Registry manages the discovery and progressive disclosure of skills.
type Registry interface {
    // ListL1 returns the name and description of all (or relevant) skills.
    ListL1(ctx context.Context, query string) ([]SkillMetadata, error)

    // LoadL2 fetches the full markdown body of a skill.
    LoadL2(ctx context.Context, name string) (*Skill, error)

    // Manage creates or updates a skill on disk and hot-reloads the registry.
    Manage(ctx context.Context, action, name, content string) error
}

// Skill represents a parsed SKILL.md file adhering to agentskills.io
type Skill struct {
    Name         string
    Description  string
    AllowedTools []string
    Body         string
    // ... other frontmatter fields
}

// Router provides intelligent subset selection for massive skill libraries.
type Router interface {
    SelectRelevant(ctx context.Context, taskDescription string, limit int) ([]SkillMetadata, error)
}
```

---

## 6. Open Research Questions

1. **How do we handle L1 token budgets?** If a user installs 200 skills, injecting 200 L1 metadata entries (~20,000 tokens) into the system prompt is wasteful. Canto needs a default `skill.Router` implementation (perhaps using local embeddings or BM25) to subset the L1 list.
2. **Security of `allowed-tools`:** The spec defines an `allowed-tools` field. If a skill says `allowed-tools: ["read_file"]`, does Canto's tool execution layer dynamically restrict the agent's tool access *only* while that skill is active? This requires tight coupling between `skill/` and `safety/`.

---

## Sources
- agentskills.io Specification
- SkillRouter (arxiv:2603.22455)
- SkillsBench (arxiv:2602.12670)
- SoK: Agentic Skills (arxiv:2602.20867)
- Google ADK Meta Skills Patterns
- Hermes Agent Closed Learning Loop

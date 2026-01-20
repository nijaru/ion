# Positioning and Differentiation

## Core Positioning

### One-Liner

**"The local-first memory layer for AI agents."**

### Elevator Pitch

> aircher is a memory-first framework for AI agents. Unlike cloud-based solutions like Mem0 or server-based systems like Letta, aircher runs entirely on your machine. Your code and context never leave your control. Start with our coding agent, or build your own agents on our framework.

### Full Value Proposition

**For developers building AI agents** who need persistent memory across sessions, **aircher** is a memory framework that is **local-first and embeddable**. Unlike **Letta** which requires a server, or **Mem0** which is cloud-only, aircher **runs as a single process with zero dependencies**.

## Differentiation Matrix

### vs. Letta

| Aspect       | Letta         | aircher              |
| ------------ | ------------- | -------------------- |
| Architecture | Client-server | Single process       |
| Storage      | PostgreSQL    | SQLite + OmenDB      |
| Deployment   | Run a server  | Import a library     |
| Dependencies | Many          | Zero (single binary) |
| Self-hosted  | Complex       | Trivial              |
| Memory model | Comprehensive | Comprehensive        |
| Maturity     | Production    | Early                |

**When to choose Letta:** Multi-tenant, cloud deployment, production workloads
**When to choose aircher:** Local-first, privacy-focused, embedded use cases

### vs. Mem0

| Aspect       | Mem0                | aircher          |
| ------------ | ------------------- | ---------------- |
| Architecture | Cloud API           | Local library    |
| Privacy      | Data leaves machine | Data stays local |
| Self-hosted  | No                  | Yes              |
| Offline      | No                  | Yes              |
| Control      | Limited             | Full             |
| DX           | Great               | Great            |
| Maturity     | Production          | Early            |

**When to choose Mem0:** Quick integration, no ops burden, cloud-native
**When to choose aircher:** Privacy required, offline needed, full control

### vs. LangChain/LangGraph

| Aspect         | LangChain         | aircher      |
| -------------- | ----------------- | ------------ |
| Focus          | Orchestration     | Memory       |
| Memory         | Bolted on         | First-class  |
| Complexity     | High              | Low          |
| Learning curve | Steep             | Gentle       |
| Use case       | Complex pipelines | Agent memory |

**When to choose LangChain:** Complex multi-step workflows, existing ecosystem
**When to choose aircher:** Memory-focused agents, simpler architecture

### vs. Building Custom

| Aspect           | Custom         | aircher           |
| ---------------- | -------------- | ----------------- |
| Development time | Weeks-months   | Hours             |
| Maintenance      | You            | Us                |
| Memory types     | What you build | 10 types included |
| Context assembly | Manual         | Built-in          |
| Testing          | From scratch   | Battle-tested     |

**When to build custom:** Unique requirements, full control
**When to choose aircher:** Standard memory needs, faster time-to-value

## Key Differentiators

### 1. Local-First by Design

**What:** All data stays on your machine by default.

**Why it matters:**

- Privacy: Code never leaves your control
- Compliance: No cloud dependency for regulated industries
- Speed: No network latency for memory operations
- Offline: Works without internet (except LLM calls)

**Proof point:** Single binary distribution, SQLite + OmenDB storage

### 2. Zero Dependencies

**What:** Ships as a single binary or importable library.

**Why it matters:**

- No PostgreSQL to run
- No Docker compose
- No cloud services
- Works immediately after install

**Proof point:** `bun build --compile` produces single executable

### 3. Comprehensive Memory Model

**What:** 10 memory types covering all agent needs.

| Type          | Purpose             |
| ------------- | ------------------- |
| Working       | Current focus       |
| Episodic      | What happened       |
| Semantic      | What things are     |
| Procedural    | What works          |
| Relational    | How things connect  |
| Contextual    | Semantic similarity |
| Prospective   | What to do          |
| User          | Who we're helping   |
| Meta          | What we can do      |
| Environmental | Where we are        |

**Why it matters:** Complete memory solution, not just vectors or events.

### 4. Smart Context Assembly

**What:** Automatically builds optimal prompts from memory.

**Why it matters:**

- Better context = better responses
- Handles token budgets
- Combines temporal + semantic + relational
- Domain-specific strategies

**Proof point:** ContextBuilder with configurable strategies

### 5. Archetype Pattern

**What:** Domain-specific memory configurations.

**Why it matters:**

- Coding agents, support agents, research agents all need different memory
- Framework provides primitives
- Archetypes provide domain semantics
- Easy to create new archetypes

**Proof point:** Coding agent archetype included

### 6. Git-Native Sync (Teams)

**What:** Team memory syncs through git, not cloud.

**Why it matters:**

- Uses existing workflow
- No new cloud service
- Privacy-preserving
- Version controlled

**Proof point:** Events export to JSONL, rebuild vectors locally

## Messaging Framework

### For Individuals

**Headline:** "Your AI agent that actually remembers"
**Subhead:** "Local-first memory that learns from every session"
**CTA:** "Get started in 60 seconds"

### For Teams

**Headline:** "Team knowledge that scales with your codebase"
**Subhead:** "Shared AI memory, synced through git"
**CTA:** "Start your free trial"

### For Enterprise

**Headline:** "AI agent memory that meets compliance requirements"
**Subhead:** "On-premise deployment, full data control"
**CTA:** "Talk to sales"

## Objection Handling

### "Why not just use Letta?"

> Letta is great if you want a full platform. aircher is for when you want memory as a library. No server to run, no PostgreSQL, just import and go. If your needs grow, you can always migrate to Letta later.

### "Why not just use Mem0?"

> Mem0 is excellent if you're comfortable with cloud. aircher is for when privacy matters. Your code and context stay on your machine. Same great DX, but you're in control.

### "Why not just use LangChain memory?"

> LangChain's memory is powerful but bolted on. aircher is memory-first. Ten memory types, smart context assembly, domain-specific archetypes. If you're building an agent that needs to learn over time, aircher is purpose-built for that.

### "This is too early/immature"

> Fair point. We're pre-1.0. But the architecture is proven (SQLite + vectors), the memory model is comprehensive, and the framework is testable. Early adopters get to shape the roadmap.

### "How do I know you'll be around?"

> We're building in public, open source first. Even if we disappeared, you'd have the code. That said, we're committed to this space and building a business around it.

## Brand Values

1. **Simple over complex** - Two layers beats five layers
2. **Local over cloud** - Your data, your control
3. **Honest over hype** - Real capabilities, clear limitations
4. **Developer-first** - Great DX, not enterprise features
5. **Open over closed** - Open source core, always

## Content Strategy

### Manifesto (to write)

**"Why Local-First AI Agents"**

- The case for keeping data local
- Privacy, speed, control
- Why server-based is overkill for most use cases

### Technical Deep-Dives

1. "How We Built a 10-Type Memory System"
2. "Context Assembly: The Hard Part of Agent Memory"
3. "Archetypes: Domain-Specific Memory for AI Agents"
4. "Git-Native Sync: Team Memory Without Cloud"

### Comparison Posts

1. "aircher vs Letta: When to Use Which"
2. "aircher vs Mem0: Local vs Cloud Memory"
3. "Building Agent Memory from Scratch vs Using a Framework"

### Tutorials

1. "Building a Coding Agent with aircher in 30 Minutes"
2. "Adding Memory to Your Existing Agent"
3. "Creating Custom Archetypes"
4. "Team Memory Setup with Git Sync"

## References

- [STRATEGY.md](STRATEGY.md) - Business strategy
- [MARKET.md](MARKET.md) - Competitive analysis
- [MONETIZATION.md](MONETIZATION.md) - Pricing

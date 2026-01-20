# Business Strategy

## Vision

**Build the memory layer that powers the next generation of AI agents.**

## Core Thesis

Memory is the hard problem in AI agents. Agent loops are commoditized (Vercel AI SDK, LangChain, etc.). What's valuable is:

1. Remembering what happened across sessions
2. Finding relevant context for prompts
3. Learning and improving over time
4. Simple, local-first architecture

## Product Strategy

### What We're Building

**aircher** - A memory-first framework for AI agents with a coding agent archetype.

```
aircher/
├── src/
│   ├── memory/          # THE CORE PRODUCT
│   │   ├── episodic.ts  # What happened (SQLite)
│   │   ├── vector.ts    # Semantic search (OmenDB)
│   │   ├── entity.ts    # What we know
│   │   ├── relation.ts  # How things connect
│   │   └── context.ts   # Smart assembly
│   ├── agent/           # Agent primitives
│   └── archetypes/
│       └── code/        # Reference implementation
```

### Product Tiers

| Tier       | Target          | Features                       | Pricing           |
| ---------- | --------------- | ------------------------------ | ----------------- |
| OSS        | Individual devs | Full framework, CLI            | Free (Apache 2.0) |
| Teams      | Small teams     | Git-native sync, shared memory | $15-25/seat/month |
| Enterprise | Large orgs      | On-prem, SSO, compliance       | Custom            |

## Strategic Options

### Option A: Pure Open Source

**Model:** Everything open source, monetize via support/consulting

**Pros:**

- Maximum adoption
- Community contributions
- No sales complexity

**Cons:**

- Revenue ceiling (~$1-5M ARR)
- Hard to build a team
- Consulting doesn't scale

**Best for:** Side project, passion project, research

### Option B: Open Core

**Model:** Core open source, premium features paid

**Pros:**

- Adoption + revenue
- Clear upgrade path
- Proven model (GitLab, Supabase)

**Cons:**

- Feature boundary tension
- Community expectations
- Dual codebase complexity

**Best for:** Venture-scale business

### Option C: Source Available

**Model:** Code visible but commercial use requires license

**Pros:**

- Transparency
- Protection from cloud providers
- Simpler than open core

**Cons:**

- Less community adoption
- License confusion
- "Not really open source"

**Best for:** Developer tools, infrastructure

## Recommended Strategy

**Open Core with Local-First Differentiation**

1. **Core (Apache 2.0):**
   - Memory framework (all 10 memory types)
   - Agent primitives
   - Coding agent archetype
   - CLI tool
   - Single-user, local storage

2. **Teams (Paid):**
   - Git-native sync for team memory
   - Shared context across developers
   - Analytics dashboard
   - Priority support

3. **Enterprise (Custom):**
   - On-premise deployment
   - SSO/SAML integration
   - Audit logging
   - Compliance certifications
   - Dedicated support

## Go-to-Market Phases

### Phase 1: Developer Adoption (Months 1-6)

**Goal:** 1,000+ GitHub stars, active community

**Activities:**

- Open source release
- "Why Local-First AI Agents" manifesto
- Technical blog posts
- Conference talks
- Discord community

**Metrics:**

- GitHub stars
- npm downloads
- Discord members
- Contributors

### Phase 2: Teams (Months 6-12)

**Goal:** 50+ paying teams

**Activities:**

- Launch Teams tier
- Design partner program
- Case studies
- Developer advocates

**Metrics:**

- Teams customers
- MRR
- NPS
- Churn

### Phase 3: Enterprise (Year 2+)

**Goal:** $1M+ ARR from enterprise

**Activities:**

- Enterprise sales team
- SOC 2 compliance
- On-prem offering
- Partner channel

**Metrics:**

- Enterprise customers
- ACV
- Sales cycle length
- Win rate

## Funding Considerations

### Bootstrap Path

- Focus on profitability over growth
- Teams tier provides revenue
- Consulting fills gaps
- Slower but sustainable
- Keep 100% ownership

### Venture Path

- Requires cloud component for scale
- Need usage-based pricing
- Target: $100M+ ARR potential
- Dilution in exchange for speed
- Typical: Seed → Series A → B

### Decision Factors

| Factor   | Bootstrap  | Venture              |
| -------- | ---------- | -------------------- |
| Timeline | 5-10 years | 2-3 years to scale   |
| Control  | Full       | Board oversight      |
| Risk     | Lower      | Higher               |
| Upside   | Moderate   | High (if successful) |
| Stress   | Moderate   | High                 |

## Risks and Mitigations

| Risk                     | Likelihood | Impact | Mitigation                 |
| ------------------------ | ---------- | ------ | -------------------------- |
| Big player adds memory   | High       | High   | Move fast, build community |
| Memory becomes commodity | Medium     | High   | Focus on DX, integration   |
| Can't monetize OSS       | Medium     | High   | Compelling Teams features  |
| Competition outpaces     | High       | Medium | Focus on wedge             |

## Key Decisions Needed

1. **License choice:** Apache 2.0 vs MIT vs BSL?
2. **Funding:** Bootstrap vs raise?
3. **Team:** Solo vs co-founders?
4. **Timeline:** When to launch Teams tier?

## References

- [MARKET.md](MARKET.md) - Competitive landscape
- [MONETIZATION.md](MONETIZATION.md) - Revenue details
- [POSITIONING.md](POSITIONING.md) - Differentiation

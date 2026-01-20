# Market Analysis

## Market Segments

### 1. AI Coding Agents

**Market size:** ~$10B+ by 2027 (fast growing)

| Player         | Model      | Funding   | Strength     | Weakness        |
| -------------- | ---------- | --------- | ------------ | --------------- |
| Cursor         | IDE-native | $400M+    | UX, speed    | Proprietary     |
| GitHub Copilot | VS Code    | Microsoft | Distribution | Basic memory    |
| Cody           | Enterprise | $225M     | Code search  | Complex         |
| Aider          | CLI/OSS    | Bootstrap | Simple       | No memory       |
| Claude Code    | CLI        | Anthropic | Model access | No persistence  |
| letta-code     | Server     | Letta     | Memory       | Server required |

**Opportunity:** Memory-first agent that learns over time.

### 2. Agent Frameworks

**Market size:** ~$1B+ developer tools

| Framework     | Focus         | Funding | Strength  | Weakness                  |
| ------------- | ------------- | ------- | --------- | ------------------------- |
| LangChain     | Orchestration | $25M+   | Ecosystem | Complex, memory bolted on |
| LlamaIndex    | RAG           | $30M+   | RAG focus | Not agent memory          |
| CrewAI        | Multi-agent   | $18M    | Growing   | Basic memory              |
| Mastra        | TypeScript    | Early   | Modern    | New, unproven             |
| Vercel AI SDK | Streaming     | Vercel  | DX        | No memory                 |

**Opportunity:** Memory-first framework, not orchestration-first.

### 3. Agent Memory

**Market size:** Emerging (~$500M+)

| Solution  | Architecture        | Funding   | Strength        | Weakness        |
| --------- | ------------------- | --------- | --------------- | --------------- |
| Letta     | Server + PostgreSQL | $10M+     | Comprehensive   | Requires server |
| Mem0      | Cloud API           | $10M+     | Simple API      | Cloud-only      |
| Zep       | Hybrid              | Seed      | Fact extraction | Less mature     |
| LangGraph | State machines      | LangChain | Powerful        | Complex         |

**Opportunity:** Local-first, simple, embeddable memory.

### 4. Vector Databases

**Market size:** ~$2B+ by 2027

| Database | Architecture  | Strength      | Weakness         |
| -------- | ------------- | ------------- | ---------------- |
| Pinecone | Cloud         | Managed       | Cloud-only       |
| Weaviate | Hybrid        | Hybrid search | Complex          |
| Qdrant   | Rust          | Performance   | Ops overhead     |
| Chroma   | Embedded      | Simple        | Limited features |
| OmenDB   | Embedded Rust | Fast, local   | New              |

**Opportunity:** Full memory layer, not just vectors.

## Competitive Positioning

### Competitor Matrix

|             | Letta |  Mem0   |   Zep   | LangChain | **aircher** |
| ----------- | :---: | :-----: | :-----: | :-------: | :---------: |
| Local-first |  No   |   No    | Partial |    No     |   **Yes**   |
| No server   |  No   |   No    |   No    |    No     |   **Yes**   |
| Embeddable  |  No   |   No    |   No    |  Partial  |   **Yes**   |
| Simple      |  No   |   Yes   | Partial |    No     |   **Yes**   |
| Full memory |  Yes  | Partial | Partial |    No     |   **Yes**   |
| OSS         |  Yes  | Partial |   Yes   |    Yes    |   **Yes**   |

### Detailed Competitor Analysis

#### Letta (formerly MemGPT)

**Background:**

- Spun out of Berkeley research
- $10M+ funding (YC, a16z)
- Team of ~10 engineers

**Architecture:**

```
Client (letta-code) → HTTP → Letta Server → PostgreSQL
                                    ↓
                              LLM Provider
```

**Memory Model:**

- Core memory (always in context)
- Archival memory (searchable)
- Recall memory (conversation)

**Strengths:**

- Research pedigree
- Comprehensive memory model
- Well-funded

**Weaknesses:**

- Requires running a server
- PostgreSQL dependency
- Complex for simple use cases
- Operational overhead

**How we differentiate:**

- No server required
- Single binary distribution
- Simpler architecture
- Local-first by design

#### Mem0

**Background:**

- Founded 2023
- $10M+ raised
- Focus on memory API

**Architecture:**

```
Your Agent → HTTP → Mem0 Cloud → Storage
```

**Strengths:**

- Simple API
- Great DX
- Fast

**Weaknesses:**

- Cloud-only
- No self-hosted option
- Privacy concerns
- Vendor lock-in

**How we differentiate:**

- Local-first (your data stays local)
- Self-hosted always
- No vendor dependency
- Full control

#### Zep

**Background:**

- Smaller team
- Open source + cloud
- Focus on fact extraction

**Architecture:**

- Self-hosted option
- Hybrid search
- Fact extraction

**Strengths:**

- Fact extraction is clever
- Self-hosted available
- Hybrid search

**Weaknesses:**

- Smaller community
- Less mature
- Limited mindshare

**How we differentiate:**

- Simpler architecture
- Better DX
- More comprehensive memory types

## Market Gaps

### What Nobody Has

| Need               | Current State          | Our Opportunity           |
| ------------------ | ---------------------- | ------------------------- |
| Local-first memory | Server or cloud        | Single process            |
| Zero dependencies  | PostgreSQL, Redis      | SQLite + embedded vectors |
| Embeddable library | Requires API calls     | Import as module          |
| Simple model       | Complex state machines | 2-layer core              |
| Privacy-first      | Cloud default          | Local default             |
| Git-native sync    | Cloud sync             | Sync via git              |

### Underserved Segments

1. **Privacy-conscious developers**
   - Don't want code going to cloud
   - Prefer local-first tools
   - Willing to pay for privacy

2. **Regulated industries**
   - Finance, healthcare, government
   - Compliance requirements
   - On-prem mandates

3. **Small teams**
   - Can't afford enterprise tools
   - Need shared context
   - Value simplicity

4. **Open source projects**
   - Can't use cloud services
   - Need self-hosted
   - Community-driven

## Market Trends

### Tailwinds

1. **Privacy regulation increasing**
   - EU AI Act
   - GDPR enforcement
   - Enterprise compliance

2. **Local-first movement**
   - Linear, Figma proving local-first can win
   - Developers want control
   - Offline-capable is valued

3. **AI agent explosion**
   - Every company building agents
   - Memory is universal need
   - Framework fatigue setting in

4. **Open source success**
   - Supabase, Vercel proving OSS works
   - Developers prefer OSS
   - Community is distribution

### Headwinds

1. **Big player consolidation**
   - OpenAI, Anthropic adding features
   - Cloud providers bundling
   - Memory might become built-in

2. **Framework fatigue**
   - Too many options
   - Developers hesitant to adopt
   - Switching costs

3. **Commoditization risk**
   - Memory could become commodity
   - Low barriers to copy
   - Price pressure

## Target Market

### Primary: Individual Developers

**Size:** 30M+ developers globally
**Segment:** Power users of AI coding tools

**Characteristics:**

- Use CLI tools (Aider, Claude Code)
- Care about privacy
- Value simplicity
- Early adopters

**Why they'd use aircher:**

- Better memory than alternatives
- Local-first
- Open source
- Simple to understand

### Secondary: Small Teams

**Size:** 1M+ development teams
**Segment:** Teams of 5-20 developers

**Characteristics:**

- Need shared context
- Want team memory
- Can pay for tools
- Value collaboration

**Why they'd use aircher:**

- Shared team memory
- Git-native sync
- Affordable
- Self-hosted option

### Tertiary: Enterprise

**Size:** 10K+ enterprise engineering orgs
**Segment:** Large organizations

**Characteristics:**

- Compliance requirements
- Budget for tools
- Long sales cycles
- Need support

**Why they'd use aircher:**

- On-premise deployment
- Compliance-ready
- Enterprise support
- Audit logging

## References

- [POSITIONING.md](POSITIONING.md) - How we differentiate
- [MONETIZATION.md](MONETIZATION.md) - Revenue model
- [STRATEGY.md](STRATEGY.md) - Overall strategy

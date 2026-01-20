# Research Directory

> Organized research for AI agent development. Topic-based structure for easy navigation.

## Quick Reference

| Task             | Directory     | Start With                  |
| ---------------- | ------------- | --------------------------- |
| Context patterns | context/      | engineering.md              |
| Stack-based ctx  | context/      | stack-patterns.md           |
| Competitor intel | competitive/  | analysis-2025.md            |
| Agent patterns   | architecture/ | agent-scaffolding.md        |
| Tool integration | reference/    | tools-strategy.md           |
| Historical ref   | \_archive/    | (Python-era, concepts only) |

---

## context/

Context engineering, memory systems, and attention management.

| File              | Content                                     |
| ----------------- | ------------------------------------------- |
| engineering.md    | Context rot, ACE, decision traces, Zep      |
| stack-patterns.md | ContextBranch, THREAD, bi-temporal, Factory |

**Key insights:**

- Shuffled > logical organization (counterintuitive)
- Delta updates +17% accuracy, -83% cost (ACE)
- Claude lowest hallucination, abstains when uncertain

---

## competitive/

SOTA agent analysis with evidence levels.

| File             | Content                            |
| ---------------- | ---------------------------------- |
| analysis-2025.md | Comprehensive with evidence levels |
| claude-code.md   | Anthropic patterns (43.2% T-Bench) |
| codex-cli.md     | OpenAI patterns                    |
| factory-droid.md | #2 T-Bench (58.8%), spec-first     |
| tui-agents.md    | Terminal agent landscape           |

**Key benchmarks:**

- Terminal-Bench SOTA: Ante 60.3%, Factory 58.8%, Claude 43.2%
- SWE-bench SOTA: Grok 4 75%, GPT-5 74.9%, Claude Opus 74.5%

---

## architecture/

Agent design patterns and scaffolding.

| File                 | Content                          |
| -------------------- | -------------------------------- |
| agent-scaffolding.md | LM-centric interfaces, SWE-Agent |
| subagent-patterns.md | Sub-agent coordination (CRUSH)   |
| adaptive-spec.md     | Memory-informed spec generation  |

---

## reference/

Tools, protocols, and implementation guides.

| File                     | Content                       |
| ------------------------ | ----------------------------- |
| tools-strategy.md        | Tool bundling, fallbacks      |
| tool-calling.md          | Reality check on tool calling |
| acp-integration.md       | ACP protocol details          |
| lsp-landscape.md         | LSP tooling options           |
| codebase-intelligence.md | Code analysis approaches      |
| benchmarking.md          | Terminal-Bench, SWE-bench     |
| discoveries.md           | Key research insights         |

---

## \_archive/

Python-era research. Concepts valid, implementation outdated.

| File                     | Original Purpose               |
| ------------------------ | ------------------------------ |
| memory-system-python.md  | DuckDB-era memory design       |
| distribution-python.md   | Python/Mojo distribution       |
| rust-vs-python.md        | Language decision (historical) |
| v14-improvements.md      | Pre-Bun improvements           |
| week8-validation.md      | Old integration testing        |
| codex-roadmap.md         | Python-era roadmap             |
| sota-research-nov2025.md | Recovery report + trends       |

---

## Current Stack

| Component | Choice     | Research Source                 |
| --------- | ---------- | ------------------------------- |
| Database  | bun:sqlite | context/engineering.md          |
| Vectors   | OmenDB     | reference/codebase-intelligence |
| Graph     | Graphology | architecture/subagent-patterns  |
| Agent     | AI SDK v5  | architecture/agent-scaffolding  |
| Runtime   | Bun        | competitive/analysis-2025       |

---

## Research Validated (No Change Needed)

From context/engineering.md:

- Budget-based context assembly
- Append-only episodic store
- Outcome tracking (success/failure)
- Artifacts section for files
- Bi-temporal schema

## Priority Additions (Backlog)

From recent research synthesis:

| Change                   | Source             | Priority |
| ------------------------ | ------------------ | -------- |
| Helpful/harmful counters | ACE paper          | High     |
| Query-needle similarity  | Chroma context rot | High     |
| Tool masking             | Manus/Momo         | Medium   |
| Decision event type      | Foundation Capital | Medium   |

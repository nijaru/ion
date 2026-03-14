# ion ↔ canto

## What Is canto?

[canto](https://github.com/nijaru/canto) is a composable Go framework for building LLM agents and agent swarms. It provides general-purpose primitives: provider-agnostic LLM streaming, durable session logging, agent loop, tool execution, context compaction, and memory — usable by any agent or application.

## Relationship

ion is a TUI coding agent. canto is a general agent framework ion plans to adopt.

ion's `NativeIonSession` will become a thin adapter over canto's runtime once canto reaches a stable Phase 1. canto is designed for the general case — ion adapts to canto's APIs, not the other way around.

```
ion (TUI + UX layer)
  └── canto (agent framework)
        ├── llm/       provider-agnostic streaming
        ├── agent/     perceive → decide → act → observe loop
        ├── session/   durable JSONL/SQLite event log
        ├── context/   compaction, KV-cache helpers
        ├── tool/      execution, registry, MCP
        ├── skill/     progressive disclosure packages
        ├── runtime/   session execution, lane queue, heartbeat
        └── memory/    in-context + external, SQLite-backed
```

## Shared Research

ion's `ai/` research feeds directly into canto's design. When significant research is done in ion that applies to canto's layers, copy the relevant files:

```bash
cp ai/research/<file>.md ../canto/ai/research/
cp ai/design/<file>.md   ../canto/ai/design/
```

Files already copied to canto (as of 2026-03-14):

**research/**

- `sota-agent-patterns-2026.md`
- `session-storage-patterns-2026.md`
- `compaction-techniques-2026.md`
- `letta-memory-systems.md`
- `tool-architecture-survey-2026-02.md`
- `prompt-caching-providers-2026.md`
- `subagent-best-practices.md`
- `coding-agents-state-2026-02.md`
- `context-management.md`
- `extensibility-systems-2026.md`
- `model-routing-for-subagents.md`

**design/**

- `memory-and-context.md`
- `session-interface.md`
- `native-ion-agent.md`
- `subagents-swarms-rlm.md`
- `plugin-architecture.md`
- `tool-pass.md`

## Development Order

canto Phase 1 (in progress):

1. `llm/` — Provider interface, streaming, cost tracking
2. `session/` — Event log, JSONL store
3. `agent/` — Agent loop, turn execution
4. `tool/` + `runtime/` — Tool interface, registry, runner

Once Phase 1 gates pass, ion can wire `NativeIonSession` to canto's runtime. Until then, ion's `internal/` implementations stand independently.

## Cross-Project Workflow

- New agent/session/memory research done in ion → copy to `../canto/ai/research/`
- canto API changes that affect ion → update `internal/` accordingly
- Keep ion's `AgentSession` interface aligned with canto's session contract

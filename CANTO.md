# ion ↔ canto

## What Is canto?

[canto](https://github.com/nijaru/canto) is a composable Go framework for building LLM agents and agent swarms. It extracts the agent, session, memory, context, and tool infrastructure into a reusable library — the pieces ion needs under the hood.

## Relationship

ion is a TUI coding agent. canto is the framework it will eventually run on.

ion is being rewritten in Go at the same time canto is being built. The plan is for ion's `NativeIonSession` to become a thin wrapper over canto's runtime once canto reaches a stable Phase 1.

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

canto Phase 1 (in progress) delivers the minimal viable stack ion needs:

1. `llm/` — Provider interface, streaming, cost tracking
2. `session/` — Event log, JSONL store
3. `agent/` — Agent loop, turn execution
4. `tool/` + `runtime/` — Tool interface, registry, runner

Once Phase 1 gates pass (`go test ./...`), ion's `NativeIonSession` can be wired to canto's runtime in place of its current internal implementation.

## Cross-Project Workflow

- New agent/session/memory research done in ion → copy to `../canto/ai/research/`
- canto API changes that affect ion → update `internal/` accordingly
- Keep ion's `AgentSession` interface aligned with canto's session contract

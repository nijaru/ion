# Canto → Ion: Memory & Tracing Decisions

**Date:** 2026-03-21  
**Context:** Ion's `tk-61t4` task proposed promoting FTS5 knowledge tables and tracing fields into Canto's core. This note records what was actually landed and how Ion should integrate it.

---

## What landed in Canto (use this, not Ion's local schema)

### 1. `memory.CoreStore` — Knowledge API (replaces Ion's local `knowledge_fts`)

`CoreStore.SaveKnowledge` / `CoreStore.SearchKnowledge` are now part of Canto's `memory` package. Ion's local `KnowledgeItem` schema and `knowledge_fts` table should migrate to these.

```go
// Save
item := &memory.KnowledgeItem{
    ID:        hashOf(content),
    SessionID: sess.ID(),
    Content:   content,
    Metadata:  map[string]any{"source": "workspace"},
}
store.SaveKnowledge(ctx, item)

// Search
results, _ := store.SearchKnowledge(ctx, "FTS5 query", 5)
```

### 2. `x/tools.MemorizeKnowledgeTool` + `RecallKnowledgeTool`

Drop-in replacements for Ion's local `recall` / `memorize` tools. Wire them to the shared `CoreStore` instance:

```go
reg.Register(&tools.MemorizeKnowledgeTool{Store: coreStore, SessionID: sessID})
reg.Register(&tools.RecallKnowledgeTool{Store: coreStore, Limit: 5})
```

### 3. `context.KnowledgeMemory()` processor

Replaces explicit session memory injection. Add to Ion's context builder:

```go
builder := context.NewBuilder(
    context.Instructions(systemPrompt),
    context.KnowledgeMemory(coreStore, "", 5), // "" = use last user message as query
    context.History(),
    context.Tools(reg),
)
```

This injects a `<knowledge_memory>…</knowledge_memory>` block into the system prompt automatically each turn, replacing any hand-rolled memory injection Ion was doing.

---

## What was rejected — and why

### ❌ `knowledge_fts` in `session.SQLiteStore`

Ion's proposal put the knowledge table inside the session store. **Rejected** — `session.SQLiteStore` is strictly an append-only event log. The knowledge store lives in `memory.CoreStore` (separate SQLite file or same file with a separate connection is fine).

### ❌ `AgentID` / `TraceID` columns on `session.Event`

Adds UI routing columns to the lowest data layer. **Rejected.**

---

## How Ion should handle multi-agent stream routing

Two mechanisms work together:

**1. Child session subscription** — each child run gets its own isolated `SessionID`. Ion's TUI already maps events by session; subscribing to `childRef.SessionID` gives a dedicated stream per agent pane.

```go
ref, _ := childRunner.Spawn(ctx, parentSess, runtime.ChildSpec{Agent: subAgent, ...})
ch := childSess.Subscribe(ctx) // tail ref.SessionID events for that pane
```

**2. `session.WithMetadata` for correlation** — Canto now auto-stamps `agent_id` into every event the child appends. Ion can read `event.Metadata["agent_id"]` for any cross-session correlation (logs, analytics) without altering the schema.

```go
ctx = session.WithMetadata(ctx, map[string]any{"trace_id": traceID})
// all events appended under this ctx will carry trace_id in Metadata
```

---

## Ion migration checklist (`tk-61t4`)

- [ ] Remove local `KnowledgeItem` struct and `knowledge_fts` creation from `cantoStore`
- [ ] Remove local `recall` / `memorize` tool implementations
- [ ] Wire `memory.NewCoreStore(dsn)` in Ion's backend setup
- [ ] Register `tools.MemorizeKnowledgeTool` + `tools.RecallKnowledgeTool`
- [ ] Add `context.KnowledgeMemory(coreStore, "", 5)` to the context builder chain
- [ ] For multi-agent TUI: subscribe to `childRef.SessionID` per agent pane
- [ ] Remove `ai/design/memory-fts5-migration.md` (superseded by this note)

# Aircher Roadmap

## Vision

**Local-first memory framework for AI agents.**

General-purpose memory that works for any agent. Archetypes customize for specific domains. Coding archetype is our primary focus.

## Strategy

| Aspect      | Decision                                     |
| ----------- | -------------------------------------------- |
| Core        | Memory framework (open source, MIT)          |
| Primary     | Coding archetype (our focus)                 |
| Ecosystem   | Other archetypes (community/premium)         |
| Positioning | Local-first vs Letta (server) / Mem0 (cloud) |
| Business    | Open core + paid tiers                       |

## Phase Summary

| Phase | Status      | Focus                          |
| ----- | ----------- | ------------------------------ |
| 1-8   | Done/Paused | Prior work (Python, Bun setup) |
| 9     | **Active**  | Memory Framework + Coding      |
| 10    | Future      | Distribution + Polish          |
| 11    | Future      | Ecosystem + Monetization       |

## Completed Work

### Python Era (Phases 1-6)

- 3-layer memory (DuckDB + ChromaDB + KG)
- Model routing, cost tracking
- ACP protocol
- Tagged as `v0.0.1-python`

### Bun Setup (Phase 8, partial)

- Project scaffolding
- Types & Config
- Stack decisions (bun:sqlite, OmenDB, AI SDK v5)

## Phase 9: Core Development (Active)

### 9.1 Memory Framework

| Task              | Priority | Status | Notes                       |
| ----------------- | -------- | ------ | --------------------------- |
| SQLite schema     | P0       | Done   | events, entities, tasks     |
| Episodic memory   | P0       | Done   | Event recording/querying    |
| Semantic memory   | P0       | Done   | Entity management + decay   |
| Task memory       | P0       | Done   | Stack-based management      |
| Context assembly  | P0       | Done   | Budget-based with artifacts |
| Relational memory | P1       | Defer  | Graphology integration (v2) |
| Contextual memory | P1       | Defer  | OmenDB vectors (v2)         |

**Deliverable:** Working memory framework. **128 tests passing.**

### 9.2 Archetype System

| Task                | Priority | Status | Notes               |
| ------------------- | -------- | ------ | ------------------- |
| Archetype interface | P0       | Done   | Types and contracts |
| Entity extractors   | P0       | Done   | Per-event-type      |
| Relation extractors | P1       | Defer  | Cross-entity (v2)   |
| Context strategies  | P0       | Done   | Budget allocation   |
| Archetype registry  | P2       | Done   | Plugin system       |

**Deliverable:** Pluggable archetype system. Done.

### 9.3 Coding Archetype

| Task                   | Priority | Status | Notes                    |
| ---------------------- | -------- | ------ | ------------------------ |
| Event types            | P0       | Done   | file_read, edit, etc.    |
| Code entity extraction | P0       | Done   | Functions, imports, etc. |
| Import parsing         | P0       | Done   | TS/JS, Python, Go, Rust  |
| Agent loop             | P0       | Done   | AI SDK v5 integration    |
| tree-sitter setup      | P1       | Defer  | Multi-language AST (v2)  |
| File tools             | P1       | Defer  | Framework tools (v2)     |
| Shell tools            | P1       | Defer  | Framework tools (v2)     |

**Deliverable:** Working coding archetype. Done.

### 9.4 CLI

| Task               | Priority | Status      | Notes              |
| ------------------ | -------- | ----------- | ------------------ |
| Basic CLI          | P0       | In Progress | Interactive prompt |
| Session management | P0       | In Progress | List, resume       |
| Config loading     | P0       | Done        | Environment + file |

**Deliverable:** Usable CLI for coding tasks. In progress.

### Phase 9 Success Criteria

- [ ] Memory framework works standalone (no archetype)
- [ ] Archetype system is pluggable
- [ ] Coding archetype extracts code entities
- [ ] Context assembly < 50ms
- [ ] Agent completes multi-step tasks
- [ ] Works offline with Ollama

## Phase 10: Distribution + Polish (Future)

| Task          | Description                    |
| ------------- | ------------------------------ |
| Single binary | `bun build --compile`          |
| ACP server    | Editor integration (Toad, Zed) |
| TUI           | Rich terminal interface        |
| Documentation | API docs, guides, examples     |
| npm package   | `npm install aircher`          |
| Website       | Landing page, docs site        |

## Phase 11: Ecosystem + Monetization (Future)

| Task                | Description               |
| ------------------- | ------------------------- |
| Payment integration | Gumroad/Paddle            |
| Team sync           | Git-native memory sharing |
| Premium archetypes  | Research, support, etc.   |
| Marketplace         | Community archetypes      |
| Enterprise features | SSO, audit, compliance    |

## Business Model

### Pricing Structure

| Tier       | Price      | What                           |
| ---------- | ---------- | ------------------------------ |
| Open Core  | Free (MIT) | Framework + coding archetype   |
| Individual | $49 once   | Binary + 1yr updates + support |
| Team       | $199 once  | 5 seats + git sync             |
| Enterprise | Custom     | SSO, audit, compliance, SLA    |

### Add-ons

| Add-on             | Price         |
| ------------------ | ------------- |
| Cloud sync         | $5/user/month |
| Premium archetypes | $29-99 each   |
| Marketplace cut    | 30%           |

### Revenue Path

```
Year 1: Build, early adopters         ~$10K
Year 2: Growing adoption              ~$70K
Year 3: Ecosystem, enterprise         ~$400K+
```

## Dependencies

```
9.1 Memory Framework
        │
        ▼
9.2 Archetype System
        │
        ▼
9.3 Coding Archetype ──► 9.4 CLI
        │
        ▼
   Phase 10 (Distribution)
        │
        ▼
   Phase 11 (Ecosystem)
```

## Development Approach

### Weeks 1-2: Memory Foundation

```
src/memory/db.ts        # SQLite setup
src/memory/episodic.ts  # Event store
src/memory/semantic.ts  # Entity store
src/memory/schema.sql   # Tables
```

### Weeks 3-4: Archetype + Context

```
src/archetype/index.ts  # Archetype interface
src/memory/context.ts   # Context builder
src/memory/relational.ts# Graph store
```

### Weeks 5-6: Coding Archetype

```
src/archetypes/coding/  # Coding archetype
src/code/parser.ts      # tree-sitter
src/agent/loop.ts       # AI SDK
src/agent/tools.ts      # Tools
```

### Weeks 7-8: CLI + Polish

```
src/cli/index.ts        # CLI
tests/                  # Coverage
docs/                   # Documentation
```

## Risk Mitigation

| Risk                   | Mitigation                  |
| ---------------------- | --------------------------- |
| tree-sitter complexity | Start with TS/JS only       |
| OmenDB instability     | Fallback to Orama           |
| Scope creep            | P0 features only for MVP    |
| Framework too generic  | Coding archetype grounds it |
| Market timing          | Ship fast, iterate          |

## Key Documents

- [DESIGN.md](DESIGN.md) - Architecture
- [DECISIONS.md](DECISIONS.md) - Decision records
- [design/](design/) - Technical specs
- [business/](business/) - Strategy reference

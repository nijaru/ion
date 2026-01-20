# Language Decision: Rust vs TypeScript/Bun

**Date**: 2026-01-13
**Status**: Rust (decided, with documented tradeoffs)
**Purpose**: Comprehensive analysis of language choice for aircher TUI agent

## Executive Summary

**Decision**: Rust

**Key factors**:

- OmenDB is Rust-native (no FFI overhead)
- Safety matters for agents executing code
- Models are much better at Rust than 6 months ago
- Codex CLI (MIT) provides reference implementation

**Main tradeoff**: Slower initial development vs. long-term safety and performance.

---

## Detailed Comparison

### Development Velocity

| Factor                   | TypeScript/Bun          | Rust                              |
| ------------------------ | ----------------------- | --------------------------------- |
| Initial scaffolding      | Fast                    | Medium                            |
| Provider implementations | Very fast (AI SDK)      | Medium (custom)                   |
| TUI development          | Medium (ink has issues) | Medium (ratatui mature)           |
| Agent-generated code     | Very reliable           | Reliable (improved significantly) |
| Iteration speed          | Fast                    | Medium                            |
| Debugging                | Easy                    | Harder                            |

**TypeScript wins** on development velocity for the first few weeks.

**Rust catches up** once the foundational traits are in place.

### Provider Ecosystem

| Provider          | TypeScript        | Rust                |
| ----------------- | ----------------- | ------------------- |
| OpenRouter        | AI SDK adapter    | Custom impl needed  |
| Anthropic         | Official SDK      | Official SDK (beta) |
| Google            | Official SDK      | Official SDK        |
| Ollama            | Multiple options  | ollama-rs crate     |
| OpenAI-compatible | AI SDK unified    | Custom impl         |
| Streaming         | Built into AI SDK | reqwest-eventsource |

**TypeScript wins** significantly here. Vercel AI SDK provides unified interface across 30+ providers.

**Rust mitigation**: Most providers use OpenAI-compatible API. One good implementation covers many providers. OpenRouter handles 200+ models.

### Memory Integration (Our Differentiator)

| Factor            | TypeScript             | Rust                         |
| ----------------- | ---------------------- | ---------------------------- |
| OmenDB access     | Via MCP (IPC overhead) | Native crate (zero overhead) |
| SQLite            | bun:sqlite (fast)      | rusqlite (fast)              |
| Concurrent access | Callbacks              | RwLock (explicit)            |
| Memory management | GC                     | Manual (predictable)         |

**Rust wins** here. OmenDB is Rust-native. Using it through MCP adds:

- Process spawn overhead
- JSON-RPC serialization
- IPC latency on every query

For budget-aware context assembly (our key differentiator), this matters.

### Agent Safety

| Factor            | TypeScript              | Rust                        |
| ----------------- | ----------------------- | --------------------------- |
| Type safety       | Runtime errors possible | Compile-time guarantees     |
| Null safety       | Optional chaining       | Option/Result               |
| Tool execution    | Can crash               | Recoverable errors          |
| Sandbox escape    | Possible                | Harder                      |
| Memory corruption | Possible via native     | Prevented by borrow checker |

**Rust wins significantly**. Agents execute untrusted code (user commands, tool results). Rust's safety guarantees matter:

1. A panic in a tool doesn't crash the whole agent
2. Buffer overflows are prevented
3. Use-after-free impossible
4. Thread safety enforced at compile time

### Binary Distribution

| Factor            | TypeScript/Bun  | Rust                 |
| ----------------- | --------------- | -------------------- |
| Binary size       | ~50-80MB        | ~10-15MB             |
| Startup time      | 20-50ms         | <10ms                |
| Dependencies      | Bundled runtime | None                 |
| Cross-compilation | bun build       | cargo build --target |
| Code signing      | Works           | Works                |

**Rust wins** on distribution. Single static binary with no runtime.

### TUI Framework Quality

| Framework | Language   | Stars | Quality      | Async        |
| --------- | ---------- | ----- | ------------ | ------------ |
| ratatui   | Rust       | 10k+  | Excellent    | Native       |
| crossterm | Rust       | 3k+   | Excellent    | event-stream |
| ink       | TypeScript | 27k+  | Janky, React | Hooks        |
| blessed   | TypeScript | 11k   | Dated        | Callbacks    |

**Rust wins**. ratatui is more mature and has better async support than ink.

ink issues:

- React rendering model doesn't fit TUI well
- Streaming updates are awkward
- Layout system is limited

### Model Code Generation Quality

| Task           | TypeScript | Rust (6mo ago) | Rust (now) |
| -------------- | ---------- | -------------- | ---------- |
| Provider impl  | Excellent  | Good           | Excellent  |
| TUI components | Good       | Poor           | Good       |
| Async patterns | Excellent  | Medium         | Good       |
| Error handling | Good       | Good           | Excellent  |
| Tool impls     | Excellent  | Good           | Good       |

**6 months ago**: TypeScript had significant advantage. Models struggled with Rust lifetimes, async traits, and ratatui patterns.

**Now**: Models (Claude 4, Opus 4.5) handle Rust much better. Codex CLI exists as reference. ratatui has better docs.

### Reference Implementations

| Project     | Language   | License     | Value                                                 |
| ----------- | ---------- | ----------- | ----------------------------------------------------- |
| Codex CLI   | Rust       | MIT         | Multi-turn loop, tool orchestration, approval caching |
| Claude Code | TypeScript | Proprietary | Skills, MCP, checkpoints (can't use directly)         |
| Gemini CLI  | TypeScript | Apache 2.0  | Provider patterns                                     |
| Pi          | TypeScript | MIT         | Minimal agent architecture                            |

**Rust has Codex CLI** (MIT) which we can reference for:

- Agent loop patterns
- Tool orchestration
- Approval caching
- Streaming handling

### Long-term Maintenance

| Factor           | TypeScript           | Rust       |
| ---------------- | -------------------- | ---------- |
| Dependency churn | High (npm ecosystem) | Low        |
| Breaking changes | Frequent             | Rare       |
| Type stability   | @types can lag       | Built-in   |
| Security updates | Many transitive deps | Fewer deps |

**Rust wins** on maintenance. Fewer dependencies, more stable ecosystem.

---

## Risk Assessment

### TypeScript Risks

1. **ink quality**: The main TUI library has known issues with streaming and layout
2. **MCP overhead**: Memory queries through IPC add latency
3. **Runtime errors**: Agent crashes are possible at runtime
4. **Dependency security**: Large npm tree increases attack surface

### Rust Risks

1. **Development speed**: Initial implementation takes longer
2. **Model code quality**: Though improved, models can still produce non-compiling Rust
3. **OmenDB stability**: Version 0.0.23 - API may change
4. **Learning curve**: Contributors need Rust knowledge

---

## Mitigations

### For Rust Risks

| Risk               | Mitigation                                      |
| ------------------ | ----------------------------------------------- |
| Development speed  | Use Codex CLI patterns, start with minimal MVP  |
| Model code quality | Iterative development, compile-check frequently |
| OmenDB stability   | Pin version, wrap in abstraction layer          |
| Learning curve     | Good documentation, idiomatic patterns          |

### If We Chose TypeScript

| Risk                | Mitigation                           |
| ------------------- | ------------------------------------ |
| ink quality         | Consider blessed-contrib or raw ANSI |
| MCP overhead        | Cache aggressively, batch queries    |
| Runtime errors      | Extensive error boundaries           |
| Dependency security | Audit, lock versions                 |

---

## Decision Matrix

| Factor              | Weight | TypeScript | Rust    | Notes                         |
| ------------------- | ------ | ---------- | ------- | ----------------------------- |
| Dev velocity        | 15%    | 9          | 6       | TS faster initially           |
| Provider ecosystem  | 15%    | 9          | 5       | AI SDK is great               |
| Memory integration  | 20%    | 5          | 9       | OmenDB is Rust-native         |
| Agent safety        | 20%    | 5          | 9       | Rust safer for code execution |
| Binary distribution | 10%    | 6          | 9       | Rust smaller, faster          |
| TUI quality         | 10%    | 5          | 8       | ratatui > ink                 |
| Maintenance         | 10%    | 5          | 8       | Rust more stable              |
| **Weighted Total**  |        | **6.2**    | **7.6** | **Rust wins**                 |

---

## Recommendation

**Go with Rust.**

The memory integration advantage (20% weight, 9 vs 5) and agent safety (20% weight, 9 vs 5) outweigh the development velocity advantage of TypeScript.

Key reasons:

1. **Our differentiator is memory** - native OmenDB access matters
2. **Agents execute code** - safety guarantees matter
3. **Models are better at Rust now** - the gap has closed
4. **Codex CLI exists** - we have a reference implementation
5. **Long-term stability** - Rust ecosystem is more stable

### If We're Wrong

If Rust development proves too slow after Phase 1:

- TypeScript rewrite is possible
- Core traits/patterns transfer
- OmenDB has npm package (with MCP overhead)

The decision is not irreversible, but switching mid-project is expensive.

---

## Historical Context

**Previous attempt (6 months ago)**:

- TUI looked good but wasn't functional
- Models struggled with async Rust + ratatui
- Abandoned for TypeScript exploration

**What changed**:

- Claude 4 and Opus 4.5 are much better at Rust
- Codex CLI released (MIT reference)
- ratatui documentation improved
- OmenDB Rust crate matured

---

## Action Items

1. [x] Document decision in DECISIONS.md
2. [x] Update architecture for simple keybindings (no vim modes)
3. [ ] Start Phase 1 with Rust
4. [ ] Evaluate after Phase 1 completion
5. [ ] Consider TypeScript if Phase 1 takes >2x expected time

---

## References

- Codex CLI: https://github.com/openai/codex (MIT)
- ratatui: https://ratatui.rs/
- OmenDB: https://docs.rs/omendb
- Vercel AI SDK: https://sdk.vercel.ai/ (for comparison)

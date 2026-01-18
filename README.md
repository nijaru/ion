# ion

Local-first TUI coding agent with native memory integration.

## Vision

- **Persistent Memory**: OmenDB + rusqlite for long-term project knowledge
- **Smart Context**: Budget-aware assembly that fills context with relevant memories
- **TUI First**: A `ratatui` interface designed for professional developers

## Technology Stack

| Component  | Choice                  | Why                                  |
| ---------- | ----------------------- | ------------------------------------ |
| **TUI**    | `ratatui` + `crossterm` | Mature, async-friendly terminal UI   |
| **Memory** | `omendb` + `rusqlite`   | Vector search + relational storage   |
| **LLM**    | OpenRouter (Primary)    | Access to DeepSeek, Claude, and more |
| **Async**  | `tokio`                 | Industry standard for async Rust     |

## Installation

Requires Rust Nightly (omendb uses `portable_simd`).

```bash
git clone https://github.com/nijaru/ion.git
cd ion
cargo run
```

## Project Structure

- `src/` — Core Rust implementation.
  - `agent/` — Multi-turn loop and session management.
  - `memory/` — OmenDB and context assembly logic.
  - `provider/` — LLM provider abstractions (OpenRouter, Anthropic, etc.).
  - `tool/` — Built-in tools (read, write, grep, etc.).
  - `tui/` — Terminal UI components.
- `ai/` — Persistent session context and design documents.
- `AGENTS.md` — Deep-dive instructions for AI assistants.

## Roadmap

- [ ] **Phase 1: Foundation** — Provider traits and basic TUI structure.
- [ ] **Phase 2: Tools** — Core file and shell tools implementation.
- [ ] **Phase 3: Memory** — OmenDB integration and vector storage.
- [ ] **Phase 3.5: RLM Context** — Recursive context management.
- [ ] **Phase 4: Skills + MCP** — Extensibility via SKILL.md and Model Context Protocol.

## License

MIT License - see [LICENSE](LICENSE) file.

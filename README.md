# ion

**Local-first TUI coding agent with native OmenDB memory integration.**

`ion` is a high-performance Rust-based terminal agent designed for deep codebase understanding and autonomous task execution. It distinguishes itself through a "budget-aware memory context assembly" system, using native Rust vector search (OmenDB) to build highly relevant context windows.

## 🚀 Vision

- **Native Speed**: Built in Rust for near-instant tool execution and UI responsiveness.
- **Persistent Memory**: Native OmenDB + rusqlite integration for long-term project knowledge without IPC overhead.
- **Smart Context**: Budget-aware assembly that fills your context window with the most relevant files and memories.
- **TUI First**: A polished `ratatui` interface designed for professional developers.

## 🛠️ Technology Stack

| Component    | Choice                  | Why                                             |
| ------------ | ----------------------- | ----------------------------------------------- |
| **Language** | Rust (Nightly)          | Performance, safety, single binary.             |
| **TUI**      | `ratatui` + `crossterm` | Mature, async-friendly terminal UI.             |
| **Memory**   | `omendb` + `rusqlite`   | Native Rust vector search + relational storage. |
| **LLM**      | OpenRouter (Primary)    | Access to DeepSeek, Claude, and more.           |
| **Async**    | `tokio`                 | Industry standard for async Rust.               |

## 📦 Installation

_Note: Requires Rust Nightly (for `portable_simd` used by OmenDB)._

```bash
# Clone the repository
git clone https://github.com/nijaru/ion.git
cd ion

# Build and run
cargo run
```

## 📂 Project Structure

- `src/` — Core Rust implementation.
  - `agent/` — Multi-turn loop and session management.
  - `memory/` — OmenDB and context assembly logic.
  - `provider/` — LLM provider abstractions (OpenRouter, Anthropic, etc.).
  - `tool/` — Built-in tools (read, write, grep, etc.).
  - `tui/` — Terminal UI components.
- `ai/` — Persistent session context and design documents.
- `AGENTS.md` — Deep-dive instructions for AI assistants.

## 📋 Roadmap

- [ ] **Phase 1: Foundation** — Provider traits and basic TUI structure.
- [ ] **Phase 2: Tools** — Core file and shell tools implementation.
- [ ] **Phase 3: Memory** — OmenDB integration and vector storage.
- [ ] **Phase 3.5: RLM Context** — Recursive context management.
- [ ] **Phase 4: Skills + MCP** — Extensibility via SKILL.md and Model Context Protocol.

## ⚖️ License

MIT License - see [LICENSE](LICENSE) file.

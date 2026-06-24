# Changelog

All notable changes to Ion are documented in this file.

## [0.0.0] — 2026-06-21

### Added

#### Commands
- `/hotkeys` — show all keyboard shortcuts
- `/clone` — duplicate current session
- `/copy` — copy last assistant response to clipboard
- `/reload` — reload keybindings and model config
- `/scoped-models` — show configured scoped models
- `/logout` — clear provider API key
- `/name` — name current session
- `/export` — export session bundle
- `/import` — import session bundle
- `/tree` — show session lineage

#### Hotkeys
- `Ctrl+L` / `Ctrl+Shift+L` — cycle model forward/backward (scoped models or primary/fast fallback)
- `Ctrl+G` — open external editor (Pi parity)
- `Ctrl+T` — toggle thinking blocks visibility
- `Ctrl+O` — toggle tool output
- `Shift+Tab` — cycle thinking level
- `Alt+Up` — recall queued turns
- `Alt+Enter` — queue follow-up
- `Ctrl+F` — fork from session picker
- `Ctrl+N` — toggle named sessions only
- `Ctrl+S` — cycle sort modes (recent/threaded/relevance)

#### Features
- Scoped models — configure multiple models in `[[scoped_model]]` config sections
- Keybindings config — user-customizable via `~/.ion/keybindings.json`
- Auth filtering — filter scoped models by available API keys
- Provider display names — human-readable names in status bar
- Runtime diagnostics — token usage, cost, thinking level, preset, session info in `/status`
- Output guard — 50KB limit on all tool outputs with per-line truncation

### Changed

- Hotkey redesign: `Ctrl+M` → `Ctrl+L`, `Ctrl+X` → `Ctrl+G`, removed `Ctrl+P`/`Ctrl+N`
- Session picker now shows branch labels and supports Ctrl+F fork

## [0.0.0] — 2026-06-20

### Added

- Phase 1: Core Agent + TUI (P1 15/15, P2 23/23)
- Phase 2: Rewrite/Refactor
- Session persistence (SQLite), export/import, fork
- MCP client (stdio), MCP server
- Provider integration (Xiaomi direct, OpenRouter, Anthropic, Gemini, Ollama)
- All slash commands (/help, /model, /provider, /status, /compact, /resume, /quit, etc.)

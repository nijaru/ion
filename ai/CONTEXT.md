# Recent Work Context

## TUI Overhaul (2026-01-16)

### Keybinding Refactor

Removed vim-style Normal/Insert modes. Now single Input mode with standard keybindings:

| Key          | Action                                       |
| ------------ | -------------------------------------------- |
| Enter        | Send message                                 |
| Shift+Enter  | Newline                                      |
| Ctrl+C       | Clear / quit if empty / interrupt if running |
| Ctrl+D       | Quit if empty                                |
| Tab          | Cycle tool mode: Read → Write → Agi          |
| Ctrl+M       | Model picker (current provider)              |
| Ctrl+P       | API provider picker                          |
| Page Up/Down | Scroll chat                                  |
| Arrow keys   | Text cursor (history at boundaries)          |

### Provider Architecture

Two distinct concepts:

- **API Provider** (Ctrl+P): Backend service (OpenRouter, Anthropic, OpenAI, etc.)
- **Model** (Ctrl+M): Specific model from current provider

**Files:**

- `src/provider/api_provider.rs` - ApiProvider enum, auth detection from env vars
- `src/tui/provider_picker.rs` - Provider picker modal
- `src/tui/model_picker.rs` - Model picker with filtering

**Auth detection:**

- Checks env vars: `OPENROUTER_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.
- Only shows implemented providers (currently just OpenRouter)
- Visual: ● green = authenticated, ○ gray = needs auth

### Tool Permission Modes

Three modes cycled with Tab:

- **Read** (Cyan): Only safe/read-only tools
- **Write** (Yellow): Standard mode, prompts for restricted tools
- **Agi** (Red): Full autonomy, no prompts

## Current State

- 42 tests passing
- Only OpenRouter provider implemented
- Provider picker shows auth status but can only select OpenRouter
- Next: Implement additional providers (Anthropic, OpenAI, etc.)

## Key Files

- `src/tui/mod.rs` - Main TUI, keybinding handlers, Mode enum
- `src/provider/api_provider.rs` - ApiProvider enum with 8 providers defined
- `src/tool/mod.rs` - ToolOrchestrator with set_tool_mode()

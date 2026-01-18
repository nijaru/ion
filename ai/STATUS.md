# ion Status

## Current State

| Metric | Value           | Updated    |
| ------ | --------------- | ---------- |
| Phase  | 5 - Polish & UX | 2026-01-18 |
| Focus  | TUI Polish      | 2026-01-18 |
| Status | Runnable        | 2026-01-18 |
| Tests  | 51 passing      | 2026-01-18 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Built-in providers (OpenRouter, Anthropic, Ollama)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management
- Claude Code-compatible hook system

**Memory Plugin** (ion-memory, separate):

- OmenDB integration
- Loaded via hook system
- Can be skipped for minimal agent

## Session Accomplishments

**CLI One-Shot Mode Implemented:**

- `ion run "prompt"` with full flag support
- Flags: `-m`, `-o`, `-q`, `-y`, `-f`, `-v`, `--max-turns`, `--no-tools`, `--cwd`
- Output formats: text, json, stream-json
- Exit codes: 0=success, 1=error, 2=interrupted, 3=max-turns
- Code reviewed and all findings fixed (UTF-8 safety, error handling, abort token)

**Model Picker (Previous Session):**

- Fixed pricing parsing, sorting, column headers
- Tab navigation between provider and model pickers

## Priority: TUI Bugs & Polish

**P0 - Critical Bugs:**

- [ ] tk-ltfn: Spinner/Ionizing persists after agent completion
- [ ] tk-pm5r: Text input box disappears during agent response
- [ ] tk-3jba: Ctrl+C not interruptible during tool execution

**P1 - UX Issues:**

- [ ] tk-arh6: Tool execution not visually obvious
- [ ] tk-yx6s: Message headers show 'ion' instead of model name
- [ ] tk-3ffy: Tool indicator '~ tool' not intuitive
- [ ] tk-4xe8: Approval dialog wording (APPROVAL → Approval)
- [ ] tk-32ou: Status/progress text positioning
- [ ] tk-o4uo: Modal escape handling consistency

**P2 - Features:**

- [x] tk-9n1n: CLI one-shot mode (`ion run`) - DONE
- [ ] Terminal title: `ion <cwd>`
- [ ] Slash command autocomplete (fuzzy)
- [ ] Session retention (30 days)

**P3 - Plugin System:**

- [ ] Hook event enum
- [ ] Hook runner (subprocess, JSON stdin/stdout)
- [ ] Plugin discovery

## TUI Issues Detail

### Bugs

1. **Spinner persists** - "Ionizing..." and spinner remain after agent completes
2. **Input disappears** - Text box vanishes during response, should always be visible
3. **Can't interrupt** - Ctrl+C doesn't work during tool execution, can't exit app

### UX Problems

4. **Tool calls unclear** - Not obvious when tools are running vs spinner for no reason
   - Claude Code shows: `⏺ Bash(command)` with collapsible output
   - We show: `~ tool` / `Executing bash...` which is vague

5. **Message headers** - Shows "ion" instead of actual model (e.g., "claude-sonnet-4")
   - User may switch models mid-conversation
   - Important for debugging which model responded

6. **Tool indicator** - `~` is not intuitive, consider `⏺` or `>`

7. **Approval dialog**:
   - "APPROVAL" is shouty → "Approval"
   - "(A)lways permanent" wording unclear
   - May accept wrong keys (needs investigation)

8. **Layout during execution**:
   - Input should always be visible
   - Progress/status on line below input, above status line
   - Empty line buffer at bottom

9. **Modal escape**:
   - Provider picker: Escape blocked during setup (should close)
   - Model picker: Tab for switching, Escape for closing

## Reference: Claude Code Style

```
⏺ Bash(echo "hello")
  ⎿  hello

⏺ Write(path/to/file.rs)
  ⎿  Wrote 158 lines to path/to/file.rs
     ... (ctrl+o to expand)

⏺ Done. Summary here.
```

Key patterns:

- `⏺` prefix for tool calls
- Tool name with args in parens
- Collapsible output with line counts
- Clear "Done." marker

## Completed

- [x] TUI Modernization: Minimal Claude Code style
- [x] Help modal: One keybinding per line, centered headers
- [x] Plugin architecture design
- [x] Hardened Errors: Type-safe error hierarchy
- [x] Context Caching: minijinja render cache
- [x] First-time setup flow
- [x] Provider/Model picker UX overhaul
- [x] Model picker pricing (fixed)
- [x] Model sorting (org → newest)
- [x] CLI one-shot mode (`ion run`)

## Design Documents

- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions

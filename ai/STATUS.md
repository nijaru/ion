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

**Input & Message Queueing Implemented:**

- Input always visible (even while agent is running)
- Yellow border + spinner indicates running state
- Enter while running queues message for next turn
- "Queued" badge shows pending message
- Agent checks queue between turns for mid-task steering
- Approval dialog wording improved (less shouty)

**CLI One-Shot Mode (Previous):**

- `ion run "prompt"` with full flag support
- Flags: `-m`, `-o`, `-q`, `-y`, `-f`, `-v`, `--max-turns`, `--no-tools`, `--cwd`
- Output formats: text, json, stream-json
- Exit codes: 0=success, 1=error, 2=interrupted, 3=max-turns

## Priority: TUI Bugs & Polish

**P0 - Critical Bugs:**

- [x] tk-ltfn: Spinner/Ionizing persists after agent completion - DONE
- [x] tk-pm5r: Text input box disappears during agent response - DONE
- [x] tk-7cpv: Message queueing (type while agent runs, steer mid-task) - DONE
- [ ] tk-3jba: Ctrl+C not interruptible during tool execution

**P1 - UX Issues:**

- [ ] tk-arh6: Tool execution not visually obvious
- [ ] tk-yx6s: Message headers show 'ion' instead of model name
- [ ] tk-3ffy: Tool indicator '~ tool' not intuitive
- [x] tk-4xe8: Approval dialog wording (APPROVAL → Approval) - DONE
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

## Remaining TUI Issues

### Bugs

1. **Can't interrupt** - Ctrl+C doesn't work during tool execution, requires tools to check abort token

### UX Problems

2. **Tool calls unclear** - Not obvious when tools are running
   - Claude Code shows: `⏺ Bash(command)` with collapsible output
   - We show: `~ tool` / `Executing bash...` which is vague

3. **Message headers** - Shows "ion" instead of actual model (e.g., "claude-sonnet-4")

4. **Tool indicator** - `~` is not intuitive, consider `⏺` or `>`

5. **Modal escape** - Provider/model picker escape handling

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
- [x] Input always visible
- [x] Message queueing for mid-task steering
- [x] Approval dialog wording

## Design Documents

- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions

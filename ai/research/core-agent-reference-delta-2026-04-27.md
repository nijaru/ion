---
date: 2026-04-27
summary: Focused Pi/Codex/Claude reference delta for Ion CLI, command, fork, and goal behavior.
status: active
---

# Core Agent Reference Delta

## Answer

Ion should keep the native Canto/Ion split and current Bubble Tea direction. The
immediate parity target is narrow: preserve the reliable native loop, keep the
prompt CLI scriptable, and only add command surfaces when they simplify daily
session/workflow management.

Recent Ion changes already close the most obvious CLI gaps against Pi and Codex:
- `-p` is a print-mode switch with a positional prompt.
- known flags can appear before or after the positional prompt.
- piped stdin can be the prompt, and prompt-plus-stdin appends a `<stdin>` context block.
- Fedora/local-api can run a live bash-tool smoke without duplicating endpoint config.

## Reference Findings

### Command and fork delta, 2026-05-03

Sources:
- `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/core/slash-commands.ts:18`
- `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/docs/sessions.md:21`
- `/Users/nick/github/openai/codex/codex-rs/tui/src/slash_command.rs:12`
- `/Users/nick/github/openai/codex/codex-rs/tui/src/chatwidget/slash_dispatch.rs:612`
- `/Users/nick/github/openai/codex/codex-rs/app-server/src/codex_message_processor.rs:4849`
- <https://code.claude.com/docs/en/commands>
- <https://code.claude.com/docs/en/sub-agents>

Findings:

- Pi has no built-in `/goal`. Its newer session surface is `/tree`, `/fork`,
  `/clone`, `/resume`, `/new`, `/export`, `/import`, `/share`, `/reload`, and
  model/scoped-model controls.
- Pi's split is a good Ion target: `/tree` branches inside the current session
  file, `/fork` creates a new session from an earlier user message, and
  `/clone` duplicates the current active branch into a new session.
- Codex has a feature-gated `/goal` for long-running tasks. It supports setting
  an objective plus `clear`, `pause`, and `resume`; it is available during an
  active task.
- Codex also has `/fork` and `/side`. `/fork` copies persisted history into a
  new thread; `/side` starts an ephemeral fork for side conversation.
- Claude Code now documents `/branch` with `/fork` as an alias, and an
  experimental forked-subagent mode where `/fork <directive>` inherits the full
  current conversation, runs in the background, and returns only the final
  result to the main thread.
- Claude also has `/btw` for discarded side questions, `/tasks` for background
  tasks, and larger workflow commands such as `/batch`, `/loop`, and
  `/schedule`.

Ion implications:

- Do not add `/goal` yet. A real goal primitive depends on durable
  long-running/background task state, pause/resume status, token/time
  accounting, and status/progress display. Without those, `/goal` would be
  prompt metadata instead of a product primitive.
- Keep investing in session branching before goal tracking. Ion already has
  `/fork` and `/tree`; the next useful deltas are clearer tree UX, a distinct
  `/clone` if users need "duplicate current branch" semantics, and later
  `/side` for ephemeral exploratory forks.
- Background job visibility should come before `/goal`: a command surface like
  `/tasks`/`/ps` and `/stop` is the missing substrate for long-running goals and
  dev-server workflows.
- Use one command catalog as the source for dispatch, help, picker rows,
  autocomplete, active-turn availability, and feature visibility. Codex and
  Claude both show why command surfaces need gating as they grow.

### Pi

Sources:
- `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/cli/args.ts:123`
- `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/cli/args.ts:217`
- `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/cli/args.ts:260`
- `/Users/nick/github/badlogic/pi-mono/packages/coding-agent/src/modes/print-mode.ts:1`

Useful shape:
- `--print, -p` is a boolean noninteractive switch.
- Prompt text is positional, including examples like `pi -p "List all .ts files in src/"`.
- `--mode json` is event-stream JSON, not just final-response JSON.
- `--tools read,grep,find,ls -p ...` is the simple read-only scripting pattern.

Ion status:
- Keep Ion's current `--json` final-result shortcut for smoke tests.
- Add event JSONL only when an integration needs streaming event semantics.
- Preserve the Pi-like distinction between in-file tree navigation and new-file
  fork/clone workflows.
- Do not copy Pi's broad extension/package surface now.

### Codex

Sources:
- `/Users/nick/github/openai/codex/codex-rs/README.md:51`
- `/Users/nick/github/openai/codex/codex-rs/exec/src/cli.rs:67`
- `/Users/nick/github/openai/codex/codex-rs/exec/src/lib.rs:1643`
- `/Users/nick/github/openai/codex/codex-rs/exec/src/lib.rs:1690`
- `/Users/nick/github/openai/codex/codex-rs/exec/src/lib.rs:1716`

Useful shape:
- `codex exec PROMPT` is the programmatic path.
- If no prompt is supplied, piped stdin becomes the prompt.
- If both prompt and stdin are supplied, stdin is appended as a `<stdin>` block.
- `-` forces stdin as prompt.
- There is an explicit ephemeral/no-persist mode.

Ion status:
- Ion now matches the stdin behavior that matters for scripts.
- `--continue -p ...` and `--resume <id> -p ...` are enough for current automated core-loop testing.
- Ephemeral/no-session mode is useful later, but not required for core reliability.
- `/goal` is a useful later Codex reference, not a current Ion command target.

## Next Priority

1. Keep `tk-mmcs` active as the core-parity umbrella.
2. Do not pull ACP tasks forward just because they are P2 in `tk ready`; native loop is still the product path.
3. Next practical slice should be one of:
   - a small JSONL event output mode for print mode if a concrete automation needs live tool/status events,
   - TUI manual/tmux smoke coverage for resume and slash-command behavior,
   - provider/model/thinking selector polish only if it blocks regular use.

## Non-Goals For Now

- `/tree` until Canto has real session-tree primitives.
- extension/package systems.
- profiles or presets beyond existing explicit provider/model/thinking settings.
- sandbox/approval expansion ahead of native loop polish.

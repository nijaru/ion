---
date: 2026-04-27
summary: Focused Pi/Codex reference delta after Ion core-loop Gate 2 stabilization.
status: active
---

# Core Agent Reference Delta

## Answer

Ion should keep the native Canto/Ion split and current Bubble Tea direction. The immediate parity target is now narrower: preserve the reliable native loop, keep the prompt CLI scriptable, and avoid adding ACP/sandbox/subagent/tree work until a specific workflow needs it.

Recent Ion changes already close the most obvious CLI gaps against Pi and Codex:
- `-p` is a print-mode switch with a positional prompt.
- known flags can appear before or after the positional prompt.
- piped stdin can be the prompt, and prompt-plus-stdin appends a `<stdin>` context block.
- Fedora/local-api can run a live bash-tool smoke without duplicating endpoint config.

## Reference Findings

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

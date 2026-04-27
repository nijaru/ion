---
date: 2026-04-27
summary: Target lifecycle design for Ion TUI commands, startup/resume, and print CLI.
status: active
---

# Ion App And CLI Lifecycle

## Purpose

Make command/session lifecycle predictable before further TUI polish.

## Input Classification

| Input | Session materialized? | Model-visible transcript? | Display row? |
| --- | --- | --- | --- |
| plain user prompt | yes | yes, by Canto | yes, user row |
| slash command | no | no | command-dependent system row only |
| picker open/cancel | no | no | no or system row only |
| model/provider/thinking setting | no unless preserving existing materialized session | no | system notice |
| `/resume` picker | no | no | picker only |
| `/resume <id>` | no new transcript | no | resumed marker + replay |
| `--continue` startup | no new transcript | no | startup header + replay |
| `-p` print prompt | yes | yes, by Canto | stdout final result |

Current concern:

- `submitText` prints a user row even for slash commands. That may be acceptable visually, but it must not imply durable transcript. The design preference is to make command display clearly system/command-shaped, not model-user-shaped, during the cleanup pass.

## Startup Lifecycle

1. Load config/state.
2. Resolve trust/mode.
3. Open storage.
4. Select existing session only if `--continue` or `--resume <id>` requested.
5. Open runtime.
6. Load startup replay entries only for selected session.
7. Print launch header.
8. Print resumed marker after launch header and before replay.
9. Start TUI event loop.

Startup must not create a durable session unless a real model turn is sent.

## Runtime Switch Lifecycle

Provider/model/session switch should:

- cancel in-flight turn
- close old session after new runtime opens successfully
- clear stale progress error on success
- preserve materialized session only when explicitly requested
- not persist provider/model automatically at startup
- print only system notices and replay entries, not fake user transcript

Failure should:

- leave previous runtime intact if possible
- show one visible error
- not partially switch store/session pointers

## Print CLI Lifecycle

Print mode is the core automation surface.

Required paths:

| Command shape | Meaning |
| --- | --- |
| `ion -p "prompt"` | run one prompt, text output |
| `ion -p --json "prompt"` | run one prompt, final JSON output |
| `ion --print "prompt" --json` | same as above |
| `echo x | ion -p` | stdin is prompt |
| `echo x | ion -p "prompt"` | prompt plus `<stdin>` context block |
| `ion --continue -p "prompt"` | resume most recent conversation session and send prompt |
| `ion --resume <id> -p "prompt"` | resume specific session and send prompt |
| `ion --resume` | TUI mode opens selector, no session materialization |

Print mode should keep final JSON stable, but streaming event JSONL is deferred until a concrete integration needs it.

## Progress/Error Rules

Stale error clearing:

- clear on successful runtime switch
- clear on `TurnStarted`
- clear on model/provider/session picker success
- preserve on failed switch

Status persistence:

- retry/network status may persist as display-only state
- "Ready" should not spam replay unless it is the only useful status
- provider-limit errors should persist with raw provider text and readable prefix

## Command During Active Turn

Allowed local commands should be explicit:

- `/help`
- `/model`, `/provider`, `/thinking`, `/settings` picker open
- `/mode`
- `/cancel` if added later
- `/cost`
- `/tools`

Commands that alter runtime during active turn must cancel or queue deliberately. No command should mutate model-visible transcript unless it submits a real prompt.

## Testing Plan

- slash command before first model turn creates no session
- `/resume` picker creates no session
- bare `--resume` opens picker in TUI mode
- `--continue -p` resumes and sends prompt
- model/provider switch clears stale error on success
- failed runtime switch preserves previous session/runtime
- print prompt plus stdin produces correct prompt block
- command rows are not durable model transcript

## Implementation Slices

1. Write command classification tests.
2. Adjust slash command live display if needed.
3. Harden runtime switch success/failure state transitions.
4. Expand print CLI smoke matrix.
5. Improve launch header formatting after lifecycle is stable.

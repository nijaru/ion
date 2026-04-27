---
date: 2026-04-27
summary: Focused review of native core-loop contract enforcement after resume/tool-call failures.
status: active
---

# Core Loop Review

## Verdict

The native core loop now has deterministic and live evidence for the failure shape that triggered this review. The empty-assistant protection is not only a replay bandaid. Canto has both creation-side prevention and projection-side sanitation:

- creation-side prevention: non-streaming and streaming agent paths skip assistant commits without content, reasoning, thinking blocks, or tool calls.
- projection-side sanitation: effective history drops invalid legacy, snapshot, and post-snapshot assistant rows before provider requests.
- Ion follow-up coverage targets the reported failure shape: first turn persists text, second turn uses a tool, resumed session sends a third turn.
- Fedora/local-api live smoke passed for tool approval, persistence, resume, and follow-up, and direct `ion -p` / `--resume ... -p` CLI smokes passed against the qwen model.

## Findings

| Area | Status | Notes |
| --- | --- | --- |
| Canto write-side assistant commits | OK | `agent/loop.go` and `agent/stream.go` both call `hasAssistantPayload` before appending assistant messages. |
| Canto effective history | OK | `session/rebuilder.go` normalizes effective entries and drops invalid assistant rows, including snapshot-derived rows. |
| Ion deterministic resume/tool coverage | OK | Backend test now creates a real tool turn, resumes the session, submits a follow-up, and asserts provider history contains matching assistant tool call and tool result with no empty assistant rows. |
| Ion persistence/replay coverage | OK | Existing app/storage tests cover duplicate transcript prevention, terminal cancellation/error/status durability, tool replay, and resume transcript spacing. |
| Fedora/local-api live verification | OK | `TestLiveSmokeTurnAndToolCall` passed against `local-api/qwen3.6:27b-uncensored` with tool approval, bash output, persistence, resume, and follow-up response. |
| Print CLI live verification | OK | `ion -p --json`, `ion --mode auto -p --json` with a tool call, and `ion --resume <session> -p --json` all passed against Fedora/local-api. |

## Next Slice

1. Keep this contract test set green while working on later features.
2. Use `ion -p` and `--resume ... -p` as the default live smoke surface for future core-loop changes.
3. Resume Pi/Codex reference polish and slash/TUI work only in narrow slices that do not weaken the native-loop contract.

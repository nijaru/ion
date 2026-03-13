# ACP Integration

## Summary

ACP support should arrive as an `ACPAgentSession` implementation behind the host-facing session interface.

## Responsibilities

The ACP layer owns:

- process or transport setup
- initialize / session handshake
- prompt submission
- event translation
- cancel handling
- session resume/load mapping
- approval / tool / plan event translation

## Boundaries

The ACP adapter should translate from protocol events into ion-owned session events.

Keep protocol-specific quirks inside the adapter:

- wire structs
- transport lifecycle
- backend-specific capability differences
- approval and tool event normalization

## Initial Targets

1. Gemini CLI or another clean ACP reference backend
2. Claude Code adapter/backend
3. Codex-compatible backend later as support matures

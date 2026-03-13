# Memory and Context

## Summary

The rewrite should preserve `ion`'s ambition around durable context and strong memory systems, but those systems belong behind the session/runtime layer, not in the UI.

## Design Goals

- durable session persistence
- compact but accurate transcript summaries
- explicit working context and task context
- room for semantic memory later
- predictable boundaries between live transcript, stored transcript, and derived memory

## Inputs to Preserve

Useful retained research/themes:

- context management
- compaction techniques
- session storage patterns
- tool-display patterns
- coding-agent architecture surveys

## Near-Term Work

1. transcript/session persistence for the Go host
2. context compaction strategy for native sessions
3. memory model that can scale to semantic memory later

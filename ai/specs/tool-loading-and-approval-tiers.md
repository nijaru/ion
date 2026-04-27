# Tool Loading And Approval Tiers

## Current Slice

Ion's native Canto backend already uses Canto `prompt.NewLazyTools`, which
switches to `search_tools` once the registry exceeds Canto's lazy threshold
(currently 20 tools). Ion now exposes that state to the user:

- startup line: `Tools N registered • Search tools enabled • Sandbox <mode>`
- slash command: `/tools`

This keeps the core loop simple while confirming whether MCP-heavy sessions are
using deferred tool loading.

## Approval Tiers

Ion is intentionally staying with three visible modes:

- READ
- EDIT
- AUTO

Extra Claude-style tiers are represented by policy config and per-session
category approvals rather than more modes. The current product rule is:

- add config rules when users need durable specificity
- add new modes only when they create a genuinely different safety boundary

## Deferred Work

Future tool UX can add:

- richer `/tools <query>` backed by `search_tools`
- per-tool category labels in the TUI
- MCP server grouping
- approval history/audit views

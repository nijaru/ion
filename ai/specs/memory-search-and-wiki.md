# Memory Search And Wiki UX

## Current Slice

Ion now exposes Canto workspace memory through `/memory`:

- `/memory` renders a Canto memory index tree for the current workspace.
- `/memory <query>` searches workspace memory and prints concise results.

The agent already has:

- `recall_memory`
- `remember_memory`
- background prompt injection via `prompt.MemoryPrompt`

## Product Boundary

Do not build a full wiki editor until the basic search/store loop is reliable.
The useful near-term shape is:

1. explicit memory search
2. explicit semantic writes
3. compact tree rendering
4. later: curated markdown export/import

## Deferred Work

- Wiki compilation command
- collection management UI
- markdown rendering beyond plain scrollback
- sleep-time consolidation policy

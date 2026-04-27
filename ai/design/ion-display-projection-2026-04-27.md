---
date: 2026-04-27
summary: Target design for Ion storage and transcript display projection.
status: active
---

# Ion Display Projection

## Purpose

Make replay a pure display projection over Canto effective history plus Ion display-only events.

## Projection Stack

| Layer | Owner | Data |
| --- | --- | --- |
| Durable transcript/context | Canto `session` | `MessageAdded`, `ContextAdded`, snapshots/projections |
| Effective model-visible view | Canto `EffectiveEntries` | sanitized provider-visible entries with event metadata |
| Ion display-only events | Ion storage | system notices, cancellation, status, token usage, subagent breadcrumbs |
| Rendered transcript | Ion app | `internal/session.Entry` with spacing/styling/tool compaction |

## Current Problem Shape

`internal/storage/canto_store.go` still has an `Append` method that can write model-visible `User`, `Agent`, and `ToolResult` rows. Some of that exists for legacy/file-store compatibility, but the native Canto path should not use it for live model transcript persistence.

Replay currently relies on:

- `EffectiveEntries` for transcript/context.
- manual scan of `ToolCompleted` events for error flags.
- `displayEntries` for Ion-only rows.
- `normalizeDisplayEntries` for compacting and dropping empty agent rows.

This is close to the target, but the responsibilities need to be explicit and tested.

## Display Event Policy

Allowed Ion durable display events:

- `ion_system`
- `ion_subagent`
- `status_changed`
- `token_usage`
- `routing_decision`
- `escalation_notification`
- cancellation notice when user cancels
- provider/user-visible errors not already represented as transcript

Disallowed in native live path:

- `storage.User`
- `storage.Agent`
- `storage.ToolUse`
- `storage.ToolResult`

Exception:

- compatibility tests or non-Canto backends may use the generic `storage.Session.Append` API, but native Canto live path should not persist model-visible transcript through Ion.

## Effective Entry Mapping

| Canto effective entry | Ion display entry |
| --- | --- |
| `MessageAdded` user | user row |
| `MessageAdded` assistant with content/reasoning | agent row |
| `MessageAdded` assistant with only tool calls | no visible row unless needed for debugging |
| `MessageAdded` tool | tool row using tool name and error metadata |
| `ContextAdded` summary/working set/bootstrap | system row, compact if needed |
| durable system/developer transcript context | system row only if it survived Canto projection |

## Compaction Rule

Routine `list`, `read`, `glob`, and `grep` output compaction is display-only:

- provider-visible tool result keeps full content
- replay uses compact summary by default
- errors remain expanded enough to debug
- future detail expansion can read from the durable Canto event

## Duplicate Prevention Invariants

Tests should assert:

- live user row prints once and is persisted only by Canto
- live assistant row prints once and replays once
- live tool row prints once and replays once
- `storage.Append(storage.User/Agent/ToolResult)` is not called by native app path
- `Entries()` never returns both a model-visible tool result and a duplicated Ion display tool row for the same tool use ID

## Replay/Live Renderer Contract

One renderer owns spacing:

- live printing calls `Model.RenderEntries`
- startup replay calls `Model.RenderEntries`
- runtime `/resume` replay calls `Model.RenderEntries`

No alternate string assembly for transcript entries. Startup headers and resumed markers may be separate, but transcript rows must use the shared renderer.

## Open Questions

- Should `storage.Session.Append` reject model-visible events when the backing store is Canto-native and the caller is the app? That may be too invasive; start with tests and call-site cleanup.
- Should Canto expose tool error state directly in `EffectiveEntries`? Ion currently scans raw `ToolCompleted` events to decorate tool rows.
- Should `ion_system` cancellation/error rows be correlated with turn IDs? Useful later, not required for first refactor.

## Test Plan

- real store: clean session success replay
- real store: tool result replay with compact routine output
- real store: tool error replay expanded
- real store: cancellation notice replay
- real store: provider-limit/routing stop replay
- duplicate detector over raw Canto events after live TUI/app path

## Implementation Slices

1. Add tests that capture the current native display event policy.
2. Remove or isolate native app calls that append model-visible rows through Ion storage.
3. Centralize `EffectiveEntries -> Entry` mapping.
4. Add duplicate-prevention assertions.
5. Re-run live `--resume -p` smoke.

# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I4 advanced integrations
**Focus:** I4 skills and self-extension boundary
**Active umbrella:** none - `tk-mmcs` is closed
**Active task:** `tk-hfgh` - safe skill install and gated model-visible tools
**Updated:** 2026-05-02

## Current Truth

- Ion has one native baseline path. There is no global stabilization mode.
- I0-I3 are complete: dirty baseline, native boundary refactor, shell polish,
  and safety/trust/sandbox table stakes are green.
- Canto owns durable events, provider-visible history, agent/tool lifecycle,
  reasoning capability translation, and compaction primitives.
- Ion owns TUI/CLI UX, commands, settings/state, product tools, provider
  selection, display projection, safety/trust policy, and session bundle UX.
- Ion can also run as an ACP agent over stdio with `--agent`; this is a
  secondary host-integration surface around the same `AgentSession` runtime
  boundary, not a second native loop.
- Current default tool surface is `bash`, `read`, `write`, `edit`,
  `multi_edit`, `list`, `grep`, and `glob`; `verify` is not registered by
  default.
- `read_skill` is implemented behind the opt-in `skill_tools = "read"` config
  gate. It is not part of the default eight-tool surface and does not add skill
  inventories to the prompt.
- Canto is closed unless Ion evidence proves a framework-owned defect.

## Latest Evidence

- `tk-4lty` is closed. It adds a storage-level portable bundle plus CLI surface:
  `--export-session <file>` and `--import-session <file>`.
- The bundle format is versioned JSON and includes Ion `session_meta`, Canto
  event envelopes, Canto ancestry metadata, per-session event checksums, a
  whole-bundle checksum, and explicit conflict detection.
- Canto `7dec159` exposes portable event JSON helpers and ancestry import
  primitives; Ion imports that revision.
- Latest bundle gates passed:
  `go test ./cmd/ion ./internal/storage -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
- The bundle CLI smoke runs Ion's `main()` path in a subprocess with temp homes:
  export via `--resume <id> --export-session <file>`, import via
  `--import-session <file>`, then verify the imported transcript through
  storage.
- `tk-gopd` is closed. `Ctrl+X` opens the composer buffer in `$VISUAL`,
  `$EDITOR`, or `vi` through Bubble Tea `ExecProcess`, then reloads edited
  content into the composer. It is blocked while turns, approvals, or
  compaction are active.
- Latest editor-handoff gates passed:
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and a tmux
  smoke that rewrote composer text from `draft` to `edited` through the
  external editor path.
- `tk-st4q` is closed. Ion now has `--agent` headless ACP mode over stdio.
  The adapter implements ACP initialize, new/load session, prompt streaming,
  tool updates, approval requests, cancellation, and session mode updates by
  translating to the existing Ion `AgentSession` boundary.
- Latest ACP-agent gates passed:
  `go test ./cmd/ion ./internal/backend/acp -count=1 -timeout 180s`,
  `go test ./cmd/ion -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and a
  subprocess smoke that starts `ion --agent`, initializes over ACP stdio, and
  verifies no normal TUI startup banner pollutes protocol stdout.
- Recent I4 completed slices: portable session bundles, boundary-step
  steering, typed thinking capability filtering, Canto coding primitive audit,
  local `/skills` browser, local `/fork`, `/tree`, external editor handoff, and
  ACP headless-agent mode.
- First `tk-hfgh` slice is committed as `598d1a2`: opt-in `read_skill` reads
  installed `SKILL.md` bodies by name, is categorized as a read tool, and is
  registered only when `skill_tools` enables skill tools. Gates passed before
  commit: focused config/skills/backend/tool tests, `go test ./...`, and the
  native race subset.

## Next Action

1. Continue `tk-hfgh` with safe skill install staging and keep
   `manage_skill`/marketplace/self-extension behind later explicit gates.
2. Keep one green slice per commit and avoid reopening Canto unless Ion tests
   expose a framework defect.

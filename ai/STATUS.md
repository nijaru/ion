# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I4 advanced integrations
**Focus:** I4 request-cache follow-up
**Active umbrella:** none - `tk-mmcs` is closed
**Active task:** none - next ready item is `tk-aiiz` request cache continuity
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
- Safe local skill install is committed as `e5a5f9f`: `ion skill install
  <path>` previews and validates, `ion skill install --confirm <path>` stages
  then installs regular-file bundles into `~/.ion/skills/<name>`, and
  `ion skill list [query]` mirrors `/skills`. It rejects remote sources,
  symlinks/special files, and overwrites. Gates passed: focused cmd/skills
  tests, `go test ./...`, native race subset, and `git diff --check`.
- `tk-hfgh` is closed. `manage_skill`, marketplace install, and self-extension
  are intentionally split to `tk-exeg` because write-capable skill mutation
  needs a clear trust/policy/undo contract before implementation.
- `tk-exeg` design is captured in `ai/specs/instructions-and-skills.md`:
  future `manage_skill` is opt-in via `skill_tools = "manage"`, unavailable in
  READ, hard-approval for mutations even in AUTO, local-root only, audited, and
  trash-based for removal undo.
- `tk-exeg` is closed as a design/spec task. No `manage_skill` code is exposed
  yet; host-owned `ion skill install --confirm` remains the only skill mutation
  path.
- `tk-03hf` evidence is captured in
  `ai/research/tool-surface-sota-2026-05.md`. ripgo is faster on one warmed
  Ion grep benchmark, but it is not semantically ready to replace rg: closest
  CLI flags include `.git/config` in a fixture where Ion excludes `.git/**`,
  and there is no CLI `rg --files` equivalent for current `glob`.
- `tk-03hf` is closed. Current decision: keep ripgrep-backed Ion tools.
- `tk-n0n4` closeout commits:
  - `f069150` redacts ACP headless tool start/raw input, output deltas, and
    tool-result display payloads before sending them to external ACP hosts.
  - `c1298cf` redacts ACP stderr debug logs before appending to disk.
  Latest gates passed: focused cmd/acp/privacy tests, `go test ./...`, native
  race subset, and `git diff --check`.
- `tk-n0n4` is closed. Provider-visible prompt/history redaction remains
  explicit/future, not default, because silent redaction would change task
  content.

## Next Action

1. Start `tk-aiiz` and define the narrow request-cache continuity risk before
   changing runtime/provider behavior.
2. Keep one green slice per commit and avoid reopening Canto unless Ion tests
   expose a framework defect.

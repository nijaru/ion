# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** Minimal core consolidation
**Focus:** Make the native coding loop boring before more harness or advanced work
**Active task:** `tk-g5sf` - Ion minimal core consolidation pass
**Blocked next task:** `tk-ezms` - Ion runtime boundary alignment
**Updated:** 2026-05-02

## Current Truth

- Ion has one native baseline path. There is no global stabilization mode.
- Recent work added or restored many useful surfaces, but the project should
  not treat that breadth as evidence that the minimal core is finished. The
  current blocker is to simplify and harden the default native path first.
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
- Flue and Mendral are applicable as boundary checks, not as feature requests:
  keep the runtime/session/tool boundary explicit, keep state outside
  disposable execution, and preserve a small model-visible tool surface.
- Canto `canto-2vxb` and Ion `tk-ezms` remain useful, but they wait behind the
  minimal-core pass so the refactor is grounded in the actual default path.

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
- `tk-aiiz` is closed. Canto `62dc906` deep-clones request tool parameters and
  response schemas through `llm.Request.Clone()`, and Ion imports that revision
  so provider-history capture uses the framework clone instead of a local
  shallow clone. Gates passed: Canto `go test ./llm`, Canto `go test ./...`,
  Ion focused `./internal/backend/canto`, Ion `go test ./...`, Ion native race
  subset, and `git diff --check`.
- `tk-a4m1` is closed. Current official OpenAI path is Codex CLI/IDE/app/web
  with ChatGPT sign-in or API-key sign-in, plus Codex app-server/MCP surfaces.
  There is no supported `codex --acp` command in the current CLI, so Ion no
  longer derives a default command for `chatgpt` or `codex` catalog entries.
  Canonical details are in `ai/specs/subscription-providers.md`.
- `tk-pwsl` is closed as a design gate. Ion already has compact inline Plane B
  subagent progress rows and durable child breadcrumbs, but a full
  alternate-screen swarm/operator view waits until subagent registration and
  child-session ownership are stable. The registration prerequisite is closed
  by `tk-29xj`; full swarm mode remains deferred until opt-in subagent usage is
  boring.
- `tk-hz8p` is implemented. The `subagent` tool schema now exposes explicit
  `context_mode` values: `summary`, `fork`, and `none`. `summary` remains the
  default handoff shape, `fork` seeds a child from the parent's provider-visible
  effective history plus the task, and `none` starts fresh with only the task.
  Tests cover schema, mapping, `none` context rejection, and fork-mode child
  history with an in-flight parent tool call. Gates passed:
  `go test ./internal/backend/canto -run 'TestSubagent' -count=1`,
  `go test ./internal/backend/canto -count=1`, `go test ./... -count=1`, and
  the native race subset.
- `tk-29xj` is implemented. `subagent_tools = "on"` is the explicit opt-in
  gate for the model-visible `subagent` tool; the default surface remains the
  eight core tools. READ mode hides `subagent`, EDIT prompts through the
  sensitive-tool policy, and AUTO may run it. Built-in personas now use only
  registered Ion tools, and fast-slot personas fall back to the primary model
  when no fast model is configured. A deterministic smoke proves an opted-in
  `explorer` child runs in `none` mode, receives only the task, returns a tool
  result to the parent, and preserves parent provider-visible history. Gates
  passed: focused config/subagent/backend tests, `go test ./... -count=1`, and
  the native race subset.
- `tk-6prx` is implemented. Markdown rendering now reuses a cached
  Goldmark/GFM renderer instead of rebuilding it for every render. Focused app
  tests passed.
- `tk-w5uj` is implemented. The footer/status line now shows cached git diff
  stats like `+42/-11` from `git diff --shortstat HEAD --`; stats load at TUI
  startup and refresh after completed turns rather than shelling out during
  render. Focused app tests passed.
- `tk-lya7` is implemented. Context usage text is unchanged, but the status
  segment now renders green below 50%, yellow from 50% through 79%, and red at
  80%+. Focused status-line tests, `go test ./...`, and the native race subset
  passed.
- `tk-lggk` is closed as a design decision. Ion should not add an Ion-only
  default `ask_user` tool; models can ask ordinary assistant questions today.
  A future blocking interaction tool belongs behind a Canto elicitation
  primitive with explicit TUI, CLI, ACP, cancellation, resume, and
  noninteractive behavior.
- `tk-ritc` is closed as a design decision. `internal/storage` remains Ion's
  app adapter over Canto because it owns workspace/session indexes, input
  history, lazy materialization, TUI replay projection, and portable bundle UX.
  Canto continues to own reusable durable event, ancestry, and effective-history
  primitives.

## Next Action

1. Execute `tk-g5sf`: audit the default native hot path from startup through
   replay, identify stale/default-off clutter, and remove or relocate only what
   simplifies the minimal core.
2. Re-run deterministic, race, tmux, and Fedora live gates for the minimal
   path before returning to Canto `canto-2vxb` or Ion `tk-ezms`.
3. Keep one green slice per commit; do not expand the model-facing tool surface
   or add new product features during this pass.

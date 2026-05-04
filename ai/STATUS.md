# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** C5 review and simplification
**Focus:** whole-product UI/UX and codebase organization review before new features
**Active task:** `tk-ywbt` — whole UI/UX and codebase simplification pass
**Updated:** 2026-05-04

## Current Truth

- Ion has one native baseline path. There is no global stabilization mode.
- `tk-g5sf` is closed. The minimal native surface is now simplified enough to
  move to the next refactor layer without treating advanced features as part of
  the default core.
- Canto owns durable events, provider-visible history, agent/tool lifecycle,
  reasoning capability translation, and compaction primitives.
- Ion owns TUI/CLI UX, commands, settings/state, product tools, provider
  selection, display projection, safety/trust policy, and session bundle UX.
- Ion can also run as an ACP agent over stdio with `--agent`; this is a
  secondary host-integration surface around the same `AgentSession` runtime
  boundary, not a second native loop.
- Current default tool surface is `bash`, `read`, `write`, `edit`,
  `multi_edit`, `list`, `grep`, and `glob`; the old `verify` tool is removed.
- C2 executor-boundary, C3 context-survival, and C4 command/workflow shell work
  are closed unless a smoke exposes a concrete defect.
- Fork primitives already exist at the session/history level: `/fork [label]`,
  `/tree`, portable export/import bundles, and opt-in subagent
  `context_mode=fork`. Worktree/filesystem forks and richer tree UI are future
  workflow work, not part of the current timestamp pass.
- `tk-jwfs` is closed. Ion now preserves Canto event timestamps through
  host-facing transcript/replay projections while keeping default TUI rendering
  and provider-visible history timestamp-free.
- Fedora is unreachable from this Mac right now:
  `http://fedora:8080/v1/models` times out. `tk-jkcl` is deferred until the
  host responds. OpenRouter `deepseek/deepseek-v3.2` is the current live-smoke
  harness default; older DeepSeek Flash smokes remain historical evidence.
- `tk-d2m6` is closed. Fork/session workflow tests now prove copied fork
  entries, portable bundle import/export, and subagent `context_mode=fork`
  preserve Canto ancestry plus usable event timestamps.
- `read_skill` is implemented behind the opt-in `skill_tools = "read"` config
  gate. It is not part of the default eight-tool surface and does not add skill
  inventories to the prompt.
- Memory remains deferred; the native backend no longer initializes a memory
  manager on the default hot path.
- Flue and Mendral are applicable as boundary checks, not as feature requests:
  keep the runtime/session/tool boundary explicit, keep state outside
  disposable execution, and preserve a small model-visible tool surface.
- `tk-ezms` is closed. `CantoBackend` now uses the Canto harness facade for
  native turn execution and no longer caches second runtime owners.
- `tk-0r23` is closed. Virtual resource namespace direction is now captured in
  the canonical design/spec files.
- Current namespace direction: keep the eight workspace tools as default, keep
  non-workspace resources behind explicit `skill://`, `memory://`, or
  `artifact://` capability resolvers, and expose them only through host
  commands or opt-in narrow tools.
- Command-surface direction after fresh Pi/Codex/Claude review: keep Ion small
  and session-centric. `/tree` and current-point `/fork [label]` are enough for
  now; `/clone`, `/goal`, `/side`, background-job commands, and bash mode are
  deferred until daily use or reference-agent evidence proves they are worth
  their maintenance cost.
- Current maintenance sequence is review/refactor first, not feature growth:
  `tk-ywbt` whole-product review, `tk-245w` app event/shell-state refactor,
  `tk-omw4` app test split, then `tk-rkmn` backend/tool/storage boundary review.
- Sandbox/trust direction: trust is workspace eligibility, mode is approval
  posture, sandbox is executor enforcement, and provider credentials are not
  subprocess credentials by default.
- `tk-vv4y` is closed. Sandbox/trust/credential boundaries are captured in the
  canonical specs.
- `tk-2g2e` is closed. Local bash process execution now sits behind an
  Ion-local executor object while preserving the existing model-facing `bash`
  schema and local sandbox behavior.
- `tk-fhds` is closed. Current design direction keeps local bash environment
  inheritance unchanged until Ion exposes an explicit environment policy.
  Visibility comes first; provider-key stripping and named tool-secret
  injection are future hardening slices.
- `tk-k5yp` is closed. `/tools`, approval previews, and notifications expose
  executor environment posture without listing variable names or values. The
  startup shell intentionally omits this low-level detail.
- `tk-kxpa` is closed. `tool_env = "inherit_without_provider_keys"` preserves
  inherited developer env while stripping provider API-key variables from the
  provider catalog for local bash.
- `tk-lux7` is closed. Tool secrets are specified as named user-global
  injections with approval, redaction, audit, and remote-executor behavior
  defined before any model-visible `bash.secrets` field exists.
- Latest C2 gates passed:
  focused command/app/backend/tool tests for environment posture,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
  Tmux `/tools` smoke showed bash-environment posture in `/tools` with no
  environment values.
- Final C2 provider smoke: Fedora `http://fedora:8080/v1/models` timed out
  from this machine. OpenRouter `deepseek/deepseek-v4-flash` live smoke passed:
  real `bash` tool call, persisted resume, resumed follow-up answered
  `continued`, and provider-history ordering was verified as
  `first_user=1 tool_call=2 tool_result=3 assistant=4 resume_user=5`.
- Command/fork reference pass checked local Pi and Codex source plus current
  Claude Code docs. Pi has `/tree`, `/fork`, and `/clone` but no `/goal`;
  Codex has feature-gated `/goal`, `/fork`, and `/side`; Claude documents
  `/branch` with `/fork` alias and experimental forked subagents. Canonical
  updates landed in `ai/DESIGN.md`, `ai/PLAN.md`, `ai/DECISIONS.md`,
  `ai/specs/tui-architecture.md`, `ai/specs/tools-and-modes.md`, and the
  existing reference delta note.
- Streaming UX direction is now explicit: assistant deltas render live in Plane
  B as plain wrapped text, incomplete Markdown stays raw while streaming, and
  committed assistant messages still render once through Goldmark/GFM in Plane
  A. `/settings` display changes update runtime state without restart.
- C4 `/tree` and `/fork` review is complete. `/fork [label]` is the current
  current-point branch/duplicate flow and switches into the labeled child
  session. `/clone` remains deferred because it is not distinct until Ion grows
  an earlier-turn fork selector.
- `ai/` is pruned back to root source-of-truth files, canonical specs, and a
  small set of active topic/evidence docs. Old one-off plans, duplicate design
  docs, completed sprint notes, archive specs, and stale review files were
  deleted rather than re-archived.
- `tk-g34g` is closed. Minimal-harness acceptance reviewed the default native
  path, found no second loop or leaked deferred command surface, and removed
  internal `/tools` copy that exposed implementation jargon.
- `tk-xh5w` is closed. The minimal-harness acceptance suite is now codified in
  deterministic app/CLI tests plus an optional tmux smoke script. `tk-er04`
  bash-mode evaluation remains low-priority and should not start unless daily
  use proves it is worth the extra surface.
- `tk-l8eo` is closed. The inline shell separators now render at the wrap-safe
  shell width instead of the accidental 24-column cap.
- `tk-yvpb` is closed. Idle `Ready` is now suppressed after transcript/local
  command output, while fresh launch and terminal progress states still render.

## Latest Evidence

- Review hotfix slice closed three code-review findings and one copy issue:
  model-picker metric headers are clamped to shell width, Ion-only storage
  events preserve supplied timestamps, `multi_edit` no longer writes
  predictable user-path `.tmp` files and now emits deterministic diffs, and the
  executor environment label is no longer shown in the startup shell, and
  duplicate idle `Ready` rows after local commands are suppressed. Focused
  tests, `go test ./... -count=1 -timeout 300s`, the native race subset, and
  `scripts/smoke/tmux-minimal-harness.sh` passed after the follow-ups.
- C5 review/refactor started. The first shell-state cleanup extracted the live
  shell renderer from `View()` and made idle-ready suppression an explicit
  predicate; focused app tests, `go test ./...`, the native race subset, and
  tmux smoke passed.
- App event reducer cleanup started: turn-finished settlement is now isolated
  from the large session-event switch. Focused turn-finish tests,
  `go test ./...`, and the native race subset passed.
- Streaming event cleanup continued: thinking deltas, assistant deltas,
  assistant messages, and subagent assistant messages now use dedicated handlers
  instead of large inline switch bodies. Focused app tests, `go test ./...`,
  and the native race subset passed.
- TUI separator hotfix restored full-width composer bars while keeping the
  terminal-width-minus-one resize guard. Focused separator/progress tests,
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and
  `scripts/smoke/tmux-minimal-harness.sh` passed. A fresh 100-column tmux
  capture showed one `Ready` row and wrap-safe full-width separators.
- C5 minimal-harness acceptance now has repeatable gates:
  `TestMinimalHarnessAcceptanceFinalStateAndReplay` covers a fake-backend
  submit/stream/tool/final-render/replay path, print-mode JSON acceptance
  captures streaming deltas/tool calls/token usage, and
  `scripts/smoke/tmux-minimal-harness.sh` covers fresh launch, `/help`,
  `/tools`, `/settings`, resize, and optional live tool/resume flow. Focused
  tests, `go test ./... -count=1 -timeout 300s`, the native race subset, the
  non-live tmux smoke, and OpenRouter `deepseek/deepseek-v3.2` live smoke all
  passed. Fedora local-api still timed out from this Mac.
- Roadmap/task alignment pass found the product is on the right track for a
  minimal but well-engineered core: one native harness path, eight default
  tools, small command surface, compact TUI, and strong deterministic/race/live
  coverage. The gap is now regression packaging, not more feature design.
  Created `tk-xh5w` for the repeatable minimal-harness suite, `tk-omw4` for
  app test-file splitting after that suite, `tk-0gni` for a future edit-tool
  eval, and `tk-tpxu` for later resolved-doc pruning. Fedora still timed out
  on `2026-05-04`.
- Minimal-harness acceptance passed
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
  Source review confirmed the default path uses one Canto harness boundary, the
  visible command catalog filters deferred commands for help/picker/completion,
  and the default model-visible tool surface remains the eight core tools.
  Tmux `/tools` smoke now shows `Tools: 8 (sandbox off; bash env inherited)`
  plus the tool names, with no `eager`/lazy implementation jargon in the
  default UI.
- Resize shell fix passed
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
  Tmux `go run ./...` capture at 120 columns, then resize to 84 and 60
  columns, showed a single `Ready` row. The live composer separators are now
  short rules instead of width-filling rows so terminal reflow does not leave
  stale progress fragments after monitor/window moves. Follow-up edge pass
  also tightened command/session picker overlay rows so long search/help text
  truncates inside the live shell width instead of wrapping during resize.
- AI context prune sanity checks passed: `ai/README.md` link targets resolve,
  stale deleted-doc references are absent from active `ai/` Markdown, and
  `tk ready` showed only the active prune task before closeout. The active
  index now lists root files, canonical specs, and the remaining active topic
  docs only.
- C4 session-command slice passed focused app tests,
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
  Tmux `--continue` smoke verified `/tree`, `/fork smoke branch`, and `/tree`
  from the forked child. Source review fixed session-command targeting to use
  the durable materialized storage session ID instead of a potentially stale
  live agent session ID.
- Streaming Plane B slice passed
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
  Tmux against OpenRouter DeepSeek Flash showed live assistant text appearing
  while a long bullet answer streamed. The capture exposed leading-newline
  padding and duplicate full error copy; both were corrected so live text starts
  on the first content row and the progress line now uses compact `× Error`
  state while the transcript row keeps the detailed provider error.
- `tk-jwfs` timestamp preservation gates passed:
  `go test ./internal/session ./internal/storage ./internal/app ./internal/backend/canto -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`,
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`,
  and a fresh tmux launch smoke showing the normal startup/progress/footer shell
  without visible timestamps.
- OpenRouter `deepseek/deepseek-v4-flash` live smoke passed after timestamp
  changes. It proved a real `bash` tool call, persisted resume,
  provider-history ordering, and nonzero host timestamps on streamed deltas and
  token usage. The model answered `fresh`, but provider-history capture
  verified prior tool history, so this is classified as a model semantic miss.
- Fork/timestamp audit gates passed:
  `go test ./internal/storage ./internal/backend/canto -run 'TestCantoStore(ForkSessionCopiesEventsAndIndexesChild|SessionBundleExportsAndImportsLineage)|TestSubagentForkContextUsesProviderVisibleParentSnapshot' -count=1 -timeout 180s`,
  Canto `go test ./session ./runtime -count=1 -timeout 180s`,
  Ion `go test ./... -count=1 -timeout 300s`, and the Ion native race subset.
- Canto `e880c1c` fixes the harness ownership boundary: `Harness.Close` closes
  the runner and only closes a session store the harness created itself. Ion
  imports that revision.
- `CantoBackend` now builds a Canto `Harness` in `Open()` and consumes the
  harness `PromptStream` event stream in `SubmitTurn()` instead of manually
  pairing `Runner.Watch` with `SendStream` in separate goroutines.
- Caller-context cancellation now settles the Ion host turn with a single
  `TurnFinished` even when cancellation stops the stream before a durable
  terminal Canto event reaches the host.
- Removed leftover cached `runner`, `agent`, and `stopWatch` fields from
  `CantoBackend`; opt-in subagents now use the active harness when they need
  child delegation or parent instructions.
- Latest harness-boundary gates passed:
  `go test ./internal/backend/canto -count=1`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
- Fedora local-api probe timed out from this machine. OpenRouter DeepSeek live
  smoke passed with `deepseek/deepseek-v4-flash`: real `bash` tool call,
  persisted resume, provider-history capture, and resumed follow-up request
  ordering verified.
- Tmux TUI smoke passed for fresh launch, `/tools`, `/settings`, and a live
  auto-mode bash turn against OpenRouter DeepSeek Flash with compact
  `Bash(echo ion-tmux)` display and clean completion shell.
- `tk-g5sf` minimal-core cleanup is closed. The default model-visible
  surface is now exactly eight tools (`bash`, `read`, `write`, `edit`,
  `multi_edit`, `list`, `grep`, `glob`); stale `verify`, model-visible
  `compact`, and memory command/backend hot-path scaffolding have been removed.
- OpenRouter live smoke with `deepseek/deepseek-v4-flash` passed: real `bash`
  tool call, persisted resume, provider-history capture, and resumed follow-up
  with prior tool history in the request.
- Tmux shell smoke now confirms `/help` and `/tools` print above the
  progress/composer/footer shell, with one blank row before `Ready`.
- Removed the stale self-initiated model-visible `compact` tool spec; current
  compaction design is host `/compact` plus overflow recovery.
- Removed unreachable `/mcp` and `/rewind` dispatcher implementations. They
  remain rejected at the deferred command catalog boundary instead of living as
  hidden future-edition code.
- Removed the unused MCP registration method from the host `AgentSession`
  boundary and native backend, since no active Ion command can call it.
- Local command errors now print once to scrollback without leaving the progress
  line in a duplicate `Error` state.
- Latest gates passed for the shell/command cleanup:
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
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

1. Start `tk-omw4` to split oversized app tests by behavior without changing
   behavior.
2. Implement `tk-hdwz` if we want the small session-end resume hint before the
   larger test split.
3. Retry deferred `tk-jkcl` Fedora C2 live smoke only when local-api is
   reachable.

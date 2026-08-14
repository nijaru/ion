# Ion Agent Instructions

## Mission

Ion is a native Go terminal coding agent and TUI. The goal is a complete,
stable, correct, elegant, optimized, idiomatic daily-driver agent—not merely a
green demo or a collection of feature checkboxes.

Ion is v0.0.0. Clean breaks are allowed. There are no Pi compatibility
requirements: no Pi file formats, arguments, JSONL/session protocols, package
names, or compatibility shims.

The required core workflow is:

submit → stream → tool call/result → steer/follow-up → cancel/error → persist → replay/resume

It must be correct under normal use, restart, cancellation, provider failure,
tmux, the race detector, and live providers.

## Session start

Run these before making claims or choosing work:

    cat ai/brief.md
    tk ready
    git log --oneline -10
    git status --short

Read ai/DESIGN.md before structural work. Read the relevant task, tests, and
recent journal/decision entries. Repo-local code, tests, ai/, and tk outrank
stale chat context.

At the start of a continuing session, identify the active task, its acceptance
gate, the current commit, and the first unfinished dependency. If the active
task is still the highest-priority unblocked work, continue it; do not reopen a
completed design task or start a more visible but lower-priority feature. If
the task is blocked, record the concrete blocker and advance the next safe
dependency rather than improvising around it.

## Product and reference posture

Pi is the closest behavioral reference because it is a provider-agnostic agent
harness. Use its current source and SDK/release documentation to recover
invariants and solved product problems. Also inspect Codex CLI/Desktop,
Claude Code, Cursor, Zed, and other strong references for safety, CLI/TUI,
review, settings, integrations, and workflow patterns.

References provide evidence about behavior and tradeoffs, not a specification
to copy. Translate the invariant into an Ion-owned contract and choose the
idiomatic Go implementation. A reference feature is not automatically an Ion
requirement; a missing reference feature is not automatically a reason to add
one.

Ion is a developer-first terminal agent harness, not an over-prescriptive web
assistant or sandbox toy. Do not impose artificial role boundaries, read-only
handicaps, or bureaucratic gating on subagents and tools (such as stripping
`bash` from exploration/diagnostic child runs or blocking compiler/git
inspection). In trusted workspaces, subagents inherit the user's host
capabilities to reproduce failures, run tests, and diagnose systems freely.

## Required order of work

For any substantial product or structural change:

1. Reference research: inspect authoritative/current source or docs and record
   the observable behavior and invariant.
2. Ideal target: define the desired Ion behavior, ownership, lifecycle, failure
   semantics, safety semantics, and UX before reading existing code as a design
   guide. ai/DESIGN.md is the authoritative target.
3. Implementation audit: map current code to the target using grep, imports,
   call sites, tests, and runtime evidence. Classify each area as keep,
   refactor, delete, or rewrite. Do not infer quality from filenames, line
   counts, or passing legacy tests.
4. Migration/rewrite: fix ownership and contracts first. Delete obsolete paths
   in the same change. A full rewrite of a bad subsystem is preferable to
   preserving its shape with wrappers or transitional layers.
5. Behavioral proof: add tests for the target contract, then run the affected
   serial, race, TUI/tmux, provider, security, and performance gates.
6. Adversarial review: have an authorized native Sol reviewer challenge the
   design and diff at important boundaries. Reconcile findings before calling
   the slice complete.
7. Persist and save: update ai/brief.md, ai/decisions.md, ai/journal.md, and tk
   when state or architecture changes materially; commit each coherent working
   chunk.

Do not start broad feature work while the product charter or target
architecture is unresolved. Do not let the current implementation define the
ideal merely because it already exists.

Before editing a substantial slice, write down the target invariant, its single
Ion owner, the failure/recovery behavior, and the behavioral evidence that will
close the slice. During implementation, keep the change on that slice. Expand
scope only when deleting or refactoring an obsolete owner is required to make
the target contract true.

## Priority and scope

Choose the next slice from the highest-priority open gate, not from the most
visible missing feature:

1. Correctness and ownership: lifecycle, persistence, replay/resume,
   cancellation, failure recovery, and resource cleanup.
2. Safety: explicit permission, canonical action identity, auditability,
   sandbox enforcement, and fail-closed behavior.
3. Core daily-driver UX: submit/stream/steer/follow-up in TUI, CLI, and print
   modes through the same runtime contract.
4. Provider and integration completeness: real adapters, auth/configuration,
   MCP, and useful host capabilities.
5. Measured performance and polish: latency, allocations, throughput,
   terminal rendering, accessibility, and documentation.

Do not use lower-priority polish to conceal an unresolved correctness or safety
gate. Keep each slice narrow enough to finish and prove, but rewrite an entire
owner when a local patch would preserve duplicated state or an unsafe boundary.
Deferred work is acceptable only when it is recorded in tk with an owner,
reason, and unblock condition. `archive/` and `ai/archive/` are historical
material, not implementation or architecture sources.

Priority is a dependency order, not a request to optimize every dimension at
once: unresolved correctness, safety, or ownership gates block dependent UX,
provider breadth, and polish. Within one priority, choose the smallest slice
that removes a real gate and produces user-visible or testable progress. Do
not spend a session trading prose, abstractions, or benchmarks for a missing
owner or an unverified lifecycle guarantee.

## Quality bar

- One owner for every invariant, state transition, and persistence guarantee.
- No duplicate domain models, event streams, transcripts, caches, policy
  engines, runtime composition roots, or cleanup paths.
- No speculative abstractions, dead configuration, compatibility aliases,
  fallback branches, TODO-based architecture, or “temporary” v2 files.
- Rewrite repeated or tangled code when local patches would preserve bad
  ownership. Do not optimize a subsystem before its contract is correct.
- Prefer simple, explicit Go: small packages, concrete types at ownership
  boundaries, narrow interfaces at consumption boundaries, context-aware I/O,
  typed errors, deterministic ordering, bounded queues/loops, and clear
  shutdown semantics.
- Let errors propagate. Recover only to add useful context or to implement a
  defined recovery contract. Never silently downgrade a failed write,
  approval, cancellation, teardown, or provider request.
- Performance claims require benchmarks or profiles. Keep hot paths measured,
  bounded, allocation-aware, and free of avoidable global locks/state.
- Tests must verify behavior and invariants, not names, existence, or legacy
  implementation details.

## Go formatting and static checks

The repository formatter is `golines` with `gofumpt` as its base formatter.
Use the checked-in wrapper so local and CI commands share the same file set and
line-length policy:

    scripts/go-format.sh
    scripts/go-format.sh --check

The default maximum line length is 120 and can be overridden for a deliberate
local experiment with `ION_GO_MAX_LEN`. Do not use plain `gofmt` as the final
formatter. Keep formatting-only changes in their own commit, separate from
behavioral refactors, so review can distinguish mechanical movement from
ownership or correctness changes.

Quality-convergence slices should also run `go test ./...`, the relevant race
suite, `go vet ./...`, `staticcheck ./...`, and `govulncheck ./...`. A check that
is not yet green belongs in the active `tk` task with its concrete finding and
unblock condition; do not hide an unresolved gate behind a broad cleanup.

## Target architecture constraints

ai/DESIGN.md is the current authoritative architecture. If the ideal target
changes, update it by explicit decision before changing structural code.

- session/ owns the typed domain model, session tree, durable entries, and
  storage. The session tree is the source of truth for conversation state;
  runtime owns the live lifecycle event stream.
- internal/agent/loop.go is a stateless turn engine. It receives all inputs,
  persists nothing, owns no session/store, and emits typed events.
- The runtime controller is the sole stateful owner of active session state,
  turn phases, tools, queues, context, compaction/recovery,
  model/thinking/tool state, hooks, policy, event ordering, and persistence
  coordination. `internal/agent/loop.go` remains a stateless turn engine;
  `Controller` is the concrete runtime implementation of this contract.
- Frontends consume `agent.Runtime` for submit/stream/control and request
  narrow optional capabilities for session administration. Do not recreate a
  broad `Runner` interface or make the TUI depend on controller internals.
- Public runtime operations must enter through the bounded controller command
  queue; private direct operations may perform owned external work only after
  acceptance. Every operation needs a typed result or error, and no void
  setter may silently lose input after close or queue saturation.
- Runtime event delivery is an Ion-owned snapshot-plus-cursor subscription,
  not a shared channel or callback registry. Each subscriber has an
  independent bounded stream; a slow subscriber is explicitly detached with
  `ErrSubscriptionLagged` and must resubscribe from the authoritative runtime
  snapshot. Do not reintroduce `Events()`, injected event channels, listener
  callbacks, or frontend-side transcript recovery.
- The host/composition root owns process lifetime, provider/auth/model
  construction, resource loading, runtime replacement, teardown, and CLI mode
  selection. `Runtime.Close` stops the controller; host-owned resource handles
  are closed separately through the runtime's resource capability after that
  boundary. No hidden package-global host state or controller-owned final
  close.
- app/ is a Bubble Tea v2 projection/control layer. It owns view state and
  user intent, not a second agent loop, transcript, session materializer, or
  hidden runtime.
- llm/ owns provider-neutral wire contracts and real provider adapters.
  Catalogs and endpoint resolvers must have explicit host ownership. Catalog
  metadata may describe only executable provider behavior.
- tool/ owns tool contracts and execution adapters. tool/mcp/ owns MCP client
  lifecycle; MCP state is runtime state, not session content.
- Instructions, skills, memory, checkpoints, jobs, and other auxiliary
  services must each have one explicit owner and a documented relationship to
  session history and prompt context.

The current safety implementation is a baseline, not automatically the final
policy. As of 2026-07-29, the P1 security reference is current Pi: project
trust gates project-local settings and resource loading; ordinary local tools
run with the launching user's permissions; and there is no mandatory
per-action approval or in-process sandbox. Ion applies that host-owned trust
boundary to project settings, instructions, skills, prompts, themes, packages,
and extensions (including instruction files because they affect model policy).
Ion implements the current resource boundary with persisted
`--trust` decisions for the working directory and descendants; untrusted
non-interactive runs skip project-local instructions, prompts, and skills.
Codex remains a secondary reference for reviewable action identity, recovery,
and cancellation. Do not add or preserve stronger policy machinery by
momentum when it has no current Ion owner and user-facing purpose.

Keep the action planner and journal only where an external-effect contract
requires durable prepared/started/outcome evidence or indeterminate recovery;
delete unused or speculative paths rather than carrying them for P2. Explicit
confirmation or sandbox modes must fail clearly when selected but unavailable.
Ordinary trusted execution must not pretend to be sandboxed. An effect must
not bypass any policy or recovery boundary that its actual contract requires.

The same ownership rule applies to every cross-cutting guarantee: session
durability belongs to `session/`, live lifecycle ordering to the runtime
controller, provider translation to `llm/`, OS enforcement to the sandbox/tool
boundary, and rendering to `app/`. When two layers appear to own a guarantee,
stop and resolve the ownership before adding another callback, wrapper, cache,
event, or compatibility path.

## Work tracking and review

Use tk for multi-step work. The current overall goal is tk-2lt7. The
reference-backed product charter is captured in
ai/research/agent-reference-matrix.md; the ideal architecture is frozen in
ai/DESIGN.md and tracked by completed task tk-uyfo. The implementation audit
tk-03v2 and session durability task tk-mkmt are complete; the active bounded
implementation task tk-k6gf (core daily-driver proof) is complete. The Go
quality convergence task tk-1qp1 and Pi security posture task ion-j2wv are
complete. The active P1 task is tk-l6cq, which closes the explicit project
resource-trust boundary and its behavioral gates. The live-provider task
tk-h5yr is deferred, as are P2 queued-input and cache/compaction follow-ups;
do not select deferred work merely because a reference feature is visible.
Keep tasks atomic, demoable, and acceptance-tested. Log findings while fresh,
and do not reopen completed design work unless new evidence requires an
explicit target change.

Use native Codex subagents when a second opinion materially improves an
important architecture, safety, or code-review decision. The user has
authorized Sol reviewers. Do not route Codex-available GPT models through Pi;
use Pi only when explicitly requested or when a non-native model is the
deliberate choice. A reviewer must report evidence, changed files (if any),
commands, findings, risks, and unresolved questions.

When continuing an existing goal, verify the baton first, then advance the
highest-priority unblocked slice materially. Do not spend the session merely
rewriting status prose, reopening completed design work, or declaring a phase
complete without its acceptance evidence. At a clean boundary, the current
commit, active task, remaining gates, and exact verification commands must be
recoverable from `ai/brief.md`, `handoff.md`, and tk.

For each coherent chunk, use this loop:

1. Read the target and current owner; inspect call sites and relevant tests.
2. Make the smallest correct implementation or rewrite, deleting obsolete
   paths in the same change.
3. Run focused behavioral tests immediately; add failure-injection tests for
   lifecycle, persistence, cancellation, safety, and cleanup changes.
4. Review the diff for duplicate ownership, accidental compatibility, silent
   fallback, unbounded work, and stale context claims.
5. Run proportional repository gates, log the evidence in tk and ai/, and
   commit the working chunk before switching slices.

Green compilation or a legacy test suite is only a compatibility signal. A
slice is complete only when its target invariant, failure behavior, and stated
acceptance evidence are all demonstrated. If the tree cannot be left clean,
leave an explicit handoff with the exact uncommitted files and next command.

## Verification

Evidence means:

- reference source/docs with a direct citation;
- Ion source with file and line;
- actual test command and output;
- behavioral terminal/TUI/provider proof.

Minimum gates for core changes:

    go test ./... -count=1 -timeout 300s
    go test -race ./internal/agent/ ./session/ ./app/ ./llm/ -count=1 -timeout 180s
    go vet ./...

TUI changes require deterministic reducer tests plus tmux acceptance. Provider
or lifecycle changes require the relevant fake, failure-injection, restart,
and live-provider evidence. Security changes require allow/deny, boundary,
cancellation, shutdown, and non-interactive tests. Performance changes require
before/after benchmarks or profiles.

Never claim completion from a checklist alone. If a gate is skipped, state why.

## User regressions and stop conditions

Treat every user-reported behavior problem as a regression until disproven.
Search tk, ai/journal.md, recent commits, and relevant tests before answering
that it is fixed. Create a task when no record exists.

Stop and re-audit when:

- a phase is declared complete repeatedly while new defects appear;
- a change adds a wrapper, duplicate state, global mutable seam, or temporary
  compatibility path;
- a design claim lacks grep/source/test evidence;
- a user asks “are you sure?”;
- the next implementation step depends on an unresolved product or threat-model
  decision.

## Active context files

- ai/DESIGN.md — authoritative target architecture
- ai/research/agent-reference-matrix.md — consolidated reference evidence and
  product charter
- ai/brief.md — concise current state and roadmap
- ai/decisions.md — durable architectural decisions
- ai/journal.md — append-only evidence and findings
- handoff.md — ephemeral next-session baton
- .tasks/ via tk — executable work queue

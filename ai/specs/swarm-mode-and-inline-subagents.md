# Swarm Mode And Inline Subagents

Updated: 2026-05-02

## Answer

Yes, we should wait to implement a full swarm dashboard until the regular inline TUI and the ion agent path are stable enough.

But we should design the current inline TUI with that future in mind now.

Pi is a useful benchmark for what "mature enough" can look like, but it is not a hard parity gate for this work.

The near-term direction is:

- keep inline chat as the primary mode for normal ion use
- add subagent visibility to Plane B as ephemeral live state, not transcript noise
- reserve an alternate-screen view for explicit orchestration or mission supervision later
- do not replace inline chat with a dashboard

Current implementation note: Ion already has inline Plane B subagent progress
rows, concise durable child breadcrumbs, built-in `explorer`, `reviewer`, and
`worker` personas, and Canto child-session hook points. The model-visible
`subagent` is not registered by default because adding a ninth model-visible
tool should be an explicit choice. It is available through
`subagent_tools = "on"`.

The `summary` / `fork` / `none` context modes now exist at the tool boundary.
Full alternate-screen swarm mode waits until opt-in subagent usage is boring.

## Product Boundary

### Inline mode

Inline mode is for:

- direct user-to-agent work
- one active main conversation
- a small amount of live orchestration context
- lightweight awareness of subagents without switching cognitive modes

Inline mode should remain the default experience.

### Swarm mode

Swarm mode is a future alternate-screen operator view for:

- supervising multiple concurrent agents
- tracking tasks, retries, failures, alerts, and handoffs
- understanding spatial relationships across workers
- operating at a mission level rather than a single conversation level

Swarm mode should be additive, not a replacement for inline chat.

## Why Not Build Swarm Mode First

Swarm mode is higher complexity and wants stable primitives underneath it:

- subagent lifecycle
- sync vs async child execution
- durable child result semantics
- retry and failure semantics
- compact status surfaces in the inline TUI

Until those are stable, the right move is to keep inline mode primary and let
the swarm dashboard remain a design target.

## Inline Subagent Display

### Current state

Ion's TUI already has a small inline subagent display path:

- live child progress stays in Plane B
- at most three active rows render before a collapsed `+N more` summary
- started/completed/failed child events can persist as concise breadcrumbs
- child deltas stay ephemeral by default

That is enough for the near-term inline product. It is not enough to expose
subagents by default, because the child context contract is still missing.

### Where it belongs

Active subagents should render in Plane B, not in committed scrollback by default.

Recommended Plane B order:

1. in-flight main-agent content
2. active subagent lines
3. one blank spacer
4. progress line
5. top separator
6. composer
7. bottom separator
8. status line

This keeps live orchestration visible without turning transcript history into a task log.

### What each subagent line should show

Each visible subagent row should stay compact and answer four questions:

1. which worker is this
2. what was it asked to do
3. what is it doing right now
4. what is the most relevant current detail

Suggested row shape:

`worker-2  edit  policy migration  running go test ./...`

Fields, in priority order:

- worker label
- agent type or role, if meaningful
- short assignment or intent
- live phase
  - `thinking`
  - `tool`
  - `streaming`
  - `waiting`
  - `done`
  - `failed`
- one salient detail
  - active command
  - active tool
  - short current step

Do not show:

- full streaming prose from every worker
- raw JSON args
- multi-line logs by default
- repeated noisy deltas

### How many to show

Inline mode should not try to display every worker equally.

Recommended default:

- show up to 3 active subagent rows directly
- if more exist, collapse the remainder into a summary row like `+4 more workers`

Prioritize visible rows by:

1. sync workers the main agent is currently blocked on
2. workers in error or waiting state
3. workers with fresh tool activity
4. oldest still-running workers

This keeps Plane B small and preserves the inline experience.

## Transcript Policy For Subagents

### What should be durable

Subagent lifecycle should still leave durable breadcrumbs in scrollback.

Keep these:

- subagent started
- subagent completed
- subagent failed
- subagent cancelled

These durable entries should be concise.

Examples:

- `Started worker-2: inspect approval flow`
- `Completed worker-2: found policy mismatch in canto backend`
- `Worker-3 failed: context overflow during model fetch`

### What should stay ephemeral

Do not durably log:

- every child delta
- every child step change
- every intermediate command or progress update

That information belongs in Plane B while live, and later in swarm mode if the user explicitly wants orchestration detail.

## Sync And Async Subagents

### Sync children

Sync children are part of the main agent's current critical path.

Implications:

- show sync children at the top of the active subagent block
- progress line should reflect when the parent is waiting on them
- completion should immediately unblock the parent

### Async children

Async children can continue in the background while the main inline conversation remains usable.

Implications:

- show them in the active subagent block while running
- when they finish, emit a concise durable completion row
- surface a lightweight notification or badge in Plane B if the result has not been consumed yet

Default rule:

async child completion should not automatically re-enter or wake the main agent in inline mode.

Reason:

- unsolicited agent wakeups are surprising in a direct chat UI
- they blur the boundary between user-driven chat and orchestration mode

Future swarm mode can support orchestrator-driven automatic continuation, but inline mode should stay conservative.

## Session Titles And Lightweight Summaries

### Current state

Today the resume picker uses `LastPreview`, which is just the latest durable user or agent text stored in session metadata.

That is useful, but it is not a real conversation title or summary.

### Desired direction

Add a cheap one-shot summary path for session metadata generation.

This should be:

- non-streaming
- schema-constrained
- short
- async or idle-triggered
- debounced so it does not fire on every small turn

Good targets:

- session title
- one-line session summary
- maybe a short topic tag

Suggested generation times:

- after the first meaningful assistant reply
- after a large topic shift
- after a turn finishes and the session becomes idle
- on resume-list rendering only as a fallback, not the primary path

Suggested storage:

- add explicit session metadata fields for title and summary
- keep `LastPreview` as a fallback, not the primary label

### Model strategy

These requests should be cheap and bounded.

Use:

- a small non-streaming request
- a tiny structured schema
- low token budget
- a cheaper or faster model if the runtime supports it

The goal is metadata quality, not prose quality.

Tracked by: `tk-4ywr`

## Naming

`Swarm mode` is a good working name.

Why it works:

- short
- apt for orchestration
- clearly distinct from normal chat

Alternatives like `mission mode` or `ops view` can stay open for later naming review, but `swarm` is a good spec-level name now.

## Near-Term Plan

### Do now

- keep inline mode primary
- keep the current inline Plane B rows as the normal near-term display surface
- keep `subagent_tools = "on"` as the explicit opt-in path for model-visible
  subagent registration
- keep swarm mode as a design target, not an implementation priority
- keep this document as the target design for later subagent and swarm work
- design title and summary generation as lightweight session metadata

### Defer

- full alternate-screen swarm dashboard
- spatial task board or mission grid
- automatic main-agent wakeup on async child completion
- transcript-level detailed child execution logs

Closed prerequisite: `tk-29xj` - Subagents: expose gated subagent tool after
context-mode smoke.

Closed gate: `tk-pwsl` confirms the alternate-screen operator view is not the
next implementation slice. Ion should finish context modes and default
subagent registration before revisiting a full swarm surface.

## Files To Revisit During Implementation

- `internal/app/model.go`
- `internal/app/broker.go`
- `internal/app/render.go`
- `internal/app/viewport.go`
- `internal/app/session_picker.go`
- `internal/storage/canto_store.go`

# Harness Engineering Patterns

Notes for ion from an April 2026 public write-up by a researcher who ran a 13-stage
open-source contribution pipeline across 100+ repos in 72 hours (500+ commits, many
merged by maintainers of major projects; account suspended for velocity, not quality).
Source: twitter.com/JunghwanNa8355/status/2046224197672984824

The tooling details (OMX, Ouroboros, MCP state) aren't the interesting part. The
_method_ is — it translates cleanly to any coding agent, including ion running
headless on real repos.

## Patterns worth evaluating

### 1. External source of truth as resumable checklist

Each pipeline run stored its progress as a checkbox list on a GitHub issue. Any fresh
agent session could load the issue, see which stages were done, and resume without
inheriting conversation state.

**Why it matters for ion:** sessions compact, TUIs get closed, headless runs get
killed and restarted. If resume state lives only in the canto event log, recovery
requires the same process or a store handle. If it _also_ lives in a human-readable
external artifact (issue, PR description, `STATUS.md` in the workspace), a fresh ion
invocation — or a human — can pick up mid-task with no ceremony.

**Shape to consider:** for long-horizon ion runs (multi-PR work, multi-day refactors),
write a checklist artifact into the workspace or issue on each stage transition. The
canto event log remains truth; the external checklist is a projection optimized for
human + cold-start agent consumption.

### 2. Reproduction gate before fix

Stage 5 of the pipeline: if the candidate bug doesn't reproduce locally in a fork,
drop it. No hypothesis-as-fact. The author credits this single gate with most of the
merge-rate lift.

**Why it matters for ion:** "the AI proposed a fix that looked right but didn't
address the actual bug" is the dominant failure mode of coding agents. A deterministic
reproduction step — run the failing test, confirm it fails for the claimed reason —
before any edit turns a speculative loop into a grounded one.

**Shape to consider:** make reproduction a first-class node in ion's workflow, not a
suggestion in the system prompt. For bug-fix tasks, the agent should not be able to
propose an edit until it has produced a failing test or reproduction command and ion
has observed it fail. This is an `x/graph` node with a veto edge.

### 3. Merge-pattern matching over CONTRIBUTING.md

Stage 8: reading the last ~10 merged PRs of the target repo shaped output better than
reading CONTRIBUTING cover to cover. Accepted social shape is etched more clearly in
merge history than in docs.

**Why it matters for ion:** when ion contributes to an unfamiliar repo, few-shot
exemplars from that repo's own recent merges outperform static style rules. The repo
teaches itself.

**Shape to consider:** when ion operates on a repo for the first time, have it fetch
and cache recent merged PRs (title + body + diff summary) as retrievable exemplars in
canto's `memory/` layer. Wire them into the context pipeline as a `RequestProcessor`
ahead of any generic coding-style guidance. Don't hardcode precedence — let retrieval
relevance win.

### 4. Human-gate placement

Stages 11 and 12 were _explicitly_ human steps: viability review, CLA signing. The
pipeline was designed to park cleanly at every point where a human's name goes on the
line, not to automate through them.

**Why it matters for ion:** the temptation with a capable agent is to push automation
as far as it will go. The author's lesson: the _attestation_ surfaces — commits under
the user's name, PR submission, merges, destructive operations — are not friction to
engineer away, they're the load-bearing trust primitives.

**Shape to consider:** ion should have a small, explicit set of operations it will
never perform without an in-the-moment human confirmation, regardless of prior
approvals or config. Candidates: push to remote, open PR, force-push, any `rm -rf`
outside a scratch dir, merging, releases, destructive git operations (reset --hard,
branch -D). Headless runs that hit these gates park and wait, they don't proceed.

This pairs with pattern #1: when ion parks on a human gate, the external checklist
should make the gate state visible so the human knows what they're confirming.

### 5. Velocity as semantic signal

The suspension wasn't about any individual PR's quality. It was about 100 repos in 72
hours reading as spam at the abuse-detection layer, regardless of per-PR merit.

**Why it matters for ion:** any coding agent that fans out across external platforms
(GitHub, package registries, issue trackers, CI systems) will be read by those
platforms' abuse layers before it's read by humans. Legible pacing — deliberate delays,
batching that looks human-shaped — is a correctness property, not a politeness one.

**Shape to consider:** for multi-repo or multi-PR ion workflows, pacing policy should
be explicit and conservative by default. This is purely an ion concern; canto provides
the runtime lanes, ion decides the cadence.

### 6. Pipeline as amplifier, not replacement

The author's framing: "the gap between expert-with-harness and novice-with-harness is
wider than the gap between novice-with-harness and no harness." The pipeline amplifies
whatever judgment the operator brings. It does not substitute for judgment.

**Why it matters for ion:** design decisions should assume an expert operator as the
target, not a beginner. Progressive disclosure should expose more power, not hide the
pipeline. Defaults should be safe, but expert users should be able to drive the full
machinery without the TUI abstracting it away.

## What's explicitly _not_ a canto concern

The following are all ion-level (workflow, policy, or conventions), not canto
framework primitives. Canto should provide mechanisms; ion chooses policy.

- The 13-stage pipeline shape itself
- Reproduction-before-fix gate placement
- Which operations require human attestation
- Retrieval weighting in the context pipeline
- Pacing policy for external platforms
- External-checklist format (issue checkboxes vs workspace markdown vs something else)

## Canto audit item surfaced by this review

One contract worth verifying in canto (not adding new primitives — just confirming the
existing mechanism is clean):

- **External resume safety.** Can an external process holding a store handle call
  `Send(ctx, sessionID, event)` on a session that isn't currently in the runner's
  in-memory registry, without racing a concurrent load? This is the mechanism that
  makes pattern #1 (external checklist resume) and #4 (park on human gate) work. If
  the contract holds, no canto work is needed; document it in canto's DECISIONS.md.
  If there's a race, fix it minimally — do not add a "wait" event type or a park/resume
  helper. Policy stays in ion.

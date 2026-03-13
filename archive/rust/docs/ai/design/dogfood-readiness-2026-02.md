# Dogfood Readiness Spec (2026-02)

## Objective

Use ion as the primary tool to work on ion without major reliability friction.

## Scope

### In Scope

- TUI correctness and stability for day-to-day usage
- Agent usability for multi-hour sessions
- Lean automated checks that do not distort architecture
- Hardening tasks that reduce regressions in core flows

### Out of Scope

- Headless mode (explicitly deferred)
- Large testing harness/framework work that adds architectural drag
- New product-surface experiments unrelated to stability

## Constraints

1. Keep ion lean. Do not re-architect primarily for testability.
2. Prefer source-level fixes to rendering band-aids.
3. Add automation only where cheap and structurally natural.
4. Manual verification is acceptable for visual behavior that is expensive to assert robustly.

## Readiness Definition

Ion is dogfood-ready when all gates below pass for 5 consecutive days of active use:

1. TUI gate:
   - No header pinning/blank-line regressions in `--continue`, `/resume`, `/clear`, and resize.
   - No terminal corruption on cancel/quit/editor suspend-resume.
2. Agent gate:
   - Reliable streaming, tool-call display, cancellation, retry, and error surfacing.
   - Model/provider switching and session persistence behave predictably.
3. Safety gate:
   - Sandbox behavior works as designed for normal coding workflows.
   - Git/tool safety expectations are clear and enforced.
4. Regression gate:
   - Unit tests cover core render-state transitions and planner behavior.
   - Targeted manual checklist passes before merges that touch TUI state/render paths.

## Testing Strategy (Lean)

### Automated (keep)

- Unit tests for pure/pure-ish render state logic (`run.rs` planning, `render_state.rs` transitions)
- Existing integration/unit suites (`cargo test`)

### Automated (add only if easy)

- Small non-visual smoke checks (launch, command entry, clean exit, no panic)
- Minimal regression tests for newly fixed state-order bugs

### Manual (primary for visual correctness)

- Scripted checklist for:
  - startup `--continue` with short and long histories
  - in-app `/resume`
  - `/clear`
  - resize behavior in idle and running states

## Effort Guide for Rendering Tests

| Level | What | Effort | Recommendation |
| --- | --- | --- | --- |
| L0 | Unit tests on planner/state | 0.5-1 day | Do now |
| L1 | Simple non-visual smoke script | 0.5 day | Do if low churn |
| L2 | Robust PTY + terminal-state assertions | 2-4 days | Defer unless regressions persist |
| L3 | Full visual/snapshot TUI framework | 1-2 weeks | Do not do now |

Decision: stay at L0/L1 for now.

## Key Risks

1. Hidden state-order bugs across `prepare -> plan -> render` transitions
2. Resize and selector transitions reintroducing scrollback artifacts
3. Over-investment in harnesses that become brittle and expensive to maintain
4. Stability work competing with core usability blockers

## Prioritized Focus Areas

1. TUI render-state transition correctness
2. Sandbox execution and safety defaults
3. Agent reliability and recoverability under long sessions
4. Lightweight regression protections for recently fixed bugs

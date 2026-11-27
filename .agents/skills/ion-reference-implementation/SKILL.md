---
name: ion-reference-implementation
description: Use when changing or reviewing an Ion runtime, Session, tool, terminal, or client protocol contract.
---

# Ion behavior changes

Start with Pi for coding and terminal behavior, Pi Durable for hosting and
recovery, and fx for visual treatment. Use Codex or other references for a
specific problem, not as a second feature list.

- Pick work from actual coding friction or correctness problems, not missing
  upstream features.
- Trace the affected clients to the owner of the fact or effect. A queued
  input is not a durable acceptance, provisional output is not history, and a
  stop request is not joined completion. Preserve these distinctions through
  errors, cancellation, and reopen.
- Validate and bound external input at the owning boundary. Carry accepted
  state in concrete Rust types. Don't use a panic or success-like default for
  malformed input, storage failure, or an uncertain effect.
- When changing an async boundary, check what drop or cancellation means before
  dispatch, during work, and after commit before acknowledgment. Retain task
  custody through join; don't hold a synchronous guard or transaction across
  external I/O. Keep native files, runtime-specific timers, and protocols out
  of shared decisions.
- Verify the real entry point against the relevant independent result:
  transmitted request, filesystem effect, committed Session fact, or terminal
  behavior. Prefer a controlled failure at the affected boundary over timing
  sleeps. For native interaction, exercise a real terminal. A build or fixture
  does not qualify behavior it didn't exercise or establish daily-use readiness.
- For a new or widened runtime dependency, explain why it's needed and review
  the resolved feature and lockfile diff on the affected hosts. Keep native,
  hosted, and test dependencies in their respective paths. Never pull in a
  second agent engine to add a client or host.

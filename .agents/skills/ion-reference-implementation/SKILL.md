---
name: ion-reference-implementation
description: Use when changing or reviewing a runtime, Session, tool, or terminal contract in Ion.
---

# Ion behavior changes

- Before choosing a user-facing design, compare the actual Ion and Pi workflow.
  Follow Pi unless a concrete user need, correctness constraint, or measured
  cost makes another approach better. Prefer fx for terminal appearance; use
  Codex, Pi Durable, fx, and other strong references wherever they help. Inspect
  the relevant source and behavior, not just feature lists.
- Start with the proven interaction. Explain material departures before
  building them: what breaks if Ion follows the reference, and what evidence
  supports the alternative? Different internals, a constructed edge case, or
  passing tests alone do not justify a new user-facing workflow. Simplify
  existing departures that lack a reason instead of adding controls to them.
- Trace the affected clients to the owner of the fact or effect. A queued
  input is not a durable acceptance, provisional output is not history, and a
  stop request is not joined completion. Preserve these distinctions through
  errors, cancellation, and reopen.
- Verify the real entry point against the relevant independent result:
  transmitted request, filesystem effect, committed Session fact, or terminal
  behavior. For native interaction, exercise a real terminal as well as
  automated checks. Functional evidence is not sustained-use acceptance.

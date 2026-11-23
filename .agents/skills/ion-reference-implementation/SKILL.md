---
name: ion-reference-implementation
description: Use when changing or reviewing a runtime, Session, tool, or terminal contract in Ion.
---

# Ion behavior changes

- Name the workflow and the behavior that must change. If an upstream
  comparison informs the decision, inspect the relevant callers and cite the
  revision. Choose sources for the question; do not chase parity.
- Trace the affected clients to the owner of the fact or effect. A queued
  input is not a durable acceptance, provisional output is not history, and a
  stop request is not joined completion. Preserve these distinctions through
  errors, cancellation, and reopen.
- Verify the real entry point against the relevant independent result:
  transmitted request, filesystem effect, committed Session fact, or terminal
  behavior. For native interaction, exercise a real terminal as well as
  automated checks. Functional evidence is not sustained-use acceptance.

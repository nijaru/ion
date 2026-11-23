---
name: ion-reference-implementation
description: Use when changing or reviewing a runtime, Session, tool, or terminal contract in Ion.
---

# Ion behavior changes

Pi Durable is a strong reference for runtime and recovery. fx is useful for
terminal presentation; Codex and other agents can offer alternatives. Use the
comparisons that help with the decision at hand.

- Pick work from actual coding friction or correctness problems, not missing
  upstream features.
- Trace the affected clients to the owner of the fact or effect. A queued
  input is not a durable acceptance, provisional output is not history, and a
  stop request is not joined completion. Preserve these distinctions through
  errors, cancellation, and reopen.
- Verify the real entry point against the relevant independent result:
  transmitted request, filesystem effect, committed Session fact, or terminal
  behavior. For native interaction, exercise a real terminal as well as
  automated checks. Functional evidence is not sustained-use acceptance.

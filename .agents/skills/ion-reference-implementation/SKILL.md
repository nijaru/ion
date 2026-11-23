---
name: ion-reference-implementation
description: Use when changing or reviewing a runtime, Session, tool, or terminal contract in Ion.
---

# Ion behavior changes

- Start from a real Ion workflow or correctness problem, not a parity gap.
  Use Pi as the default reference: inspect its actual workflow and source and
  use what fits. Diverge when a different choice better serves Ion's users,
  Rust architecture, or durability guarantees. Explain material trade-offs.
- Use Pi Durable as a strong reference for runtime and recovery. Prefer fx's
  terminal presentation where it fits, and consult Codex and others when they
  offer a useful comparison. No reference is a specification.
- Trace the affected clients to the owner of the fact or effect. A queued
  input is not a durable acceptance, provisional output is not history, and a
  stop request is not joined completion. Preserve these distinctions through
  errors, cancellation, and reopen.
- Verify the real entry point against the relevant independent result:
  transmitted request, filesystem effect, committed Session fact, or terminal
  behavior. For native interaction, exercise a real terminal as well as
  automated checks. Functional evidence is not sustained-use acceptance.

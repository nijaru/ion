---
name: ion-reference-implementation
description: Use when changing or reviewing a runtime, Session, tool, or terminal contract in Ion.
---

# Ion behavior changes

- Start from a real Ion workflow or correctness problem, not a parity gap.
  Keep familiar Pi interactions where they reduce friction. Choose behavior and
  implementation that fit Ion's Rust architecture, durable guarantees, and
  actual use; no reference implementation is a spec.
- Treat Pi Durable as a strong reference, especially for runtime and recovery
  semantics. Draw on Pi, fx, Codex, and others where their actual source and
  behavior inform the choice. Prefer fx's terminal presentation where it fits.
  Explain the trade-off when a change affects an established workflow.
- Trace the affected clients to the owner of the fact or effect. A queued
  input is not a durable acceptance, provisional output is not history, and a
  stop request is not joined completion. Preserve these distinctions through
  errors, cancellation, and reopen.
- Verify the real entry point against the relevant independent result:
  transmitted request, filesystem effect, committed Session fact, or terminal
  behavior. For native interaction, exercise a real terminal as well as
  automated checks. Functional evidence is not sustained-use acceptance.

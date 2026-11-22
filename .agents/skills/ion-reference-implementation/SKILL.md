---
name: ion-reference-implementation
description: Use when designing, implementing, refactoring, or reviewing Ion behavior; not for routine mechanical edits.
---

# Reference-led Ion implementation

Establish the reference contract before choosing an Ion fix. Reimplement useful
semantics idiomatically in Rust; do not decorate Ion's current abstractions with
upstream examples or translate upstream classes literally.

## Establish behavior and ownership

1. Name the real user workflow and affected boundary. Choose references by
   relevance to the decision, not by domain or a fixed order. Inspect the
   relevant source and callers in Pi, Pi Durable, Codex, fx, or other systems;
   use official docs for external contracts and measured evidence for performance.
   Check claims about current behavior against the implementation.
2. Record the inspected revision and relevant files, or cite the specific
   document or paper, in the working session. A fetched revision is not
   necessarily the checkout HEAD: inspect the actual Git blobs or verify HEAD.
   Historical research is navigation and provenance, not a parity checklist.
3. Trace the same workflow through Ion. Identify the authoritative owner of each
   mutable fact, accepted input, effect, result and publication. Follow legal
   transitions through refusal, cancellation, partial execution, settlement and
   reopen as relevant. Distinguish provisional observation from committed fact,
   construction from execution, and a stop request from joined completion.
4. State the intended observable behavior and any deliberate difference from the
   reference. Justify differences by Ion's requirements, Rust ownership or an
   explicit user decision—not by existing code or tests. Preserve Ion's durable
   guarantees when ordinary Pi is weaker; Pi and Pi Durable themselves use
   different implementation structures. Resolve consequential conflicts before
   implementing.

Keep the working contract task-local. Do not require a whole-repository survey
for a local change. Keep public changes understandable on their own.

## Replace the cause through its callers

- Choose the owner and representation that enforce the contract. Reuse correct
  machinery; replace weak abstractions and remove superseded state, APIs and
  callers. A series of guards, retention bins or fallback branches is a reason
  to reconsider their shared premise, not automatically add another exception.
- Before changing a shared contract, enumerate its real callers and what they
  rely on. Trace native, headless, RPC, embedding and managed-child paths where
  affected. A construction check must not accidentally serve as an unspoken
  execution grant; a local acknowledgment must not stand in for durable receipt.
- Complete one coherent vertical migration. Keep one engine, Session writer,
  effect owner and physical terminal publisher. Do not introduce a parallel
  runtime, renderer or event ledger to avoid replacing an existing weak owner.
- Treat a newly exposed caller failure as evidence that the migration or contract
  may be incomplete. Investigate before adding a caller-specific guard or
  changing a fixture. Keep a local guard when that caller genuinely owns a
  distinct policy, and make the distinction explicit.

## Verify the independently established contract

Derive acceptance oracles from the required workflow and inspected reference,
not the implementation's return values alone. Protect named unhandled risks;
confirm a regression before the fix when feasible. Exercise the actual caller
and independently inspect transmitted input, committed Session facts and
filesystem/tool outcomes as relevant. Model narration and green fixtures are
not evidence of semantic parity or human usability.

For interruption and recovery, check the real commit/effect boundaries. Unknown
outcomes are not retry permission. For a concrete concurrency gap, use a small
independent transition model or bounded interleavings when example tests leave
that race unprotected; do not add a modeling framework as ceremony.

Review the completed ownership/call graph, not only the diff. Follow repository
verification and delivery rules. Preserve useful evidence, distinguish fixed,
justified-different and unverified behavior, and close confirmed in-scope issues
before selecting unrelated capability work. New instructions improve discovery
and decision boundaries; they do not establish future adherence by themselves.

## Source navigation

These are entry points, not a frozen behavioral specification. Recheck the files
needed for the task at the selected current revision.

- [Pi/Pi Durable](https://github.com/earendil-works/pi):
  `packages/agent/src/{agent,agent-loop}.ts`,
  `packages/coding-agent/src/core/{agent-session,session-manager}.ts`,
  `packages/coding-agent/src/modes/interactive/`,
  `packages/durable/src/harness/{harness,generation,tool}.ts`,
  `packages/durable/src/session/{session,writer}.ts`.
- [fx](https://github.com/vercel-labs/fx): `src/core/app/`,
  `src/core/input/`, `src/core/output/`, `src/core/terminal/` and
  `src/core/tooling/`; follow their actual runtime/rendering dependencies.
- [Codex](https://github.com/openai/codex): `codex-rs/core/src/tasks/`
  and `codex-rs/tui/src/chatwidget.rs`; follow the relevant effect, task or
  interaction owner rather than treating Codex as a second product target.

Re-resolve moved files and verify the inspected revision before citing behavior.

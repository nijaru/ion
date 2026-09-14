# Design slices

Concrete designs for boundaries that do not exist yet. `DESIGN.md` owns the
accepted architecture, invariants and vocabulary; a document here owns one
boundary's actual interface, wire or storage mapping, failure modes and
acceptance checks.

## When a design slice is required

Two kinds of work happen in this repository, and they need different processes.

**Repair or re-perform an existing boundary** (correctness fixes, storage and
performance work, schema changes inside an accepted contract): implement,
measure, and record evidence. R1–R6 were this kind. A design document would have
described the code that already existed and slowed the work without adding a
decision.

**Create a new boundary** (a provider adapter, the execution environment, the
terminal surface, an external protocol): write the design first, review it, then
align implementation to it. Interface choices in these boundaries are the
expensive ones: a wrong request representation, error taxonomy or ownership
split is discovered only after dependent code exists, and the rewrite rule is to
replace obsolete paths rather than keep two.

The distinction is not size. It is whether the interface already exists. If
implementation is what decides the interface, the interface was not designed.

## What a design slice contains

One file per boundary, named `docs/design/<boundary>.md`, 250–400 lines:

1. **Scope and non-goals** — the boundary, and what it deliberately refuses.
2. **Contract** — the real Rust types, signatures and enums, and which crate
   owns each.
3. **External mappings** — for each external system the boundary faces (wire
   API, file format, protocol), how meaning maps in both directions and what the
   external system cannot express.
4. **Configuration and ownership** — durable session truth versus host-owned
   state, resolved at one boundary.
5. **Freezing, replay and recovery** — what is frozen before an external action,
   in what representation, and what happens when replay cannot reproduce it.
6. **Cancellation and failure** — where cancellation can land, what is durable at
   each point, and the typed error surface with external errors mapped onto it.
7. **Acceptance checks** — the specific tests that prove the design, named by
   suite and case, and which of them need network or hardware.
8. **Idiomatic notes** — async, error and ownership shape, and the dependency
   policy for anything new.
9. **Open questions** — each with a recommended default and the evidence that
   would change it.
10. **Out of scope** — and what would have to change to bring it in.

A design slice is a document about decisions, not a tutorial and not a second
architecture document. If it restates `DESIGN.md`, it is too long.

## How it stays aligned with the code

- A design slice is reviewed before implementation (by a reviewer that did not
  write it) and the review's findings are applied to the document, not carried
  into code as unstated intent.
- The `ROADMAP.md` row for the work links the design slice, repeats its
  acceptance checks and records the evidence pointer when the row closes.
- Once implementation starts, the document is updated when reality forces a
  change, and the change is recorded in `docs/decisions.md` with its reason. A
  design document that silently disagrees with the code is worse than no
  document.
- Measurements and reproductions stay where they are today: the roadmap's
  evidence log, with commit hashes. A design slice states what must be true; the
  evidence log states what was observed.

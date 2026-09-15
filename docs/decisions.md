# Decisions

Consequential choices, with their status and what would reopen them. This is a
lookup table, not a narrative: the reasoning lives in `DESIGN.md`, the design
slice, the roadmap evidence log or the commit that made the change.

Status vocabulary: **accepted** (in force), **superseded** (replaced, with the
entry that replaced it), **proposed** (recommended, not yet implemented),
**reopened** (evidence suggests revisiting).

| # | Date | Decision | Status | Reopen when |
|---|---|---|---|---|
| D1 | 2026-09-12 | One session has one authoritative semantic writer and one crash-atomic store; no second runtime and no generic durable Effect object (`DESIGN.md` §5, §9; R0.3) | accepted | A measured ownership or atomicity case cannot be served by one session-local store |
| D2 | 2026-09-12 | Domain is `Session → Conversation → Entry/Input/Task`; history parentage, ownership, dependency and workspace binding are separate relationships (`DESIGN.md` §2–§4) | accepted | A required relationship cannot be expressed without a new durable entity |
| D3 | 2026-09-12 | Entries are append-only and immutable; context is derived through heads and edits; fork cutoffs must project completely (`DESIGN.md` §7) | accepted | Projection cost or fork correctness cannot be met by derived context |
| D4 | 2026-09-12 | One typed async `TaskKind` contract with durable complete checkpoints, invocation-generation fencing, a durable cancellation mark plus a local signal, and a fresh abort invocation (R0.1) | accepted | — |
| D5 | 2026-09-12 | Provider-neutral types live in a small independent `ion-ai`; `ion-core` may depend on it and never the reverse (R0.5) | accepted | The contract cannot stay provider-neutral without session types |
| D6 | 2026-09-13 | Recovery is fail-closed: a checkpoint is absent, readable or unreadable, and unreadable never rebuilds or repeats an external action (`9c19a331`, `8c91cb28`) | accepted | A provider or tool supplies positive evidence that makes a repeat provably safe |
| D7 | 2026-09-13 | Accepted input is placed in the transcript when its turn is created, independent of answer success; a retry is a new attempt against the same placed entry (`d6fad545`) | accepted | Steering or command turns need placement at a different boundary |
| D8 | 2026-09-13 | Repairs before features: R1–R5 correctness, then bounded work, then the single-agent baseline, before worker fan-out (`ROADMAP.md` §1) | accepted | — |
| D9 | 2026-09-13 | A command prepares its writes in place through a rollback journal instead of copying resident state; a rejected command restores state exactly, a failed commit rolls back and fences the session (`54367ada`) | accepted | Installation cost or a reliability fault shows mutation-before-durability is observable |
| D10 | 2026-09-13 | Derived indexes (entries per conversation, tasks per turn, reverse dependencies, queued inputs) are written by one owner per record type and undone by the same journal (`22a7399b`) | accepted | An index cannot be kept consistent with its records |
| D11 | 2026-09-13 | Tool retry safety is an explicit per-tool opt-in, and evidence is consulted before the tool catalogue (`9c19a331`) | accepted | — |
| D12 | 2026-09-13 | A tool-recovery decision is policy-level, not implementation-level: recorded evidence names the call and the policy, not the implementation that served it (`f93be6c7`) | reopened | Implementation identity or a compatibility decision is recorded, with a reopen-after-replacement regression |
| D13 | 2026-09-13 | Instruction/control/wire fidelity is learned from one bounded live provider path before the remaining storage residency work, because it can change the durable request representation (`f93be6c7`). Tightened 2026-09-14 after the coding-loop survey: the frozen request must carry the *resolved instruction and project-context projection* and its revision, not only model, settings, cutoff and tool specifications — every surveyed harness treats the resolved prompt as an input, not a constant | accepted | The provider contract proves unable to change the frozen request |
| D14 | 2026-09-13 | New boundaries get a design slice before implementation; repairs and performance work stay implement-and-measure (`docs/design/README.md`) | accepted | A design slice demonstrably delays a boundary without changing a decision |

| D15 | 2026-09-14 | The first two wire adapters target OpenAI-compatible chat completions and Anthropic messages, chosen because they differ materially in instruction handling, content blocks, streaming and tool-call shape | accepted | Both adapters converge on the same neutral shape for every field, which would mean the neutrality claim is untested |
| D16 | 2026-09-14 | The `ion-core`/`ion-ai` Rust surface is reviewed as a whole before the provider traits freeze | accepted | — |
| D17 | 2026-09-14 | The excluded legacy `crates/ion` application stays in the tree until R7 and the terminal pass have mined its provider/auth and TUI *behaviour*; it is mining material, not a maintained reference and not a compatibility target | accepted | Those boundaries carry what they need, at which point the crate is deleted rather than kept beside the new one |

| D18 | 2026-09-14 | Artifact publication has no owner yet, so the unused `Artifact` record type and the always-`None` `TaskOutput.artifact` reference were deleted instead of kept as an API that implies spilling support. `DESIGN.md` §2/§17 keep the noun and the publish-before-reference requirement as targets; `ArtifactId` stays in the identity namespace per §6 | accepted | A boundary owns artifact publication, lookup and integrity checks, with crash-before-reference and missing/corrupt-content tests |

| D19 | 2026-09-14 | A turn's run budget is one durable record on the turn root task (`TurnBudget`: frozen limits, absolute deadline, reserved, settled, unknown attempts), spent through writer reservation/reconciliation commands rather than by per-generation copies. Per-generation checkpoints keep their reservation as evidence only; unknown usage never reconciles to zero and blocks a configured monetary cap | accepted | A budgeted kind cannot address the turn root, or contention shows the single record cannot serve concurrent members |
| D20 | 2026-09-14 | At the provider boundary the adapter reports facts (kind, status, code, dispatch knowledge, advisory delay, usage) and never retry eligibility, which generation policy owns alone; every provider stream passes one shared validator before core can observe it; and missing host state (provider/profile/model/tool revision) blocks a drive recoverably as `DriveOutcome::Blocked` instead of settling, while unreadable evidence stays terminal `Indeterminate` | accepted | A provider supplies positive evidence that makes an adapter-side retry decision safe, or a custom provider legitimately needs to bypass shared validation |
| D21 | 2026-09-14 | A conversation's generation configuration is one durable typed record, installed as a complete replacement fenced by the commit that installed it (`conversation/config.rs`, schema v6). Instructions and project context are stored resolved, so recovery reuses text rather than rereading files; the revision is the compare-and-set basis a client presents next and the binding a frozen request will record. Reconfiguration refuses a stale revision, a retired conversation and an invalid record before any write, and opening a session re-runs the validator, so a stored record this build refuses makes the session untrustworthy rather than being repaired (`docs/design/provider-request-assembly.md` §4) | accepted | assembly needs a partial, derived or provider-specific configuration, or a host needs to read configuration without its revision |

## How to add an entry

Add a row when a choice would be expensive to reverse or would be surprising to
find in the code without explanation. Do not add a row for a routine edit, and
do not restate what `DESIGN.md` already owns: link to it. When a decision is
replaced, mark it **superseded** and name the replacement rather than editing
history.

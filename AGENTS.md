# Working on Ion

## Design and scope

- `ARCHITECTURE.md` owns accepted contracts and explicit design boundaries.
  `README.md` describes implemented usage and qualification limits. Source
  establishes implementation, not old plans, reviews or passing tests.
- Build one reusable Rust agent engine. Coding is its first application, not
  its ceiling. Bots, services, phone integrations and coordinators reuse shared
  control, admission, recovery and delegation rather than implementing new loops.
- Pi is the primary architecture/workflow baseline; fx informs alternatives
  and native presentation. Codex and Claude Code are secondary references.
  Inspect current relevant upstream source before designing an overlapping
  capability, including its interaction and edge cases. Preserve proven
  user-facing semantics by default; implement them idiomatically in Rust.
  Depart only for a concrete usability, correctness or ownership benefit,
  accounting for relearning costs—not novelty, language differences or existing
  implementation convenience. Preserve Ion's authority/durability contracts.
  Do not freeze parity against a release or old checkout. Revisions identify
  historical evidence only; distinguish delivered behavior from examples,
  experiments and claims.
- Inline native scrollback is primary. Fullscreen is another viewport over the
  same semantics, not a separate runtime or polish workstream.
- Ion is unreleased v0 without compatibility guarantees. Replace obsolete
  owners, APIs and development formats with their callers; keep no deprecated
  facades or parallel runtimes.

## Work selection and delivery

- Use the current private roadmap and capability gap map when choosing work.
  They account for missing workflows; broad category coverage is not parity.
  Finish one bounded unit through its real caller before starting another.
- Resolve consequential product choices explicitly. Delivery staging must not
  invent defaults, feature gates, permanent SDK-only limits or silent exclusions.
  An unresolved capability is neither a promised feature nor a rejected one.
- Update the owning contract, affected usage guide and private plan/rationale in
  the same slice. Keep one owner for each concern. Historical checks and failures
  belong in evidence records, not the forward plan or public architecture.
- Do not require a new whole-product audit or live-model campaign for each slice.
  Tests and ordinary use verify the design; they do not replace specifying it.

## Source owners

- Core owns committed Session/Turn facts, execution, context, recovery and durable
  delegation relationships. Start at `crates/ion-core/src/{agent,session,request}.rs`;
  `tool_set.rs` freezes capabilities and `code_gateway.rs` retains composed effects.
- Host owns application recipes, authority, shared control and live supervision.
  Start at `crates/ion-host/src/control/`, `application.rs` and `binding.rs`.
  Do not duplicate input custody or operation lifecycle in clients. Restricted
  applications must not inherit coding authority. `code_mode.rs` owns the VM,
  not Session authority; composed tool children are not managed subagents.
- Terminal interaction starts in `crates/ion-app/src/terminal_client.rs` and its
  submodules. Keep editor/picker presentation separate from shared input custody.
  Core's `transcript.rs` supplies semantics; App's `transcript_render.rs`,
  `tool_output.rs` and `presentation_style.rs` render them;
  `crates/ion-terminal/src/screen.rs` owns physical publication.
- Extract cohesive responsibilities instead of growing central orchestration
  files. Avoid one-call-site wrappers and speculative frameworks.

## Verification

Use the stable toolchain selected by `rust-toolchain.toml`:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
```

Exercise changed built entry points. `scripts/smoke.sh` covers offline headless
submit/reopen; `scripts/smoke_headless.py` covers streaming, pressure and
cancellation; `scripts/smoke_coding.py` has an independent workspace/test oracle.
`.github/workflows/ci.yml` owns the required feature-smoke set. Add focused faults
for changed storage, provider, framing or cancellation contracts; preserve
meaningful effect/recovery/concurrency protection while pruning obsolete tests.
Prefer real-client integration tests for lifecycle changes and unit tests for
pure policy. Isolate HOME/XDG/TMPDIR and credentials; verify owned cleanup.
Reuse valid evidence for unchanged inputs. Measure before performance claims.

Terminal changes require real-terminal evidence as well as cheap checks. Use
owned processes/windows, preserve user preferences and inspect emulator outcomes.
Pause unrelated GUI tests after an unexplained crash/timeout and investigate.
PTY/tmux, native rendering, live coding and human acceptance are distinct evidence.

Before live Ion model tests, read knowledge `preferred-models.md`; it governs
models invoked by Ion, not development agents. Use bounded isolated tests and
approved routes; paid tests require authorization and cost disclosure. Inspect
requests, committed facts and independent effects, not model recaps. Do not hide
failures with fixture-specific suppression or larger retry budgets. Provider
availability must not block independent implementation work.

Documentation-only changes need link, authority and consistency checks, not
runtime-validation claims.

## Repository hygiene

Multiple sessions may share this worktree. Inspect status, stage only owned
changes and never reset, stash, clean or rewrite another session's work. Review
`Cargo.toml`/`Cargo.lock` changes, licenses and dependency lifecycle behavior;
prefer locked existing dependencies. Keep credentials and personal routes out of
tracked files. Do not commit handoffs or session scratch files.

Keep this the only repository agent-instruction file. Private preferences,
research and planning belong in knowledge; public docs must stand alone.

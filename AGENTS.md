# Working on Ion

## Design and scope

- `ARCHITECTURE.md` owns accepted contracts; `README.md` describes implemented
  behavior and qualification limits. Check current source rather than treating
  old reviews or passing tests as design authority.
- Build one reusable Rust agent engine; TUI/CLI coding is its first application,
  not its scope ceiling. Bots, services, phone integrations and swarms must reuse
  shared control, not fork the loop. Follow architecture's configuration, durable
  admission, delegation and attachment contracts even when delivery is staged.
  External hosting/scheduling does not excuse missing engine ownership semantics.
- Pi and fx are primary implementation references; Codex and Claude Code are
  secondary. Implement established APIs/workflows from relevant upstream source,
  not repeated live development trials. Inspect that source before materially
  changing overlapping capabilities; adapt it to Rust and Ion's ownership,
  durability and authority contracts. Distinguish established behavior,
  experiments and documentation-only claims. Delivery staging must not invent
  product defaults, feature gates or permanent SDK-only boundaries. Keep the
  designed coding workflow distinct from its currently implemented subset.
- Inline native scrollback is primary. Fullscreen is another viewport over the
  same semantics, not a separate runtime or competing polish workstream.
- Ion is unreleased v0 without compatibility guarantees. Replace obsolete
  owners, APIs and development formats, including their callers and stale docs;
  do not keep parallel runtimes or deprecated facades.

## Completion and slice loop

- Start from the current roadmap closeout, not a broad feature inventory. Finish
  one bounded slice through its real caller, tests, docs and evidence before
  starting another. V1 is the daily-driver boundary; post-v1 parity candidates
  are not blockers unless a concrete failure changes priority.
- Reference-first: inspect relevant Pi/fx/Codex/DeepSeek source before a material
  design change; distinguish established behavior, experiments and documentation
  claims. Adapt semantics to Ion's owners; do not add a second runtime, generic
  framework, plugin system or feature inventory.
- A contract change updates `ARCHITECTURE.md`/`README.md` and the owning knowledge
  note in the same slice. Keep the roadmap forward-looking; historical checkpoints,
  CI runs and negative evidence belong in the qualification log, not the roadmap.
- Process rationale and upstream observations live in
  `~/github/nijaru/knowledge/projects/ion/research/development-process-2026-10-17.md`;
  this file remains the actionable repository rule set.

## Owners and navigation

- Core owns committed Session/Turn facts, the agent loop, context, recovery and
  durable delegation relationships. Host owns application configuration, trusted
  execution environments, shared control and live child supervision.
  Start at `crates/ion-core/src/{agent,session,request}.rs`; `tool_set.rs` freezes
  executable capabilities and `code_gateway.rs` retains composed effects.
- Shared session control belongs in `crates/ion-host/src/control/`.
  `application.rs` owns complete recipes and the coding preset; `binding.rs`
  owns preflight publication and idle replacement. Do not duplicate
  input/operation lifecycle rules in clients. Do not freeze coding defaults into
  general control, bypass it for restricted integrations, or add a separate durable
  engine. Code Mode tool children are not managed subagents. `code_mode.rs` owns
  the guest VM, not Session authority.
- Start terminal interaction work in `crates/ion-app/src/terminal_client.rs`
  and its submodules. Keep editor/picker/input presentation separate from shared
  input custody. Semantic activity comes from Core's `transcript.rs`; App's
  `transcript_render.rs`, `tool_output.rs` and `presentation_style.rs` render it;
  `crates/ion-terminal/src/screen.rs` owns physical publication.
- Prefer a cohesive submodule over growing `agent.rs`, `session.rs` or
  `terminal_client.rs` with unrelated responsibilities. Extract a concern when
  that makes its invariants and tests clearer; do not add one-call-site helpers
  or framework layers.
- Finish coherent workflow slices across affected owners and callers. Preserve
  distinct effect/recovery/concurrency protections while pruning obsolete paths
  and implementation-coupled tests. A review is not a recurring whole-product
  gate; reconsider boundaries when requirements or evidence change.

## Verification

Use the stable toolchain selected by `rust-toolchain.toml`:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
```

Exercise changed built user entry points. `scripts/smoke.sh` covers offline
headless submit/reopen; `scripts/smoke_headless.py` covers streaming, pressure and
cancellation; `scripts/smoke_coding.py` runs a fixed coding task with an
independent workspace/test oracle. Feature smokes include terminal, RPC, MCP,
fork, resources and images; `.github/workflows/ci.yml` owns the required set. Add
focused faults for changed storage, provider, framing or cancellation contracts.
Reuse valid evidence for unchanged inputs; measure relevant harness costs before
performance claims. Prefer extending an existing test/smoke owner over adding a
generic qualification framework; the M2 matrix is a named set of existing
scenario owners plus the fixed coding smoke.
Isolate test HOME/XDG/TMPDIR state and credentials where practical; verify owned
process/temp cleanup. Prefer integration tests at the real client entry point for
agent, turn, recovery and terminal changes; use focused unit tests for pure policy.
Do not duplicate CI suites or turn every slice into a live campaign.

Terminal changes require real-terminal qualification as well as cheap checks.
Use owned processes/windows, preserve user preferences, and inspect emulator
outcomes—not just Ion's exit code. Pause unrelated GUI testing after a crash or
unexplained timeout and investigate before targeted reproduction. PTY/tmux,
native-emulator runs, live coding and human polish review are different evidence.

Use live coding qualification when the changed contract needs model interaction
or broader workflow acceptance, not as a gate between every implementation slice.
Read knowledge `preferred-models.md` before live Ion model tests; it selects
models invoked by Ion, not the agents developing it. Use bounded isolated
Ion tests and explicitly approved models/routes; paid tests
require authorization and cost disclosure. Judge actual requests, committed
facts and independent host effects, including after cancellation/cuts/reopen.
Do not hide failures with canned summaries, fixture-specific suppression or
larger retry budgets. Provider availability must not stall independent work or
become a route-inventory project.

## Repository hygiene and dependencies

- Multiple sessions may share this worktree. Inspect status before editing; stage
  only the files you changed and never reset, stash, clean or rewrite another
  session's work. Stop and ask if a conflict is outside your change.
- Treat `Cargo.toml`/`Cargo.lock` changes as reviewed code. Prefer locked existing
  dependencies; review new crates, licenses, build/lifecycle behaviour and
  lockfile drift. Never put credentials or personal model routes in tracked files.
- Do not commit `handoff.md` or other temporary session files. Keep private
  preferences and research in knowledge; keep public docs reader-focused.

Keep this the only repository agent-instruction file. Private preferences,
research and decisions belong in knowledge, not public notes or scaffolding.
Documentation-only changes need link/authority/consistency checks, not runtime
validation claims.

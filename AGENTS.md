# Working on Ion

## Design and scope

- `ARCHITECTURE.md` owns accepted contracts; `README.md` describes implemented
  behavior and qualification limits. Check current source rather than treating
  old reviews or passing tests as design authority.
- Build one idiomatic Rust engine for complete TUI, CLI and embedded-library
  use. Design for durable/delegated work without making Ion own surrounding
  services or orchestration. Stage delivery through the accepted roadmap.
- Pi and fx are primary implementation references; Codex and Claude Code are
  secondary. Inspect relevant upstream source before materially designing or
  changing overlapping capabilities. Adapt semantics to Rust; distinguish
  established behavior, experiments and documentation-only claims.
- Inline native scrollback is primary. Fullscreen is another viewport over the
  same semantics, not a separate runtime or competing polish workstream.
- Ion is unreleased v0 without compatibility guarantees. Replace obsolete
  owners, APIs and development formats, including their callers and stale docs;
  do not keep parallel runtimes or deprecated facades.

## Owners and navigation

- Core owns committed Session facts, the coding loop, context and recovery.
  Start at `crates/ion-core/src/{agent,session,request}.rs`; `tool_set.rs` freezes
  executable capabilities and `code_gateway.rs` retains composed effects.
- Shared session control belongs in `crates/ion-host/src/control/`.
  `binding.rs` owns preflight assembly and idle replacement. Do not duplicate
  input/operation lifecycle
  rules in clients or implement another model/tool loop. `code_mode.rs` owns
  the guest VM, not Session authority.
- Start terminal interaction work in `crates/ion-app/src/terminal_client.rs`
  and its submodules. Keep editor/picker/input presentation separate from shared
  input custody. Semantic activity comes from Core's `transcript.rs`; App's
  `transcript_render.rs`, `tool_output.rs` and `presentation_style.rs` render it;
  `crates/ion-terminal/src/screen.rs` owns physical publication.
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
cancellation. Feature smokes include terminal, RPC, MCP, fork, resources and
images; `.github/workflows/ci.yml` owns the required set. Add focused faults for
changed storage, provider, framing or cancellation contracts. Reuse valid evidence
for unchanged inputs; measure relevant harness costs before performance claims.

Terminal changes require real-terminal qualification as well as cheap checks.
Use owned processes/windows, preserve user preferences, and inspect emulator
outcomes—not just Ion's exit code. Pause unrelated GUI testing after a crash or
unexplained timeout and investigate before targeted reproduction. PTY/tmux,
native-emulator runs, live coding and human polish review are different evidence.

Live coding qualification uses bounded isolated tasks and explicitly approved
models/routes; paid tests require authorization. Judge actual requests, committed
facts and independent host effects, including after cancellation/cuts/reopen.
Do not hide failures with canned summaries, fixture-specific suppression or
larger retry budgets. Provider availability must not stall independent work or
become a route-inventory project.

Keep this the only repository agent-instruction file. Private preferences,
research and decisions belong in knowledge, not public notes or scaffolding.
Documentation-only changes need link/authority/consistency checks, not runtime
validation claims.

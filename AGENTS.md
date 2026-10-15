# Working on Ion

## Product and contracts

- `ARCHITECTURE.md` owns the coding-agent contracts; `README.md` describes
  implemented behavior and qualification limits. Neither a plan checkbox nor
  a passing test establishes that the implementation meets those contracts.
- The first completion target is a dependable Pi-level daily driver through one
  cohesive, idiomatic Rust coding loop for terminal, headless, RPC and embedded
  clients. Design execution, context and ownership boundaries to accommodate
  durable work and subagents from the outset; stage capability delivery through
  the roadmap rather than replacing the core later. Support composition by
  callers without making Ion own the services or orchestration around it.
  Pi and fx are the primary implementation references for harness design and
  workflows; fx also guides polished, restrained presentation. Codex is a
  secondary reference, especially for Rust implementation, and Claude Code's
  documented contracts provide secondary workflow and SDK evidence. Inspect
  relevant upstream code before materially designing or changing a capability
  those open-source references provide; maintained reviews are navigation, not
  substitutes for source. Adapt semantics idiomatically to Rust rather than
  copying runtimes, appearance or feature inventories. Verify moving revisions
  when decisions depend on them, distinguishing established behavior, inspected
  implementation, experiments and vendor claims.
- Prioritize inline native scrollback, legible activity hierarchy, an editable
  composer and progressive disclosure. Keep fullscreen as a secondary view of
  the same semantics; preserve its correctness without letting separate polish
  displace the inline experience.
- Ion is unreleased v0 with no compatibility guarantees. Replace obsolete APIs,
  storage formats and implementations directly; do not retain parallel runtimes,
  migration layers or deprecated facades for development state.
- Tools act in the live working directory with host permissions, not an implicit
  sandbox or VM. Persist observed effects, distinguish unknown effects after
  interruption, and never silently rerun an incomplete call. OAuth is not a
  first-use gate.
- Broader worker features, personal memory, scheduling and generic workflow
  authoring are not implied implementation scope. That delivery boundary must
  not prevent designing the shared core for durable and multi-agent use.

## Delivery and navigation

- For product-completion work, establish Core/Host dependability, then sustained
  real-repository coding and shared-client parity, then inline usability and
  terminal integration, before daily-driver acceptance. Keep one milestone
  active; close or explicitly defer its risks before advancing. A narrow repair
  needed for that milestone does not make its subsystem the new priority.
- Finish coherent coding-workflow slices, including affected callers, cleanup and
  built-client verification. A model's final answer, a feature's presence or a
  passing helper test is not task completion. A requested comprehensive design
  audit is a bounded input to the milestone, not a recurring gate after every
  repair. Reassess affected owners when requirements or evidence change.
- Start inline interaction work in `crates/ion-app/src/terminal_client.rs` and
  its submodules; semantic activity comes from `crates/ion-core/src/transcript.rs`,
  rendering from App's `transcript_render.rs` / `tool_output.rs`, semantic styles
  from `presentation_style.rs`, and physical publication from
  `crates/ion-terminal/src/screen.rs`. Keep input custody distinct from rendering.
- Shared client changes belong in `crates/ion-host/src/binding.rs`, not another
  client loop. Core's `request.rs` binds executable capabilities;
  `code_gateway.rs` retains composed effects, while Host's `code_mode.rs` owns
  only the guest VM.
- Review these owners and the relevant qualification harness before live coding
  or GUI runs. Use narrow independent reviews for risky boundaries, not review
  chains or multiple writers restructuring the same owner. Preserve distinct
  effect, recovery and concurrency protections when pruning tests.
- Provider availability must not halt independent product work or become a route
  inventory project. Reuse valid evidence for unchanged inputs. Measure startup,
  typing/redraw, history and tool-output costs before performance claims; faster
  model output is not evidence of a faster harness.

## Semantic owners

- `ion-ai`: provider-neutral model/message contracts. `ion-core`: durable
  Session facts, Turn ordering/recovery, context and abstract model/tool contracts.
- `ion-host`: provider transports/credentials, native/MCP tools, project resources
  and shared client operations. `ion-app`: client input/protocol/presentation.
  `ion-terminal`: physical terminal ownership, input and rendering mechanics.
- Keep durable facts, live progress and typed projections distinct. Clients must
  not infer execution truth from rendered strings or become another agent loop.
  Freeze model-visible tool definitions and execution routes together at each
  request boundary; provider caching cannot become Session correctness state.
- When cleanup changes an owner, migrate every affected caller and remove the
  superseded path and its obsolete tests/docs. Do not retain incidental output
  or private-call assertions merely because they already pass.

## Verification

Use the latest stable Rust toolchain selected by `rust-toolchain.toml`:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
```

Exercise the built user entry point for changed behavior. `scripts/smoke.sh`
covers offline headless submit/reopen; `scripts/smoke_headless.py` covers
streaming, publication pressure and cancellation. Relevant feature smokes live
in `scripts/smoke_{terminal,rpc,mcp,fork,resources,images}.py`; CI owns the required
set in `.github/workflows/ci.yml`. Add focused faults for changed storage,
provider, framing or cancellation boundaries. Investigate failed checks before
changing expectations; distinguish product defects, model behavior and harness
failures using actual requests, committed facts and observed effects.

Terminal changes also need real-terminal qualification. Use owned test
processes/windows and inspect both application and emulator outcomes: Ion exit 0
alone does not establish terminal stability. A crash or unexplained timeout is
unresolved evidence, not a clean run or an assumed environment flake. Do not
modify user terminal preferences. Pause unrelated GUI qualification after a
crash; investigate before a targeted, owned reproduction. Provider mocks, PTY
checks, native-emulator runs, live coding and human polish review establish
different things.

Use bounded, isolated live tasks after the relevant source/harness review and
cheap checks. Prefer available local or verified free routes; paid qualification
needs explicit authorization. Qualification budgets are fixture limits, not
production loop caps. Judge workflow completion against actual requests,
committed facts and independently checked final host effects, including after
cancellation, compaction and reopen. Distinguish a fresh model-issued mutation
from automatic replay. Do not mask adherence failures with transcript rewrites,
canned summaries or fixture-specific duplicate suppression. Readiness requires
representative workflow evidence and disclosed gaps, not test counts, source
parity or a single successful recap.

Keep this the only repository agent-instruction file. Private research,
decisions and continuation state belong in the knowledge repository, not new
repository notes or instruction scaffolds. Documentation-only changes require
link, authority and consistency checks, not a claim of runtime validation.

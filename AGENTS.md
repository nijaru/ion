# Working on Ion

## Product and contracts

- `ARCHITECTURE.md` owns the coding-agent contracts; `README.md` describes
  implemented behavior and qualification limits. Neither a plan checkbox nor
  a passing test establishes that the implementation meets those contracts.
- Build Pi-level common-workflow functionality through one cohesive, idiomatic
  Rust coding loop for terminal, headless, RPC and embedded clients. Pi is the
  primary mature open-source reference for workflows and harness/provider
  semantics; fx is the scoped reference for polished, restrained presentation.
  Other agents and research are optional sources of specific insights, not equal
  authorities, requirements or templates. Weigh their evidence, maturity and
  stability/simplicity trade-offs; Pi's experiments do not inherit its established
  product's status. Verify moving revisions when a decision depends on them,
  distinguishing observed behavior, inspected source and research/vendor claims.
  Adopt only for a concrete Ion need; copy no architecture, appearance or feature
  inventory wholesale.
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
- Workers, personal memory, scheduling and generic workflow authoring remain
  outside the current coding-agent scope.

## Delivery and navigation

- Finish coherent coding-workflow slices, including affected callers, cleanup and
  built-client verification. A model's final answer, a feature's presence or a
  passing helper test is not task completion. Reassess design at meaningful
  workflow/ownership changes; do not restart a blanket audit after each repair.
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
covers offline headless submit/reopen. Relevant feature smokes live in
`scripts/smoke_{terminal,rpc,mcp,fork,resources,images}.py`; CI owns the required
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

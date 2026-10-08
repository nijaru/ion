# Working on Ion

## Product and contracts

- `ARCHITECTURE.md` owns the coding-agent contracts; `README.md` describes
  implemented behavior and qualification limits. Neither a plan checkbox nor
  a passing test establishes that the implementation meets those contracts.
- Build Pi-level common-workflow functionality through one cohesive, idiomatic
  Rust coding loop for terminal, headless, RPC and embedded clients. Pi is the
  primary open-source reference for workflows and harness/provider semantics;
  fx is the primary reference for polished, restrained terminal presentation.
  Codex, Amp, Droid and other strong agents inform appropriate choices, not a
  union of feature inventories. Verify relevant moving source revisions and
  distinguish source evidence from vendor claims. Copy neither architecture nor
  appearance wholesale.
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

## Priorities and approach

- Prioritize common coding-workflow parity, correctness, maintainability and
  performance over new protocol integrations or feature breadth. A newly
  mentioned reference feature is not automatically the next task. Choose work
  from concrete gaps in the affected workflow, not release checkboxes.
- Review the affected source, callers and qualification harness before expensive
  live coding or native GUI runs. Look first for violated invariants, ignored
  errors, stale or duplicated state, ownership/cleanup mistakes and unnecessary
  hot-path work. Use focused independent review where risk warrants it, not a
  mandatory whole-repository audit before every change.
- Refactor and prune the affected subsystem as part of fixing it. Prefer clear
  ownership and idiomatic Rust to translated TypeScript architecture, extra
  wrappers or speculative frameworks. Preserve independent effect, recovery and
  concurrency protections when replacing code and consolidating tests.
- Address obvious redundant work directly; measure representative before/after
  behavior before claiming performance gains or adding speed-only complexity.
  Include long-history/context, rendering, tool output and resource lifecycle
  costs where relevant. Faster model output is not evidence of a faster harness.

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

Use the checked-in Rust 1.98.0 toolchain:

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
production loop caps. Readiness claims require representative workflow evidence
and disclosed gaps, not test counts, source parity or a single successful recap.

Keep this the only repository agent-instruction file. Private research,
decisions and continuation state belong in the knowledge repository, not new
repository notes or instruction scaffolds. Documentation-only changes require
link, authority and consistency checks, not a claim of runtime validation.

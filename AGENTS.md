# Working on Ion

## Product and contracts

- `ARCHITECTURE.md` owns the coding-agent contracts; `README.md` describes
  implemented behavior and qualification limits. Neither a plan checkbox nor
  a passing test establishes that the implementation meets those contracts.
- Build one cohesive Rust coding loop for terminal, headless, RPC and embedded
  clients, with Pi-level common-workflow usability and fx-like terminal
  restraint. Use current Pi for workflow/provider semantics and fx for semantic
  activity presentation when those concerns are affected; neither is a template
  to copy wholesale. Verify moving source revisions before relying on them.
- Ion is unreleased v0 with no compatibility guarantees. Replace obsolete APIs,
  storage formats and implementations directly; do not retain parallel runtimes,
  migration layers or deprecated facades for development state.
- Tools act in the live working directory with host permissions, not an implicit
  sandbox or VM. Persist observed effects, distinguish unknown effects after
  interruption, and never silently rerun an incomplete call. OAuth is not a
  first-use gate.
- Workers, personal memory, scheduling and generic workflow authoring remain
  outside the current coding-agent scope.

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
- When cleanup changes an owner, trace and migrate every affected caller and
  remove the superseded path and its obsolete tests/docs. Fix the boundary, not
  another wrapper around it. Preserve distinct recovery, concurrency and effect
  protections; do not retain incidental output or private-call assertions merely
  because they already pass.

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
set in `.github/workflows/ci.yml`. Terminal changes also need real-terminal
qualification; provider mocks alone do not establish live coding usability.
Add focused faults for changed storage, provider, framing or cancellation
boundaries. Investigate failed checks before changing expectations.

Keep this the only repository agent-instruction file. Private research,
decisions and continuation state belong in the knowledge repository, not new
repository notes or instruction scaffolds. Documentation-only changes require
link, authority and consistency checks, not a claim of runtime validation.

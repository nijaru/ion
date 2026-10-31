# Ion development

Follow the shared `nijaru/dotfiles/instructions/AGENTS.md` rules. Source is the implementation authority. This is unreleased v0: replace obsolete APIs and modules with their callers; do not add compatibility shims or preserve weak abstractions solely because they exist.

## Design and reference policy

Ion is one reusable Rust agent engine with a complete coding application, not
a coding-only kernel or multiple loops. Pi is the primary agent/workflow
reference, fx the native/TUI reference, and Pi Durable a source for effect and
programmatic-tool recovery patterns. Inspect current upstream source for the
affected workflow, then implement idiomatically in Rust. Reuse working code;
remove obsolete abstractions through all real callers. The current TUI is
not accepted for daily use. No autonomous pull requests.

Maintainer-private design, execution plan and source evidence may be available
through the owner's knowledge repository. Consult them when accessible under
the shared working agreements, but public usage and implementation contracts
must remain understandable from this repository alone. Do not recreate public
design/roadmap copies or treat old research snapshots as current instructions.

## Source navigation

- `crates/ion-core/src/agent/turn.rs` and `crates/ion-core/src/agent/request_cycle.rs`: Turn/request sequencing; `agent.rs` owns the public Agent facade and event/error types; focused tests live in `agent/tests.rs`.
- `crates/ion-core/src/session/records.rs`: durable entry/view types; `session/projection.rs`: context/history projection; `session/store.rs`: SQLite writer, lock, and commit-before-publication. `session.rs` owns the Session API and logical transitions.
- `crates/ion-core/src/tool_set.rs`: frozen tool catalog and dispatch; `tool_set/{discovery,presentation,selection}.rs`: ranked search, semantic activity and model-visible loadouts. `crates/ion-core/src/code_gateway.rs` and `crates/ion-host/src/code_mode.rs`: bounded composed calls.
- `crates/ion-host/src/control/`, `crates/ion-host/src/application.rs`, `crates/ion-host/src/local_tools/`: client control, coding recipe and effect executors.
- `crates/ion-app/src/headless.rs`, `crates/ion-app/src/rpc.rs`, `crates/ion-app/src/terminal_client/`; `crates/ion-terminal/src/`: public callers and native interaction.

Keep one owner for each durable fact, effect, execution outcome and terminal publication. Verify source in context and follow a task end-to-end. Do not use UI green tests to claim human usability.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
```

Use `scripts/smoke.sh` for an offline built headless/reopen path; relevant `scripts/smoke_*.py` for streaming, coding effects, recovery and cancellation. Exercise the real entry point plus independent tool/filesystem/Session outcomes. Keep focused protection for crash, concurrency, recovery and effect invariants. For native UI, run real terminal workflows; PTY/tmux checks alone are insufficient.

Before **live Ion model tests** read private `projects/ion/preferred-models.md`: it governs approved routes, cost/privacy and isolation. Synthetic model fixtures suffice for routine mechanical checks. Never silently spend on unapproved routes.

Multiple sessions may share a worktree. Inspect status, stage only owned changes, preserve unrelated files and secrets. The local untracked `handoff.md` is protected user content: do not read or modify it.

# Ion development

Source is the implementation authority. This is unreleased v0: replace obsolete APIs and modules with their callers rather than adding compatibility shims.

## Design and reference policy

Ion is one reusable Rust engine with a complete coding application. The model
chooses coding actions; do not hard-code an inspect/edit/test loop or add another
runtime. Prioritize material blockers in real workflows over upstream parity.
For behavior changes and reviews, use
[ion-reference-implementation](.agents/skills/ion-reference-implementation/SKILL.md).
Pi, Pi Durable, Codex, fx, and other relevant sources can inform the same decision.
Do not claim daily-use readiness without sustained-use evidence and maintainer
acceptance.

## Source navigation

- `crates/ion-core/src/agent/turn.rs` and `crates/ion-core/src/agent/request_cycle.rs`: Turn/request sequencing; `agent/cache_warming.rs` owns optional prefix-refresh policy; `agent.rs` owns the public facade and event/error types.
- `crates/ion-core/src/session/records.rs`: durable entry/view types; `session/projection.rs`: journal validation and archive rendering; `session/context_index.rs`: selected ancestry and working-set reconstruction; `session/context.rs`: model context and compaction cuts; `session/store.rs`: SQLite writer, lock, and commit-before-publication. `session.rs` owns the Session API and logical transitions.
- `crates/ion-core/src/tool_set.rs`: frozen tool catalog and dispatch; `tool_set/{discovery,presentation,selection}.rs`: ranked search, semantic activity and model-visible loadouts. `crates/ion-core/src/code_gateway.rs` and `crates/ion-app/src/code_mode.rs`: bounded composed calls.
- `crates/ion-app/src/control/`, `crates/ion-app/src/application.rs`, `crates/ion-app/src/local_tools/`: client control, coding recipe and effect executors.
- `crates/ion-cli/src/headless.rs`, `crates/ion-cli/src/rpc.rs`, `crates/ion-cli/src/terminal_client/`; `crates/ion-terminal/src/`: public callers and native interaction.

Keep one owner for each durable fact, effect, execution outcome and terminal publication. Verify source in context and follow a task end-to-end. Do not use UI green tests to claim human usability.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
```

Use `scripts/smoke.sh` for an offline built headless/reopen path; relevant `scripts/smoke_*.py` for streaming, coding effects, recovery and cancellation. Exercise the real entry point plus independent tool/filesystem/Session outcomes. Keep focused protection for crash, concurrency, recovery and effect invariants. For native UI, run real terminal workflows; PTY/tmux checks alone are insufficient.

Use offline model fixtures by default. Before live Ion model tests, confirm maintainer approval for the route, spending, data, and test isolation.

Multiple sessions may share a worktree. Inspect status, stage only owned changes, and preserve unrelated files and secrets.

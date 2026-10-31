# Ion development

Follow the shared `nijaru/dotfiles/instructions/AGENTS.md` rules. Source is the implementation authority. This is unreleased v0: replace obsolete APIs and modules with their callers; do not add compatibility shims or preserve weak abstractions solely because they exist.

## Authoritative private project work

Read `nijaru/knowledge/projects/ion/design.md` for target behavior and `implementation-plan.md` for the only active milestone sequence. The `research/reference-guide.md` points to current Pi, fx and Pi Durable source; consult current upstream code for each affected workflow. The private knowledge repository owns design/rationale/forward work. Do not add a competing public ARCHITECTURE, design, roadmap, session report or handoff to this repository.

Agent semantics: Pi primary; native/inline UX: fx primary; Pi Durable for restart-safe Code Mode/effect semantics. Preserve real observed workflows, not upstream language-specific class structures. Simplify source owners aggressively when warranted; remove superseded implementations once real callers migrate. No autonomous pull requests.

## Source navigation

- `ion-core/src/agent/turn.rs` and `agent/request_cycle.rs`: Turn/request sequencing; `agent.rs` still contains substantial orchestration and tests.
- `ion-core/src/session.rs`, `session/history/`: durable Session facts, branch/context and recovery.
- `ion-core/src/tool_set.rs`, `code_gateway.rs`, `ion-host/src/code_mode.rs`: captured tool dispatch and bounded Code Mode.
- `ion-host/src/control/`, `application.rs`, `local_tools/`: client control, coding recipe and effect executors.
- `ion-app/src/headless.rs`, `rpc.rs`, `terminal_client/`; `ion-terminal/src/`: public callers and native interaction.

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

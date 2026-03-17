# ion

Go rewrite of a fast, lightweight inline coding agent.

## Project Structure

| Directory     | Purpose                                        |
| ------------- | ---------------------------------------------- |
| cmd/ion/      | Main entry point                               |
| internal/     | TUI, session, and backend packages             |
| ai/           | Active design memory and task context          |
| archive/rust/ | Archived Rust implementation and Rust-TUI docs |
| .tasks/       | Task tracking (`tk`)                           |

## Session Workflow

**Start:**

1. Read `ai/STATUS.md`
2. Run `tk ready`
3. Check `internal/` before making architecture assumptions

**End:**

- Update `ai/STATUS.md`
- Log or complete relevant `tk` tasks
- Commit changes

## Active Architecture

| Component               | Purpose                                                                      |
| ----------------------- | ---------------------------------------------------------------------------- |
| `internal/app`          | Bubble Tea v2 host UI: transcript, composer, footer, status                  |
| `AgentSession` boundary | Canonical host-facing session lifecycle and event model                      |
| `NativeIonSession`      | Native ion runtime behind the session interface                              |
| `ACPAgentSession`       | External agent backends such as Claude Code, Gemini CLI, and similar systems |
| `archive/rust/`         | Historical reference only; not active implementation guidance                |

## Historical Checkpoint

- Use the Git tag `stable-rnk` for the last known stable Rust/RNK-era mainline.
- Do not move or rewrite that tag.

## Commands

```bash
go test ./...
go run ./cmd/ion

tk ls
 tk ready
 tk show <id>
 tk log <id> "finding"
 tk done <id>
```

## Rules

- Treat Go as the active implementation language.
- Treat `archive/rust/` as read-only reference unless explicitly migrating something out of it.
- Do not let archived Rust docs drive new design decisions on `main`.
- Use `tk` for all multi-step work.
- When a user reports a bug, create or update a `tk` task immediately.

## Go Idioms

Use the `go-expert` skill for full guidance. Key modern idioms:

- `slices` / `maps` packages — not manual loops or `sort.Slice`
- `iter.Seq` / `iter.Seq2` — range-over-function iterators (Go 1.23+)
- `sync.WaitGroup.Go` — replaces `Add(1); go func() { defer Done() }()`
- `errors.AsType[T](err)` — type-safe error unwrapping (Go 1.26)
- `t.Context()` in tests — not `context.TODO()`

## Reference

**Active design docs:**

- `ai/design/go-host-architecture.md`
- `ai/design/session-interface.md`
- `ai/design/acp-integration.md`
- `ai/design/native-ion-agent.md`
- `ai/design/memory-and-context.md`
- `ai/design/subagents-swarms-rlm.md`
- `ai/design/rewrite-roadmap.md`

**Retained research themes:**

- memory and context
- session persistence
- tools and MCP/ACP integration
- subagents, swarms, and RLM patterns

**Historical Rust docs:**

- `archive/rust/docs/`

# ion

> **Warning:** `ion` is work in progress and not stable yet. The core agent loop, TUI, and CLI surface are still changing.

Fast, lightweight terminal coding agent written in Go.

## Current Direction

- Primary path: Ion TUI -> CantoBackend -> Canto -> provider API
- Target: simple, reliable Pi-like core behavior before larger Pi+ features
- Active implementation: Go packages under `cmd/ion/` and `internal/`
- Active UI stack: Bubble Tea v2 + Bubbles v2
- Deferred path: ACP and subscription bridges after the native core is stable
- Historical Rust checkpoint: Git tag `stable-rnk`; archive under `archive/rust/`

## Current Status

The current stabilization focus is:

- native submit -> stream -> tool execution -> cancel -> error -> persist/replay flow
- transcript viewport, multiline composer, footer/status region, and resume display
- deterministic tests first, PTY/TUI smoke tests second, live provider smoke last

Expect bugs and breaking changes until this loop is stable.

## Install Locally

From the repo root:

```sh
go install ./cmd/ion
```

Make sure your Go binary directory is on `PATH`, usually `$(go env GOPATH)/bin`
or `GOBIN` if you set it.

## Run From Source

```sh
go run ./cmd/ion
```

## Development

```sh
go test ./...
```

## Historical Checkpoint

If you need the last pre-rewrite stable terminal experience:

```sh
git checkout stable-rnk
```

The prior Rust implementation and TUI planning material are preserved under `archive/rust/` for reference.

## Agent Instructions

`ion` reads `AGENTS.md` for project-level instructions and durable workflow context.

## License

[MIT](LICENSE)

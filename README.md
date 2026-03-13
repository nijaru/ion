# ion

> **Warning:** `ion` is currently transitioning to a Go/Bubble Tea v2 rewrite. The architecture, CLI surface, and internal boundaries are in flux. If you need the last known stable Rust/RNK-era checkpoint, use the Git tag `stable-rnk`.

Fast, lightweight coding agent with an inline terminal host.

## Current Direction

- Active implementation: Go host in `go-host/`
- Active UI stack: Bubble Tea v2 + Bubbles v2
- Planned runtime boundary: ACP-shaped session interface
- Historical Rust implementation: archived under `archive/rust/`

## Current Status

The rewrite is in progress. The Go host currently includes:

- transcript viewport
- multiline composer
- footer/status region
- streamed backend event scaffold
- tool-entry rendering

## Run The Current Host

```sh
cd go-host
go run ./cmd/ion-go
```

## Development

```sh
cd go-host
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

[PolyForm Shield 1.0.0](LICENSE)

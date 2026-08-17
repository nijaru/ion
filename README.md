# Ion

Ion is a terminal coding agent. The intended experience is a Pi-like
minimal surface:

```text
$ ion

> fix the parser bug
```

This repository is a clean-sheet Rust rewrite. There is no installable
binary yet. The last Go implementation is tagged `last-go` and is not a
design, behavior, or migration reference.

## Status

Phase 0 of `tk-q99i` is still open: synthesize the Rust product, ownership,
failure semantics, protocol contracts, and acceptance gates from the
rewrite handoff and current external sources. Subagents, MCP, ACP, and
other runtime primitives are first-class design targets, implemented
idiomatically in Rust.

## Planning sources

- [AGENTS.md](AGENTS.md) — working rules for this repository
- `ai/research/ion-rust-rewrite-handoff.md` — starting product and architecture target
- `ai/research/ion-rust-current-source-research-packet.md` — external Rust/protocol research to recheck before use

## License

[MIT](LICENSE)

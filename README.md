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

Ready work is `tk-mggy` (Phase 0): synthesize the Rust target from the
rewrite handoff, TUI handoff, and current external sources. No Cargo
workspace yet. Later slices are queued under `tk-q99i`.

## Planning sources

- [AGENTS.md](AGENTS.md) — session-start contract
- `ai/brief.md` — current head, ready task, next action
- `ai/research/ion-rust-rewrite-handoff.md` — starting product target
- `ai/research/ion-rust-tui-handoff.md` — TUI-layer research
- `ai/research/ion-rust-current-source-research-packet.md` — versions to recheck

## License

[MIT](LICENSE)

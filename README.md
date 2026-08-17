# Ion

Ion is a terminal coding agent. The intended experience is a Pi-like
minimal surface:

```text
$ ion

> fix the parser bug
```

This repository is a clean-sheet Rust rewrite. Print mode can run one
scripted turn. The last Go implementation is tagged `last-go` and is not
a design, behavior, or migration reference.

```text
cargo run -p ion -- -p hello
```

## Status

`RuntimeController` can submit, stream, cancel, and shut down a scripted
print-mode turn. Tools, sessions, and the TUI are not built yet. Current
work lives in `ai/brief.md` and `tk ready`.

## Planning sources

- [AGENTS.md](AGENTS.md) — durable session rules
- `ai/brief.md` — current head, ready task, next action
- `ai/DESIGN.md` — working Rust target
- `ai/research/` — imported product, TUI, and protocol research

## License

[MIT](LICENSE)

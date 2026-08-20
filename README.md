# Ion

Ion is a terminal coding agent: a small model-facing agent loop inside a
durable, single-writer session runtime. The intended experience is a
Pi-like minimal surface:

```text
$ ion

> fix the parser bug
```

This repository is a clean-sheet Rust rewrite. The authoritative target
design is [DESIGN.md](DESIGN.md). The last Go implementation is tagged
`last-go` and is not a design, behavior, or migration reference.

```text
cargo run -p ion -- -p "hello world"
```

## Status

Print mode runs one scripted operation through the durable runtime
contract: process `Runtime` + single-writer `SessionRuntime`, a pure
`OperationMachine` transition core, inbox steering/follow-up, and
cancel-aware sequential tool effects. SQLite persistence is next
(DESIGN.md §32 Step 2); TUI, MCP/ACP, and child sessions follow.
Current work lives in `tk ready` and the central `brief.md` (see
AGENTS.md).

## Planning sources

- [DESIGN.md](DESIGN.md) — authoritative target design
- [AGENTS.md](AGENTS.md) — every-session rules, authority, invariants
- Central `agent-context` brief — current head, ready task, next action

## License

[MIT](LICENSE)

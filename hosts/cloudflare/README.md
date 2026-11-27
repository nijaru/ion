# Durable Object journal

`journal.js` stores Ion's encoded Session records in a SQLite-backed Durable Object. It does not run the Rust Turn engine or schedule wakeups. The Worker in `test/` is a local fixture, not a deployable Ion service.

Run the Workerd tests with Node 24 or later:

```sh
cd hosts/cloudflare
npm ci
npm test
```

To also run the Rust journal bridge in Workerd, use Clang and `llvm-ar` (or set `AR_wasm32_unknown_unknown` to the LLVM archiver), then run:

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.127
npm run test:bridge
```

The bridge shares the native Rust Session’s journal projection and typed Turn writes: keyed admission, continuation, request context, assistant and tool records, and closure. The writer returns a receipt only after acknowledgment. While a flush is pending, the bound writer’s reads show its last acknowledged state and further writes are refused. This is an internal interface, not a public request or tool API. The host must authenticate callers, reconstruct the recovery binding, and arrange a wake before accepting or continuing a Turn.

A host claims one writer generation at startup. Each append checks the source journal identity, writer generation, and expected tail before inserting a batch. Records are split into 1 MiB chunks in the same transaction. The writer acknowledges the new tail only after `storage.sync()` succeeds and it still owns that generation. An uncertain result fences the writer; reopen the committed journal rather than retrying the batch.

Passive reads capture a fixed tail without taking writer authority. They return the original encoded bytes without replaying the Session. The tests exercise actual object teardown and Rust continuation planning after eviction, including loss of a reply after a durable write. They do not yet run the Agent, alarms, or HTTP tools in Workerd.

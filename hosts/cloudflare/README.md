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

The bridge replays a confirmed cut into the same Rust journal projection used by native Sessions. A bound writer keeps that projection live across successive appends. While a flush is pending, readers see the last acknowledged state and further writes are refused. The bridge is internal to the host; it does not expose a request or tool API.

A host claims one writer generation at startup. Each append checks the source journal identity, writer generation, and expected tail before inserting a batch. Records are split into 1 MiB chunks in the same transaction. The writer acknowledges the new tail only after `storage.sync()` succeeds and it still owns that generation. An uncertain result fences the writer; reopen the committed journal rather than retrying the batch.

Passive reads capture a fixed tail without taking writer authority. They return the original encoded bytes without replaying the Session. The tests exercise actual object teardown, including loss of the reply after a durable write. They do not yet exercise Turn recovery after eviction.

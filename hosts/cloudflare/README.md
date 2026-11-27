# Durable Object journal

`journal.js` stores Ion's encoded Session records in a SQLite-backed Durable Object. It does not run the Rust Turn engine or schedule wakeups. The Worker in `test/` is a local fixture, not a deployable Ion service.

Run the Workerd tests with Node 24 or later:

```sh
cd hosts/cloudflare
npm ci
npm test
```

A host claims one writer generation at startup. Each append checks the source journal identity, writer generation, and expected tail before inserting a batch. Records are split into 1 MiB chunks in the same transaction. The writer acknowledges the new tail only after `storage.sync()` succeeds and it still owns that generation. An uncertain result fences the writer; reopen the committed journal rather than retrying the batch.

Passive reads capture a fixed tail without taking writer authority. They return the original encoded bytes without replaying the Session. The tests exercise actual object teardown, including loss of the reply after a durable write. They do not yet exercise Turn recovery after eviction.

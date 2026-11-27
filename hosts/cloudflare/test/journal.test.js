import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { env } from "cloudflare:workers";
import { expect, test } from "vitest";
import { claimJournal, readJournalCut } from "../journal.js";

const identity = {
  session: "00000000-0000-7000-8000-000000000001",
  providerSession: "00000000-0000-7000-8000-000000000002",
};
const encode = (value) => new TextEncoder().encode(value);
const header = encode(JSON.stringify({
  version: 19, id: identity.session, provider_session_id: identity.providerSession, cwd: null,
}));
const small = encode('{"kind":"turn_ended","data":{"turn":1,"reason":"completed"}}');
const MiB = 1024 * 1024;
const batch = (expectedTail, bodies = [small]) => ({ identity, expectedTail, bodies });
const fresh = () => env.JOURNALS.get(env.JOURNALS.newUniqueId());
const digest = async (bytes) => Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
const generation = (storage) => storage.sql.exec("SELECT CAST(generation AS TEXT) AS gen FROM ion_journal").one().gen;

// Override acknowledgment without replacing real Workerd SQLite or its flush.
const withSync = (storage, sync) => ({
  sql: storage.sql, transactionSync: (run) => storage.transactionSync(run), sync,
});

test("eviction preserves opaque records and a passive cut never acquires a writer", async () => {
  const stub = fresh();
  const large = new Uint8Array(2 * MiB + 41);
  large[MiB - 1] = 17;
  large[MiB] = 29;
  large[2 * MiB] = 42;
  const expected = await digest(large);
  const previous = await runInDurableObject(stub, async (instance, { storage }) => {
    expect(await readJournalCut(storage)).toBe(null);
    await expect(claimJournal(storage, identity)).rejects.toMatchObject({ code: "missing" });
    expect(await readJournalCut(storage)).toBe(null);
    const writer = await claimJournal(storage, identity, header);
    expect(await writer.append(batch(0n, [large, small]))).toBe(2n);
    const rows = storage.sql.exec("SELECT length(body) AS bytes FROM ion_chunks WHERE seq = 1 ORDER BY part").toArray();
    expect(rows.map(({ bytes }) => bytes)).toEqual([MiB, MiB, 41]);
    return instance.instance;
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (instance, { storage }) => {
    expect(instance.instance).not.toBe(previous);
    const cut = await readJournalCut(storage);
    expect(generation(storage)).toBe("1");
    expect(cut.identity).toEqual(identity);
    expect(cut.header).toEqual(header);
    expect(cut.tail).toBe(2n);
    expect(await digest(cut.readRecord(1n))).toEqual(expected);
    expect(cut.readRecord(2n)).toEqual(small);
    const writer = await claimJournal(storage, identity);
    expect(generation(storage)).toBe("2");
    await writer.append(batch(2n));
    expect(cut.readRecord(2n)).toEqual(small);
    expect(() => cut.readRecord(3n)).toThrow("outside this cut");
    expect((await readJournalCut(storage)).tail).toBe(3n);
  });
});

test("exact identity, writer generation, and 64-bit tail fence each transaction", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const old = await claimJournal(storage, identity, header);
    await expect(claimJournal(storage, { ...identity, providerSession: identity.session }))
      .rejects.toMatchObject({ code: "identity" });
    expect(generation(storage)).toBe("1");
    const high = (1n << 53n) + 1n;
    // Skip the first 2**53 records in this coordinate-ABI test only.
    storage.sql.exec("INSERT INTO ion_records VALUES (CAST(? AS INTEGER), ?)", `${high}`, small.length);
    storage.sql.exec("INSERT INTO ion_chunks VALUES (CAST(? AS INTEGER), 0, ?)", `${high}`, small.slice().buffer);
    storage.sql.exec("UPDATE ion_journal SET generation = CAST(? AS INTEGER)", `${high}`);
    const writer = await claimJournal(storage, identity);
    expect(generation(storage)).toBe(`${high + 1n}`);
    await expect(old.append(batch(high))).rejects.toMatchObject({ code: "generation" });
    await expect(writer.append(batch(Number(high)))).rejects.toThrow(RangeError);
    await expect(writer.append(batch((1n << 63n) - 1n))).rejects.toThrow(RangeError);
    expect(await writer.append(batch(high))).toBe(high + 1n);
    await expect(writer.append(batch(high))).rejects.toMatchObject({ code: "tail" });
    await expect(writer.append(batch(high + 1n))).rejects.toMatchObject({ code: "fenced" });
    const cut = await readJournalCut(storage);
    expect(cut.tail).toBe(high + 1n);
    expect(cut.readRecord(high)).toEqual(small);
    expect(cut.readRecord(high + 1n)).toEqual(small);
  });
});

test("identity replacement at the same tail and generation invalidates the cut and writer", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    await expect(writer.append({ ...batch(0n), identity: { ...identity, providerSession: identity.session } }))
      .rejects.toMatchObject({ code: "batch_identity" });
    await writer.append(batch(0n));
    const cut = await readJournalCut(storage);
    storage.sql.exec("UPDATE ion_journal SET provider_session = ?", identity.session);
    expect(() => cut.readRecord(1n)).toThrow("journal identity changed");
    await expect(writer.append(batch(1n))).rejects.toMatchObject({ code: "identity" });
    await expect(writer.append(batch(1n))).rejects.toMatchObject({ code: "fenced" });
    expect((await readJournalCut(storage)).tail).toBe(1n);
  });
});

test.each(["acknowledged", "lost", "superseded"])("pending append acknowledgment: %s", async (outcome) => {
  const stub = fresh();
  await runInDurableObject(stub, async (_instance, { storage }) => {
    // Claim using real storage before making one append's flush reply wait.
    await claimJournal(storage, identity, header);
    let wait;
    const entered = Promise.withResolvers();
    const writer = await claimJournal(withSync(storage, async () => {
      await storage.sync();
      if (wait) {
        entered.resolve();
        await wait.promise;
      }
    }), identity);
    wait = Promise.withResolvers();
    let complete = false;
    const result = writer.append(batch(0n)).then(
      (tail) => { complete = true; return { tail }; },
      (error) => { complete = true; return { error }; },
    );
    await entered.promise;
    const cut = await readJournalCut(storage);
    expect(cut.tail).toBe(1n);
    expect(cut.readRecord(1n)).toEqual(small);
    expect(complete).toBe(false);
    expect(() => writer.readRecord(1n, 1n)).toThrow("journal writer is busy");
    await expect(writer.append(batch(1n))).rejects.toMatchObject({ code: "busy" });
    if (outcome === "superseded") await claimJournal(storage, identity);
    if (outcome === "lost") wait.reject(new Error("lost flush acknowledgment"));
    else wait.resolve();
    if (outcome === "acknowledged") {
      expect(await result).toEqual({ tail: 1n });
      expect(writer.readRecord(1n, 1n)).toEqual(small);
    } else {
      expect(await result).toMatchObject({ error: { code: "uncertain" } });
      await expect(writer.append(batch(1n))).rejects.toMatchObject({ code: "fenced" });
    }
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (_instance, { storage }) => {
    const cut = await readJournalCut(storage);
    expect(cut.tail).toBe(1n);
    expect(cut.readRecord(1n)).toEqual(small);
  });
});

test("aborting the object after flush loses the reply, not the committed records", async () => {
  const id = env.JOURNALS.newUniqueId();
  const stub = env.JOURNALS.get(id);
  const previous = await runInDurableObject(stub, (instance) => instance.instance);
  await expect(runInDurableObject(stub, async (_instance, state) => {
    const { storage } = state;
    let abort = false;
    const writer = await claimJournal(withSync(storage, async () => {
      await storage.sync();
      if (abort) state.abort("test: abort before append acknowledgment");
    }), identity, header);
    abort = true;
    return writer.append(batch(0n));
  })).rejects.toThrow(/abort before append acknowledgment/);

  await runInDurableObject(env.JOURNALS.get(id), async (instance, { storage }) => {
    expect(instance.instance).not.toBe(previous);
    const cut = await readJournalCut(storage);
    expect(cut.tail).toBe(1n);
    expect(cut.readRecord(1n)).toEqual(small);
    const writer = await claimJournal(storage, identity);
    await expect(writer.append(batch(0n))).rejects.toMatchObject({ code: "tail" });
  });
});

test("a SQL fault after earlier inserts rolls back the whole batch", async () => {
  const stub = fresh();
  await runInDurableObject(stub, async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    await writer.append(batch(0n));
    storage.sql.exec(`CREATE TRIGGER reject_later_chunk BEFORE INSERT ON ion_chunks
      WHEN NEW.seq = 3 AND NEW.part = 1 BEGIN SELECT RAISE(ABORT, 'injected failure'); END`);
    await expect(writer.append(batch(1n, [small, new Uint8Array(MiB + 1)])))
      .rejects.toMatchObject({ code: "rolled_back" });
    expect((await readJournalCut(storage)).tail).toBe(1n);
    expect(storage.sql.exec("SELECT count(*) AS count FROM ion_chunks").one().count).toBe(1);
    // A known rollback doesn't force a new owner. This is a different batch.
    expect(await writer.append(batch(1n))).toBe(2n);
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (_instance, { storage }) => {
    const cut = await readJournalCut(storage);
    expect(cut.tail).toBe(2n);
    expect(cut.readRecord(1n)).toEqual(small);
    expect(cut.readRecord(2n)).toEqual(small);
    expect(storage.sql.exec("SELECT count(*) AS count FROM ion_chunks").one().count).toBe(2);
  });
});

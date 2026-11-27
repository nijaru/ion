import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { env } from "cloudflare:workers";
import { expect, test } from "vitest";
import { claimJournal, readJournalCut } from "../journal.js";
import { initSync, WorkerJournalCut } from "./generated/ion_core.js";
import module from "./generated/ion_core_bg.wasm?module";

initSync({ module });

const identity = {
  session: "00000000-0000-7000-8000-000000000001",
  providerSession: "00000000-0000-7000-8000-000000000002",
};
const encode = (record) => new TextEncoder().encode(JSON.stringify(record));
const header = encode({
  version: 19, id: identity.session, provider_session_id: identity.providerSession, cwd: null,
});
const started = (turn, id = "once") => encode({ kind: "turn_started", data: {
  turn,
  input: { role: "User", content: [{ Text: "accepted input" }], provider_replay: null },
  model: { provider: "fixture", model: "offline" },
  request: { scope: "bridge", id },
} });
const start = started(1);
const end = encode({ kind: "turn_ended", data: { turn: 1, reason: "completed" } });
const fresh = () => env.JOURNALS.get(env.JOURNALS.newUniqueId());
const openCut = async (storage) => new WorkerJournalCut(await readJournalCut(storage));

const withSync = (storage, sync) => ({
  sql: storage.sql, transactionSync: (run) => storage.transactionSync(run), sync,
});

test("Rust validates the batch, waits for host acknowledgment, and replays the cut after eviction", async () => {
  const stub = fresh();
  const previous = await runInDurableObject(stub, async (instance, { storage }) => {
    const entered = Promise.withResolvers();
    const release = Promise.withResolvers();
    let hold = false;
    const writer = await claimJournal(withSync(storage, async () => {
      await storage.sync();
      if (hold) {
        entered.resolve();
        await release.promise;
      }
    }), identity, header);
    const cut = await openCut(storage);
    expect(cut.tail).toBe(0n);
    await expect(cut.append(writer, [start, start])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(0n);
    hold = true;
    let acknowledged = false;
    const append = cut.append(writer, [start]).then((tail) => {
      acknowledged = true;
      return tail;
    });
    await entered.promise;
    // We are holding a reply, not a transaction. An independent durable read
    // can see an acceptance, but its existing Rust cut must not move.
    const persisted = await openCut(storage);
    expect(persisted.tail).toBe(1n);
    expect(persisted.unfinishedTurn).toBe(1n);
    expect(cut.tail).toBe(0n);
    expect(cut.unfinishedTurn).toBeUndefined();
    expect(acknowledged).toBe(false);
    release.resolve();
    expect(await append).toBe(1n);
    return instance.instance;
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (instance, { storage }) => {
    expect(instance.instance).not.toBe(previous);
    const cut = await openCut(storage);
    expect(cut.tail).toBe(1n);
    expect(cut.unfinishedTurn).toBe(1n);
    const writer = await claimJournal(storage, identity);
    expect(await cut.append(writer, [end])).toBe(2n);
    const closed = await openCut(storage);
    expect(closed.tail).toBe(2n);
    expect(closed.unfinishedTurn).toBeUndefined();
    await expect(cut.append(writer, [end])).rejects.toMatchObject({ code: "tail" });
    expect((await readJournalCut(storage)).tail).toBe(2n);
  });
});

test("aborting during the Rust append reply leaves exactly the committed acceptance", async () => {
  const id = env.JOURNALS.newUniqueId();
  const stub = env.JOURNALS.get(id);
  const previous = await runInDurableObject(stub, (instance) => instance.instance);
  await expect(runInDurableObject(stub, async (_instance, state) => {
    const { storage } = state;
    let abort = false;
    const writer = await claimJournal(withSync(storage, async () => {
      await storage.sync();
      if (abort) state.abort("test: abort during Rust append reply");
    }), identity, header);
    const cut = await openCut(storage);
    abort = true;
    return cut.append(writer, [start]);
  })).rejects.toThrow(/abort during Rust append reply/);
  await runInDurableObject(env.JOURNALS.get(id), async (instance, { storage }) => {
    expect(instance.instance).not.toBe(previous);
    const cut = await openCut(storage);
    expect(cut.tail).toBe(1n);
    expect(cut.unfinishedTurn).toBe(1n);
    const writer = await claimJournal(storage, identity);
    await expect(cut.append(writer, [start])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(1n);
  });
});

test("the Rust bridge refuses an imprecise acknowledgment even for a durable write", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const cut = await openCut(storage);
    const imprecise = { append: async (batch) => Number(await writer.append(batch)) };
    await expect(cut.append(imprecise, [start])).rejects.toMatchObject({ code: "uncertain" });
    expect(cut.tail).toBe(0n);
    const persisted = await openCut(storage);
    expect(persisted.tail).toBe(1n);
    expect(persisted.unfinishedTurn).toBe(1n);
  });
});

test("duplicate delivery keys cannot be committed to the opaque hosted journal", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const empty = await openCut(storage);
    await expect(empty.append(writer, [start, end, started(2)])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(0n);
    expect(await empty.append(writer, [start, end])).toBe(2n);
    const closed = await openCut(storage);
    await expect(closed.append(writer, [started(2)])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(2n);
    expect(await closed.append(writer, [started(2, "another")])).toBe(3n);
    const persisted = await openCut(storage);
    expect(persisted.unfinishedTurn).toBe(2n);
  });
});

test("Rust refuses the wrong header identity or an invalid committed history", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    // Bypass Rust to simulate a persisted but semantically invalid record.
    await writer.append({ identity, expectedTail: 0n, bodies: [end] });
    const cut = await readJournalCut(storage);
    expect(cut.tail).toBe(1n);
    expect(() => new WorkerJournalCut(cut)).toThrow();
    storage.sql.exec("UPDATE ion_journal SET provider_session = ?", identity.session);
    const wrong = await readJournalCut(storage);
    expect(() => new WorkerJournalCut(wrong)).toThrow("journal identity changed");
  });
});

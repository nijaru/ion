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
const ended = (turn) => encode({ kind: "turn_ended", data: { turn, reason: "completed" } });
const select = (head_entry) => encode({ kind: "context_selected", data: { head_entry } });
const start = started(1);
const end = ended(1);
const model = { provider: "fixture", model: "offline" };
const requestKey = { scope: "bridge", id: "once" };
const key = encode(requestKey);
const request = (text = "accepted input") => encode({
  key: requestKey, input: { role: "User", content: [{ Text: text }], provider_replay: null },
});
const authority = {
  session: identity.session, authority: "fixture/principal/route-v1", fingerprint: Array(32).fill(7),
};
const binding = encode(authority);
const decode = (bytes) => JSON.parse(new TextDecoder().decode(bytes));
const submit = async (journal, bytes = request()) => decode(await journal.submit(bytes, encode(model), binding));
const receipt = { session: identity.session, key: requestKey, turn: 1, end: null };
const fresh = () => env.JOURNALS.get(env.JOURNALS.newUniqueId());
const openCut = async (storage) => new WorkerJournalCut(await readJournalCut(storage));
const bind = async (storage, writer) => (await openCut(storage)).bindWriter(writer);
const withSync = (storage, sync) => ({
  sql: storage.sql, transactionSync: (run) => storage.transactionSync(run), sync,
});

test("Rust publishes only after acknowledgment, and replays the cut after eviction", async () => {
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
    const pinned = await openCut(storage);
    const journal = await bind(storage, writer);
    expect(journal.tail).toBe(0n);
    await expect(journal.append([start, start])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(0n);
    hold = true;
    let acknowledged = false;
    const append = submit(journal).then((admission) => {
      acknowledged = true;
      return admission;
    });
    await entered.promise;
    // We are holding a reply, not a transaction or a Rust projection borrow.
    const persisted = await openCut(storage);
    expect(persisted.tail).toBe(1n);
    expect(persisted.unfinishedTurn).toBe(1n);
    expect(journal.tail).toBe(0n);
    expect(journal.unfinishedTurn).toBeUndefined();
    expect(journal.messageCount).toBe(0);
    expect(journal.requestReceipt(key)).toBeUndefined();
    expect(acknowledged).toBe(false);
    await expect(journal.append([end])).rejects.toMatchObject({ code: "busy" });
    release.resolve();
    expect(await append).toEqual({ status: "accepted", receipt });
    expect(decode(journal.requestReceipt(key))).toEqual(receipt);
    expect(journal.tail).toBe(1n);
    expect(journal.unfinishedTurn).toBe(1n);
    expect(journal.messageCount).toBe(1);
    expect(pinned.tail).toBe(0n);
    return instance.instance;
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (instance, { storage }) => {
    expect(instance.instance).not.toBe(previous);
    const writer = await claimJournal(storage, identity);
    const journal = await bind(storage, writer);
    expect(journal.tail).toBe(1n);
    expect(journal.unfinishedTurn).toBe(1n);
    expect(await journal.append([end])).toBe(2n);
    expect(journal.unfinishedTurn).toBeUndefined();
    expect((await openCut(storage)).tail).toBe(2n);
  });
});

test("one live writer can append and select newly committed context without reopening", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const journal = await bind(storage, writer);
    expect(await journal.append([start, end])).toBe(2n);
    expect(await journal.append([started(2, "another"), ended(2)])).toBe(4n);
    expect(journal.messageCount).toBe(2);
    expect(await journal.append([select(2)])).toBe(5n);
    expect(journal.messageCount).toBe(1);
    // A point read of the formerly unselected path must use this writer's
    // newly acknowledged tail, not the empty cut it originally opened.
    expect(await journal.append([select(4)])).toBe(6n);
    expect(journal.messageCount).toBe(2);
    expect((await openCut(storage)).tail).toBe(6n);
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
    const journal = await bind(storage, writer);
    abort = true;
    return submit(journal);
  })).rejects.toThrow(/abort during Rust append reply/);
  await runInDurableObject(env.JOURNALS.get(id), async (instance, { storage }) => {
    expect(instance.instance).not.toBe(previous);
    const writer = await claimJournal(storage, identity);
    const journal = await bind(storage, writer);
    expect(journal.tail).toBe(1n);
    expect(journal.unfinishedTurn).toBe(1n);
    expect(await submit(journal)).toEqual({ status: "existing", receipt });
    await expect(submit(journal, request("different input"))).rejects.toThrow("different input");
    expect((await readJournalCut(storage)).tail).toBe(1n);
  });
});

test("typed continuation checks scope and checkpoint, and settles a missing tool observation", async () => {
  const stub = fresh();
  await runInDurableObject(stub, async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const journal = await bind(storage, writer);
    expect(await submit(journal)).toEqual({ status: "accepted", receipt });
    await journal.append([encode({ kind: "assistant", data: {
      turn: 1,
      message: { role: "Assistant", content: [{ ToolCall: { id: "tool-1", name: "post", arguments: {} } }] },
      tool_activities: [{ call_id: "tool-1", activity: { kind: "external", subject: "post" } }],
      execution: { route: { logical: model, effective: model, reason: "user_request" } },
    } })]);
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity);
    const journal = await bind(storage, writer);
    const { point } = decode(journal.recoveryForRequest(key));
    expect(point).toEqual({ session: identity.session, turn: 1, checkpoint: 2 });
    const wrong = encode({ ...authority, fingerprint: Array(32).fill(1) });
    await expect(journal.resume(encode(point), wrong)).rejects.toThrow("authority");
    expect(journal.tail).toBe(2n);
    expect(decode(await journal.resume(encode(point), binding))).toEqual({
      point: { ...point, checkpoint: 4 }, interrupted: 1,
    });
    await expect(journal.resume(encode(point), binding)).rejects.toThrow("checkpoint is stale");
    expect(journal.tail).toBe(4n);
    const cut = await readJournalCut(storage);
    expect(decode(cut.readRecord(3n))).toEqual({ kind: "turn_resumed", data: { turn: 1, checkpoint: 2 } });
    expect(decode(cut.readRecord(4n))).toEqual({
      kind: "tool_result", data: { turn: 1, result: { call_id: "tool-1", name: "post", outcome: { state: "unknown" } } },
    });
    await journal.append([end]);
    expect(journal.recoveryForRequest(key)).toBeUndefined();
    expect(await submit(journal)).toEqual({ status: "existing", receipt: { ...receipt, end: "completed" } });
  });
});

test("an imprecise acknowledgment fences the Rust writer without publishing", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const imprecise = { append: async (batch) => Number(await writer.append(batch)) };
    const journal = await bind(storage, imprecise);
    await expect(journal.append([start])).rejects.toMatchObject({ code: "uncertain" });
    expect(journal.tail).toBe(0n);
    expect(journal.unfinishedTurn).toBeUndefined();
    // The underlying host writer did commit, but the Rust owner cannot use it.
    await expect(journal.append([start])).rejects.toMatchObject({ code: "fenced" });
    const persisted = await openCut(storage);
    expect(persisted.tail).toBe(1n);
    expect(persisted.unfinishedTurn).toBe(1n);
  });
});

test("known rollback leaves the Rust writer reusable without publishing its candidate", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const journal = await bind(storage, writer);
    storage.sql.exec(`CREATE TRIGGER refuse_end BEFORE INSERT ON ion_records
      WHEN NEW.seq = 2 BEGIN SELECT RAISE(ABORT, 'injected rollback'); END`);
    await expect(journal.append([start, end])).rejects.toMatchObject({ code: "rolled_back" });
    expect(journal.tail).toBe(0n);
    expect(journal.messageCount).toBe(0);
    expect((await readJournalCut(storage)).tail).toBe(0n);
    storage.sql.exec("DROP TRIGGER refuse_end");
    expect(await journal.append([start, end])).toBe(2n);
    expect(journal.tail).toBe(2n);
    expect(journal.messageCount).toBe(1);
  });
});

test("published keys participate in later validation before any write", async () => {
  await runInDurableObject(fresh(), async (_instance, { storage }) => {
    const writer = await claimJournal(storage, identity, header);
    const journal = await bind(storage, writer);
    await expect(journal.append([start, end, started(2)])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(0n);
    expect(await journal.append([start, end])).toBe(2n);
    await expect(journal.append([started(2)])).rejects.toThrow();
    expect((await readJournalCut(storage)).tail).toBe(2n);
    expect(await journal.append([started(2, "another")])).toBe(3n);
    expect(journal.unfinishedTurn).toBe(2n);
    expect((await openCut(storage)).unfinishedTurn).toBe(2n);
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

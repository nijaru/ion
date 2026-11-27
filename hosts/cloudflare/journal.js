// Storage only. The Rust engine validates and interprets the encoded records.
// Call these inside the Durable Object that owns this journal, never via an
// unauthenticated RPC. The owner is responsible for serializing wake handling.

const MAX_I64 = (1n << 63n) - 1n;
// Stay below the production 2 MB SQLite row/BLOB limit, including row overhead.
const CHUNK_BYTES = 1024 * 1024;
// Same bound as ion-core's session::journal::MAX_ENTRY_BYTES.
const MAX_ENTRY_BYTES = 64 * CHUNK_BYTES;
const UUID = /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/;
const isUuid = (value) => typeof value === "string" && value.length === 36 && UUID.test(value);

class JournalError extends Error {
  constructor(code, message, cause) {
    super(message, { cause });
    this.code = code;
  }
}

function coordinate(n) {
  if (typeof n !== "bigint" || n < 0n || n > MAX_I64) {
    throw new RangeError("journal coordinate must be a nonnegative signed 64-bit bigint");
  }
  return n;
}

function checkBytes(bytes, limit) {
  if (!(bytes instanceof Uint8Array) || bytes.length === 0 || bytes.length > limit) {
    throw new RangeError(`expected 1–${limit} encoded bytes`);
  }
}

function sameIdentity(a, b) {
  return a.session === b.session && a.providerSession === b.providerSession;
}

function initialized(sql) {
  return sql.exec("SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'ion_journal'")
    .toArray().length !== 0;
}

// Use text at the SQLite binding boundary: the Workers SQL numeric ABI rounds
// integers beyond 2**53. SQLite still stores and indexes the integer values.
function head(sql) {
  const row = sql.exec(`SELECT session, provider_session AS providerSession,
    CAST(generation AS TEXT) AS generation,
    CAST((SELECT max(seq) FROM ion_records) AS TEXT) AS tail
    FROM ion_journal WHERE singleton = 1`).one();
  return {
    session: row.session,
    providerSession: row.providerSession,
    generation: BigInt(row.generation),
    tail: row.tail === null ? 0n : BigInt(row.tail),
  };
}

function initialize(sql, identity, header) {
  sql.exec(`CREATE TABLE ion_journal (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    session TEXT NOT NULL, provider_session TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0), header BLOB NOT NULL
  ) STRICT;
  CREATE TABLE ion_records (
    seq INTEGER PRIMARY KEY CHECK(seq > 0),
    bytes INTEGER NOT NULL CHECK(bytes BETWEEN 1 AND ${MAX_ENTRY_BYTES})
  ) STRICT;
  CREATE TABLE ion_chunks (
    seq INTEGER NOT NULL REFERENCES ion_records(seq),
    part INTEGER NOT NULL CHECK(part BETWEEN 0 AND ${MAX_ENTRY_BYTES / CHUNK_BYTES - 1}),
    body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND ${CHUNK_BYTES}),
    PRIMARY KEY (seq, part)
  ) STRICT, WITHOUT ROWID;`);
  sql.exec("INSERT INTO ion_journal VALUES (1, ?, ?, 1, ?)",
    identity.session, identity.providerSession, header.slice().buffer);
}

/**
 * Claim one generation at object startup, under blockConcurrencyWhile. Do not
 * rotate a writer in a running instance; fencing appends doesn't join effects.
 * An absent journal can only be created from a seed.
 */
export async function claimJournal(storage, { session, providerSession }, initialHeader) {
  if (!isUuid(session) || !isUuid(providerSession)) {
    throw new TypeError("expected canonical Session and provider Session UUIDs");
  }
  if (initialHeader !== undefined) checkBytes(initialHeader, CHUNK_BYTES);
  const identity = { session, providerSession };
  const generation = storage.transactionSync(() => {
    const sql = storage.sql;
    if (!initialized(sql)) {
      if (initialHeader === undefined) throw new JournalError("missing", "no journal to claim");
      initialize(sql, identity, initialHeader);
      return 1n;
    }
    const current = head(sql);
    if (!sameIdentity(current, identity)) {
      throw new JournalError("identity", "journal identity changed");
    }
    const next = coordinate(current.generation + 1n);
    sql.exec("UPDATE ion_journal SET generation = CAST(? AS INTEGER) WHERE singleton = 1", `${next}`);
    return next;
  });
  await storage.sync();
  const current = head(storage.sql);
  if (!sameIdentity(current, identity) || current.generation !== generation) {
    throw new JournalError("generation", "writer superseded while claiming the journal");
  }
  return new JournalWriter(storage, identity, generation);
}

class JournalWriter {
  #storage;
  #owner;
  #state = "idle";

  constructor(storage, identity, generation) {
    this.#storage = storage;
    this.#owner = { ...identity, generation };
  }

  #checkHead(current, expectedTail) {
    if (!sameIdentity(current, this.#owner)) {
      return new JournalError("identity", "journal identity changed");
    }
    if (current.generation !== this.#owner.generation) {
      return new JournalError("generation", "writer generation is stale");
    }
    if (current.tail !== expectedTail) {
      return new JournalError("tail", `journal tail changed: expected ${expectedTail}, found ${current.tail}`);
    }
    return null;
  }

  // Only the current idle writer can read against its projected tail. No
  // staged SQL becomes an input to preparing a dependent append.
  readRecord(seq, expectedTail) {
    if (this.#state !== "idle") {
      throw new JournalError(this.#state, `journal writer is ${this.#state}`);
    }
    const { sql } = this.#storage;
    const refused = this.#checkHead(head(sql), coordinate(expectedTail));
    if (refused !== null) {
      this.#state = "fenced";
      throw refused;
    }
    return readRecord(sql, seq, expectedTail);
  }

  // Resolution acknowledges exactly this batch. Rejection with `uncertain`
  // requires reopening committed history, never retrying on this writer.
  async append({ identity, expectedTail, bodies }) {
    if (this.#state !== "idle") {
      throw new JournalError(this.#state, `journal writer is ${this.#state}`);
    }
    if (!identity || !sameIdentity(identity, this.#owner)) {
      throw new JournalError("batch_identity", "append belongs to a different journal");
    }
    coordinate(expectedTail);
    if (!Array.isArray(bodies) || bodies.length === 0) {
      throw new TypeError("append requires a nonempty batch of encoded records");
    }
    const next = coordinate(expectedTail + BigInt(bodies.length));
    for (const body of bodies) checkBytes(body, MAX_ENTRY_BYTES);

    const { sql } = this.#storage;
    this.#state = "busy";
    let refused;
    let rolledBack;
    try {
      refused = this.#storage.transactionSync(() => {
        const refused = this.#checkHead(head(sql), expectedTail);
        if (refused !== null) return refused;
        try {
          let seq = expectedTail;
          for (const body of bodies) {
            const key = `${++seq}`;
            sql.exec("INSERT INTO ion_records VALUES (CAST(? AS INTEGER), ?)", key, body.length);
            for (let offset = 0, part = 0; offset < body.length; offset += CHUNK_BYTES, part++) {
              sql.exec("INSERT INTO ion_chunks VALUES (CAST(? AS INTEGER), ?, ?)", key, part,
                body.slice(offset, offset + CHUNK_BYTES).buffer);
            }
          }
        } catch (cause) {
          // transactionSync rolls back when the callback throws. Distinguish
          // that documented path from failures while committing or syncing.
          rolledBack = new JournalError("rolled_back", "journal append rolled back", cause);
          throw rolledBack;
        }
        return null;
      });
      if (refused === null) {
        // The SQL callback has returned; no transaction or mutex spans this wait.
        await this.#storage.sync();
        const changed = this.#checkHead(head(sql), next);
        if (changed !== null) throw changed;
      }
    } catch (error) {
      if (rolledBack !== undefined && error === rolledBack) {
        this.#state = "idle";
        throw error;
      }
      this.#state = "fenced";
      throw new JournalError("uncertain", "journal append was not acknowledged", error);
    }
    if (refused !== null) {
      this.#state = "fenced";
      throw refused;
    }
    this.#state = "idle";
    return next;
  }
}

/** Passive snapshot; does not create a journal, acquire a writer, or repair it. */
export async function readJournalCut(storage) {
  const cut = storage.transactionSync(() => {
    if (!initialized(storage.sql)) return null;
    const { session, providerSession, tail } = head(storage.sql);
    const { header } = storage.sql.exec("SELECT header FROM ion_journal WHERE singleton = 1").one();
    return new JournalCut(storage, { session, providerSession }, tail, header);
  });
  // Local SQL reads may see writes still awaiting durability confirmation.
  // Return a cut only when even those writes have been independently confirmed.
  if (cut !== null) await storage.sync();
  return cut;
}

class JournalCut {
  #storage;
  #identity;
  #tail;
  #header;

  constructor(storage, identity, tail, header) {
    this.#storage = storage;
    this.#identity = Object.freeze(identity);
    this.#tail = tail;
    this.#header = header;
  }

  get identity() { return this.#identity; }
  get tail() { return this.#tail; }
  get header() { return new Uint8Array(this.#header.slice(0)); }

  // Rows are immutable. A tail captured before later appends or writer claims
  // still delimits the same records, without a long-lived SQLite transaction.
  readRecord(seq) {
    const sql = this.#storage.sql;
    if (!sameIdentity(head(sql), this.#identity)) {
      throw new JournalError("identity", "journal identity changed");
    }
    return readRecord(sql, seq, this.#tail);
  }
}

function readRecord(sql, seq, tail) {
  coordinate(seq);
  if (seq === 0n || seq > tail) throw new RangeError("record is outside this cut");
  const key = `${seq}`;
  const { bytes } = sql.exec("SELECT bytes FROM ion_records WHERE seq = CAST(? AS INTEGER)", key).one();
  const body = new Uint8Array(bytes);
  let offset = 0;
  let expectPart = 0;
  for (const { part, body: blob } of sql.exec(
    "SELECT part, body FROM ion_chunks WHERE seq = CAST(? AS INTEGER) ORDER BY part", key)) {
    if (offset === bytes || part !== expectPart++ || blob.byteLength !== Math.min(CHUNK_BYTES, bytes - offset)) {
      throw new JournalError("corrupt", "invalid journal record chunks");
    }
    body.set(new Uint8Array(blob), offset);
    offset += blob.byteLength;
  }
  if (offset !== bytes) throw new JournalError("corrupt", "missing journal record chunks");
  return body;
}

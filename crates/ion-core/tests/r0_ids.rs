use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tempfile::tempdir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ConversationId(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct EntryId(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TaskId(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CommitSeq(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatchReceipt {
    conversation_id: ConversationId,
    entry_id: EntryId,
    task_id: TaskId,
    commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Representation {
    SeparateCounters,
    UnifiedSequence,
}

struct PrototypeStore {
    connection: Connection,
    representation: Representation,
}

impl PrototypeStore {
    fn open(path: &Path, representation: Representation) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;

            CREATE TABLE meta (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );
            CREATE TABLE conversations (
                id INTEGER PRIMARY KEY
            );
            CREATE TABLE entries (
                id INTEGER PRIMARY KEY,
                conversation_id INTEGER NOT NULL REFERENCES conversations(id),
                body TEXT NOT NULL
            );
            CREATE INDEX entries_by_conversation
                ON entries(conversation_id, id);
            CREATE TABLE tasks (
                id INTEGER PRIMARY KEY,
                conversation_id INTEGER NOT NULL REFERENCES conversations(id),
                source_entry_id INTEGER NOT NULL REFERENCES entries(id)
            );
            CREATE INDEX tasks_by_conversation
                ON tasks(conversation_id, id);
            CREATE TABLE commits (
                seq INTEGER PRIMARY KEY,
                created_entry_id INTEGER REFERENCES entries(id)
            );
            "#,
        )?;
        match representation {
            Representation::SeparateCounters => {
                connection.execute("INSERT INTO meta VALUES ('next_id', 0)", [])?;
                connection.execute("INSERT INTO meta VALUES ('commit_seq', 0)", [])?;
            }
            Representation::UnifiedSequence => {
                connection.execute("INSERT INTO meta VALUES ('sequence', 0)", [])?;
            }
        }
        Ok(Self {
            connection,
            representation,
        })
    }

    fn create_batch(&mut self, body: &str) -> rusqlite::Result<BatchReceipt> {
        let representation = self.representation;
        let tx = self.connection.transaction()?;
        let receipt = match representation {
            Representation::SeparateCounters => Self::create_separate(&tx, body)?,
            Representation::UnifiedSequence => Self::create_unified(&tx, body)?,
        };
        tx.commit()?;
        Ok(receipt)
    }

    fn append_entry(
        &mut self,
        conversation_id: ConversationId,
        body: &str,
    ) -> rusqlite::Result<(EntryId, CommitSeq)> {
        let representation = self.representation;
        let tx = self.connection.transaction()?;
        let (entry_id, commit_seq) = match representation {
            Representation::SeparateCounters => {
                let next_id = Self::meta(&tx, "next_id")?;
                let commit = Self::meta(&tx, "commit_seq")? + 1;
                let entry = EntryId(next_id + 1);
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'next_id'",
                    [entry.0],
                )?;
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'commit_seq'",
                    [commit],
                )?;
                (entry, CommitSeq(commit))
            }
            Representation::UnifiedSequence => {
                let sequence = Self::meta(&tx, "sequence")?;
                let entry = EntryId(sequence + 1);
                let commit = CommitSeq(sequence + 2);
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'sequence'",
                    [commit.0],
                )?;
                (entry, commit)
            }
        };
        tx.execute(
            "INSERT INTO entries (id, conversation_id, body) VALUES (?1, ?2, ?3)",
            params![entry_id.0, conversation_id.0, body],
        )?;
        tx.execute(
            "INSERT INTO commits (seq, created_entry_id) VALUES (?1, ?2)",
            params![commit_seq.0, entry_id.0],
        )?;
        tx.commit()?;
        Ok((entry_id, commit_seq))
    }

    fn reject_batch(&mut self) -> rusqlite::Result<()> {
        let representation = self.representation;
        let tx = self.connection.transaction()?;
        match representation {
            Representation::SeparateCounters => {
                let next_id = Self::meta(&tx, "next_id")? + 3;
                let commit = Self::meta(&tx, "commit_seq")? + 1;
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'next_id'",
                    [next_id],
                )?;
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'commit_seq'",
                    [commit],
                )?;
            }
            Representation::UnifiedSequence => {
                let sequence = Self::meta(&tx, "sequence")? + 4;
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'sequence'",
                    [sequence],
                )?;
            }
        }
        tx.rollback()
    }

    fn current_clock(&self) -> rusqlite::Result<(i64, Option<i64>)> {
        match self.representation {
            Representation::SeparateCounters => Ok((
                Self::meta_connection(&self.connection, "next_id")?,
                Some(Self::meta_connection(&self.connection, "commit_seq")?),
            )),
            Representation::UnifiedSequence => {
                Ok((Self::meta_connection(&self.connection, "sequence")?, None))
            }
        }
    }

    fn entry_prefix(
        &self,
        conversation_id: ConversationId,
        cutoff: EntryId,
    ) -> rusqlite::Result<Vec<EntryId>> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM entries
             WHERE conversation_id = ?1 AND id <= ?2
             ORDER BY id",
        )?;
        statement
            .query_map(params![conversation_id.0, cutoff.0], |row| {
                Ok(EntryId(row.get(0)?))
            })?
            .collect()
    }

    fn cross_references_exist(&self, receipt: BatchReceipt) -> rusqlite::Result<bool> {
        self.connection
            .query_row(
                "SELECT 1
                 FROM tasks t
                 JOIN entries e ON e.id = t.source_entry_id
                 JOIN conversations c ON c.id = t.conversation_id
                 WHERE t.id = ?1
                   AND e.id = ?2
                   AND c.id = ?3
                   AND e.conversation_id = c.id",
                params![
                    receipt.task_id.0,
                    receipt.entry_id.0,
                    receipt.conversation_id.0
                ],
                |_| Ok(true),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
    }

    fn compact_bytes(&mut self) -> rusqlite::Result<u64> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        let page_count: i64 = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok((page_count as u64) * (page_size as u64))
    }

    fn create_separate(tx: &Transaction<'_>, body: &str) -> rusqlite::Result<BatchReceipt> {
        let next_id = Self::meta(tx, "next_id")?;
        let commit = Self::meta(tx, "commit_seq")? + 1;
        let conversation_id = ConversationId(next_id + 1);
        let entry_id = EntryId(next_id + 2);
        let task_id = TaskId(next_id + 3);
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'next_id'",
            [task_id.0],
        )?;
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'commit_seq'",
            [commit],
        )?;
        Self::insert_batch(
            tx,
            conversation_id,
            entry_id,
            task_id,
            CommitSeq(commit),
            body,
        )?;
        Ok(BatchReceipt {
            conversation_id,
            entry_id,
            task_id,
            commit_seq: CommitSeq(commit),
        })
    }

    fn create_unified(tx: &Transaction<'_>, body: &str) -> rusqlite::Result<BatchReceipt> {
        let sequence = Self::meta(tx, "sequence")?;
        let conversation_id = ConversationId(sequence + 1);
        let entry_id = EntryId(sequence + 2);
        let task_id = TaskId(sequence + 3);
        let commit_seq = CommitSeq(sequence + 4);
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'sequence'",
            [commit_seq.0],
        )?;
        Self::insert_batch(tx, conversation_id, entry_id, task_id, commit_seq, body)?;
        Ok(BatchReceipt {
            conversation_id,
            entry_id,
            task_id,
            commit_seq,
        })
    }

    fn insert_batch(
        tx: &Transaction<'_>,
        conversation_id: ConversationId,
        entry_id: EntryId,
        task_id: TaskId,
        commit_seq: CommitSeq,
        body: &str,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO conversations (id) VALUES (?1)",
            [conversation_id.0],
        )?;
        tx.execute(
            "INSERT INTO entries (id, conversation_id, body) VALUES (?1, ?2, ?3)",
            params![entry_id.0, conversation_id.0, body],
        )?;
        tx.execute(
            "INSERT INTO tasks (id, conversation_id, source_entry_id)
             VALUES (?1, ?2, ?3)",
            params![task_id.0, conversation_id.0, entry_id.0],
        )?;
        tx.execute(
            "INSERT INTO commits (seq, created_entry_id) VALUES (?1, ?2)",
            params![commit_seq.0, entry_id.0],
        )?;
        Ok(())
    }

    fn meta(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<i64> {
        tx.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
    }

    fn meta_connection(connection: &Connection, key: &str) -> rusqlite::Result<i64> {
        connection.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
    }
}

fn exercise(path: &Path, representation: Representation) -> rusqlite::Result<u64> {
    let mut store = PrototypeStore::open(path, representation)?;
    let first = store.create_batch("first")?;
    assert!(store.cross_references_exist(first)?);

    let before_reject = store.current_clock()?;
    store.reject_batch()?;
    assert_eq!(store.current_clock()?, before_reject);

    let (second_entry, second_commit) = store.append_entry(first.conversation_id, "second")?;
    assert!(second_entry > first.entry_id);
    assert!(second_commit > first.commit_seq);
    assert_eq!(
        store.entry_prefix(first.conversation_id, first.entry_id)?,
        vec![first.entry_id]
    );
    assert_eq!(
        store.entry_prefix(first.conversation_id, second_entry)?,
        vec![first.entry_id, second_entry]
    );

    for index in 0..1_000 {
        store.append_entry(first.conversation_id, &format!("entry-{index}"))?;
    }
    store.compact_bytes()
}

#[test]
fn unified_session_sequence_meets_id_invariants_without_sqlite_footprint_penalty() {
    let directory = tempdir().expect("tempdir");
    let separate_path = directory.path().join("separate.sqlite");
    let unified_path = directory.path().join("unified.sqlite");

    let separate_bytes = exercise(&separate_path, Representation::SeparateCounters)
        .expect("exercise separate counters");
    let unified_bytes = exercise(&unified_path, Representation::UnifiedSequence)
        .expect("exercise unified sequence");

    // Both representations use compact INTEGER keys. The unified form should not
    // require materially more storage; its advantage is one authoritative clock
    // instead of coordinating an object-ID counter and a commit counter.
    assert!(unified_bytes <= separate_bytes + 4_096);
}

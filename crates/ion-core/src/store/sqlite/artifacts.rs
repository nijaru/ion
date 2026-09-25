//! Indexed, attempt-owned auxiliary references. No backend code runs on this thread.
use rusqlite::{OptionalExtension, params};

use super::{
    SqliteDatabase,
    semantic::{json_from, json_to},
};
use crate::store::StoreError;
use crate::{AttemptId, BlobRef, BlobStoreUsage};

pub(super) fn link(
    connection: &rusqlite::Connection,
    attempt: AttemptId,
    reference: &BlobRef,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO tool_artifacts (attempt_id,digest,reference) VALUES (?1,?2,?3)",
        params![
            attempt.get(),
            reference.digest.to_string(),
            json_to(reference)?
        ],
    )?;
    Ok(())
}

impl SqliteDatabase {
    pub(crate) fn artifact_reference(&self, attempt: AttemptId) -> Result<BlobRef, StoreError> {
        let (digest, raw): (String, String) = self
            .connection
            .query_row(
                "SELECT digest,reference FROM tool_artifacts WHERE attempt_id=?1",
                [attempt.get()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound {
                kind: "tool artifact",
                id: attempt.get(),
            })?;
        let reference: BlobRef = json_from(&raw, "tool artifact")?;
        if reference.digest.to_string() != digest {
            return Err(StoreError::Corrupt(
                "artifact reachability index mismatch".into(),
            ));
        }
        Ok(reference)
    }

    /// The command owns the exclusive GC gate through completion. No transaction
    /// or history hydration is needed: publication cannot commit while we sweep.
    pub(crate) fn collect_artifacts(&self) -> Result<BlobStoreUsage, StoreError> {
        if self.fenced {
            return Err(StoreError::Fenced {
                cause: "cannot collect after ambiguous persistence".into(),
            });
        }
        if !self.artifacts.namespace_exists()? {
            return Ok(BlobStoreUsage {
                content_bytes: 0,
                object_count: 0,
            });
        }
        let mut statement = self
            .connection
            .prepare("SELECT 1 FROM tool_artifacts WHERE digest=?1 LIMIT 1")?;
        self.artifacts.store(false)?.collect_garbage(|digest| {
            Ok::<_, StoreError>(
                statement
                    .query_row([digest], |_| Ok(()))
                    .optional()?
                    .is_some(),
            )
        })
    }
}

//! Durable exclusion for cooperating, trusted workspace tools.
//!
//! The host explicitly wraps a tool with this binding. All wrapped operations
//! are treated as mutations. This is not confinement: a trusted tool or an
//! external writer can bypass or delete `.ion/claims.sqlite`. That metadata must
//! be retained across hosts/restarts. No automatic expiry or drop releases a
//! claim: a process can disappear while its descendants remain alive.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::FutureExt;
use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{Stop, Tool, ToolOutcome};

const MAX_BINDING_BYTES: usize = 64 * 1024;
const APPLICATION_ID: i64 = 0x494f4e57;

struct BindingBytes {
    bytes: Vec<u8>,
    remaining: usize,
}

impl std::io::Write for BindingBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other(
                "execution binding exceeds byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A canonical workspace with persistent, exclusive mutation claims.
///
/// Only tools returned by [`Self::bind`] participate. Keep the coordinator
/// files intact; deleting them or copying a live workspace defeats exclusion.
/// Hosts must agree on the same canonical root: nested roots have separate
/// coordinators and are not detected as conflicts. The wrapper does not change
/// the tool's working directory; the host must bind the actual target environment.
/// This binding provides no isolation from arbitrary external editors and no
/// authority beyond the host's explicit choice to wrap an unconfined tool.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    database: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace coordinator failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("workspace binding refused: {0}")]
    Invalid(String),
}

impl Workspace {
    /// Open or initialize the coordinator. This performs blocking filesystem
    /// I/O; async hosts should call it during setup or on a blocking thread.
    pub fn open(root: &Path) -> Result<Self, WorkspaceError> {
        let root = root.canonicalize()?;
        if !root.is_dir() || root.to_str().is_none() {
            return Err(WorkspaceError::Invalid(
                "expected a UTF-8 directory path".into(),
            ));
        }
        let metadata = root.join(".ion");
        std::fs::create_dir_all(&metadata)?;
        let database = metadata.join("claims.sqlite");
        let mut connection = Connection::open(&database)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let application: i64 =
            transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let tables: i64 = transaction.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )?;
        if application == 0 && version == 0 && tables == 0 {
            transaction.execute_batch(
                "CREATE TABLE claim (
                    slot INTEGER PRIMARY KEY CHECK(slot = 1),
                    execution TEXT NOT NULL,
                    implementation TEXT NOT NULL,
                    call TEXT NOT NULL
                );",
            )?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", 1)?;
        } else if application != APPLICATION_ID || version != 1 {
            return Err(WorkspaceError::Invalid(
                "unrecognized coordinator format".into(),
            ));
        }
        transaction.commit()?;
        // Persist the new directory entries as well as SQLite's committed data.
        std::fs::File::open(&metadata)?.sync_all()?;
        std::fs::File::open(&root)?.sync_all()?;
        Ok(Self { root, database })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opt a trusted tool into exclusive, unconfined workspace mutation.
    ///
    /// The wrapper records exact arguments and implementation before execution.
    /// A known result releases the claim; uncertainty, panic, dropped futures
    /// and process loss leave it in place. There is intentionally no force-clear
    /// or time-based expiry API without evidence-based reconciliation.
    #[must_use]
    pub fn bind(&self, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        Arc::new(BoundTool {
            workspace: self.clone(),
            tool,
        })
    }

    fn connection(&self) -> Result<Connection, WorkspaceError> {
        // Never recreate a coordinator deleted while a binding was live.
        let connection = Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let application: i64 =
            connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if application != APPLICATION_ID || version != 1 {
            return Err(WorkspaceError::Invalid(
                "unrecognized coordinator format".into(),
            ));
        }
        Ok(connection)
    }

    fn claim(
        &self,
        execution: &str,
        implementation: &str,
        call: &str,
    ) -> Result<bool, WorkspaceError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let occupied: Option<String> = transaction
            .query_row("SELECT execution FROM claim WHERE slot = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        if occupied.is_some() {
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO claim (slot, execution, implementation, call) VALUES (1, ?1, ?2, ?3)",
            params![execution, implementation, call],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    fn release(&self, execution: &str) -> Result<(), WorkspaceError> {
        let connection = self.connection()?;
        if connection.execute(
            "DELETE FROM claim WHERE slot = 1 AND execution = ?1",
            [execution],
        )? != 1
        {
            return Err(WorkspaceError::Invalid(
                "execution claim changed or disappeared".into(),
            ));
        }
        Ok(())
    }
}

struct BoundTool {
    workspace: Workspace,
    tool: Arc<dyn Tool>,
}

impl Tool for BoundTool {
    fn spec(&self) -> ToolSpec {
        self.tool.spec()
    }

    fn identity(&self) -> String {
        // Bind recovery to the environment as well as the underlying tool.
        serde_json::json!(["ion-workspace-1", self.workspace.root, self.tool.identity()])
            .to_string()
    }

    fn execute<'a>(&'a self, call: &'a ToolCall, stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let implementation = self.identity();
            let mut encoded = BindingBytes {
                bytes: Vec::new(),
                remaining: MAX_BINDING_BYTES.saturating_sub(implementation.len()),
            };
            if implementation.len() > MAX_BINDING_BYTES
                || serde_json::to_writer(&mut encoded, call).is_err()
            {
                return ToolOutcome::KnownFailure(
                    "workspace execution binding exceeds its byte limit".into(),
                );
            }
            let encoded = String::from_utf8(encoded.bytes).expect("JSON encoding is UTF-8");
            if stop.is_requested() {
                return ToolOutcome::KnownFailure(
                    "workspace action stopped before claiming".into(),
                );
            }
            let execution = uuid::Uuid::now_v7().to_string();
            let workspace = self.workspace.clone();
            let claim_id = execution.clone();
            let claimed = tokio::task::spawn_blocking(move || {
                workspace.claim(&claim_id, &implementation, &encoded)
            })
            .await;
            match claimed {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => {
                    return ToolOutcome::KnownFailure(
                        "workspace has an active or unresolved mutation claim".into(),
                    );
                }
                _ => {
                    return ToolOutcome::KnownFailure(
                        "workspace claim could not be confirmed; action was not started".into(),
                    );
                }
            }
            let outcome = if stop.is_requested() {
                ToolOutcome::KnownFailure("workspace action stopped before execution".into())
            } else {
                std::panic::AssertUnwindSafe(async { self.tool.execute(call, stop).await })
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        ToolOutcome::Indeterminate(
                            "workspace action panicked; its claim is retained".into(),
                        )
                    })
            };
            if matches!(outcome, ToolOutcome::Indeterminate(_)) {
                return outcome;
            }
            let workspace = self.workspace.clone();
            match tokio::task::spawn_blocking(move || workspace.release(&execution)).await {
                Ok(Ok(())) => outcome,
                _ => ToolOutcome::Indeterminate(
                    "workspace action reported, but claim release could not be confirmed".into(),
                ),
            }
        })
    }
}

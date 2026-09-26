//! Bounded, single-directory workspace discovery.
//!
//! Directory descriptors are opened relative to the pinned workspace root without
//! following symlinks. As with native read, the host must protect the namespace
//! from hostile concurrent renames; these checks are not OS confinement.

use std::{
    collections::BTreeMap,
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ion_ai::ToolSpec;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    ApprovalState, EgressRealm, LiveToolAuthority, PreparedAction, SemanticCompatibilityId,
    ToolAttemptState, ToolAuthority, ToolBinding, ToolBindingId, ToolBoundary, ToolBoundaryError,
    ToolConcurrency, ToolExecution, ToolRecoveryPolicy, ToolResult, WorkspaceBinding,
    native_read::{
        MAX_PATH_BYTES, RootIdentity, identity, open_absolute_directory, valid_relative_path,
    },
    workspace_registry::{RegistryError, WorkspaceRegistry, WorkspaceRevision},
};

pub const MAX_NATIVE_LIST_ENTRIES: usize = 256;
const DEFAULT_LIST_ENTRIES: usize = 100;
const MAX_SCANNED_ENTRIES: usize = 65_536;
const MAX_CONCURRENT_LISTS: usize = 4;
const IMPLEMENTATION_ID: &str = "native-list-v1";
const AUTHORITY_ALLOW: u8 = 0;
const AUTHORITY_ASK: u8 = 1;
const AUTHORITY_DENY: u8 = 2;

/// A read-only directory lister bound to one registry-owned workspace.
pub struct NativeListBoundary {
    binding: ToolBinding,
    workspace: WorkspaceBinding,
    executor: SemanticCompatibilityId,
    root: File,
    root_identity: RootIdentity,
    registry_directory: PathBuf,
    live_authority: Arc<AtomicU8>,
    permits: Arc<Semaphore>,
}

impl std::fmt::Debug for NativeListBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeListBoundary")
            .field("binding", &self.binding.id)
            .field("workspace", &self.workspace.id)
            .finish_non_exhaustive()
    }
}

impl NativeListBoundary {
    /// Pin a current registry workspace root. Live authority starts denied.
    pub fn new(
        registry: &WorkspaceRegistry,
        workspace: WorkspaceBinding,
    ) -> Result<Self, NativeListError> {
        if workspace.id.is_empty()
            || workspace.object_identity.is_empty()
            || workspace.canonical_root.len() > MAX_PATH_BYTES
        {
            return Err(NativeListError::InvalidWorkspace);
        }
        registry.verify_current(&workspace)?;
        let executor = SemanticCompatibilityId::new(workspace.backend.clone())
            .map_err(|_| NativeListError::InvalidWorkspace)?;
        let binding = native_list_binding().map_err(NativeListError::InvalidBinding)?;
        let root = open_absolute_directory(&workspace.canonical_root)?;
        let root_identity = identity(&root)?;
        registry.verify_current(&workspace)?;
        Ok(Self {
            binding,
            workspace,
            executor,
            root,
            root_identity,
            registry_directory: registry.directory().to_path_buf(),
            live_authority: Arc::new(AtomicU8::new(AUTHORITY_DENY)),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_LISTS)),
        })
    }

    /// Set current host policy. The worker rechecks immediately before listing.
    pub fn set_live_authority(&self, authority: LiveToolAuthority) {
        let value = match authority {
            LiveToolAuthority::Allow => AUTHORITY_ALLOW,
            LiveToolAuthority::Ask => AUTHORITY_ASK,
            LiveToolAuthority::Deny => AUTHORITY_DENY,
        };
        self.live_authority.store(value, Ordering::SeqCst);
    }

    #[must_use]
    pub fn tool_binding(&self) -> &ToolBinding {
        &self.binding
    }

    #[must_use]
    pub fn workspace_binding(&self) -> &WorkspaceBinding {
        &self.workspace
    }

    fn prepared_arguments(&self, action: &PreparedAction) -> Option<ListArguments> {
        if action.binding != self.binding.id
            || action.authority != ToolAuthority::ReadOnly
            || action.egress != EgressRealm::Local
            || action.workspace_revision.is_some()
            || !action.base_facts.is_empty()
        {
            return None;
        }
        let arguments: ListArguments = serde_json::from_value(action.arguments.clone()).ok()?;
        if !arguments.valid() || arguments.limit.is_none() {
            return None;
        }
        let canonical = serde_json::to_value(&arguments).ok()?;
        if canonical != action.arguments {
            return None;
        }
        let expected = PreparedAction::new(
            self.binding.id.clone(),
            canonical,
            EgressRealm::Local,
            ToolAuthority::ReadOnly,
            None,
            Vec::new(),
        )
        .ok()?;
        (expected == *action).then_some(arguments)
    }

    fn workspace_is_current(&self, workspace: &WorkspaceBinding) -> bool {
        workspace == &self.workspace
            && open_absolute_directory(&self.workspace.canonical_root)
                .and_then(|current| identity(&current))
                .is_ok_and(|current| current == self.root_identity)
    }
}

impl ToolBoundary for NativeListBoundary {
    fn binding(&self) -> ToolBinding {
        self.binding.clone()
    }

    fn executor(&self) -> SemanticCompatibilityId {
        self.executor.clone()
    }

    fn prepare(&self, arguments: Value) -> Result<PreparedAction, ToolBoundaryError> {
        let mut arguments: ListArguments =
            serde_json::from_value(arguments).map_err(|_| ToolBoundaryError::InvalidArguments)?;
        if !arguments.valid() {
            return Err(ToolBoundaryError::InvalidArguments);
        }
        arguments.limit.get_or_insert(DEFAULT_LIST_ENTRIES as u64);
        PreparedAction::new(
            self.binding.id.clone(),
            serde_json::to_value(arguments).map_err(|_| ToolBoundaryError::InvalidAction)?,
            EgressRealm::Local,
            ToolAuthority::ReadOnly,
            None,
            Vec::new(),
        )
        .map_err(|_| ToolBoundaryError::InvalidAction)
    }

    fn permits_retry(&self) -> bool {
        true
    }

    fn live_authority(
        &self,
        action: &PreparedAction,
        workspace: &WorkspaceBinding,
    ) -> LiveToolAuthority {
        if self.prepared_arguments(action).is_none() || !self.workspace_is_current(workspace) {
            return LiveToolAuthority::Deny;
        }
        decode_authority(self.live_authority.load(Ordering::SeqCst))
    }

    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        stop: CancellationToken,
    ) -> ion_ai::BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            let Some(arguments) = self.prepared_arguments(&execution.action) else {
                return not_started("invalid or incompatible prepared list action");
            };
            if execution.binding != self.binding
                || execution.workspace != self.workspace
                || !execution.action.permitted_by(&execution.ceiling)
            {
                return not_started("list binding, workspace, or authority changed");
            }
            if stop.is_cancelled() {
                return not_started("list cancelled before admission");
            }
            let output_limit = execution.output_limit.min(crate::MAX_TOOL_RECORD_BYTES);
            let permit = tokio::select! {
                biased;
                () = stop.cancelled() => return not_started("list cancelled before admission"),
                permit = Arc::clone(&self.permits).acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return not_started("list backend is shutting down"),
                },
            };
            if stop.is_cancelled() {
                return not_started("list cancelled before admission");
            }
            let root = match self.root.try_clone() {
                Ok(root) => root,
                Err(_) => return not_started("workspace root is no longer available"),
            };
            let drop_cancellation = CancelWorkerOnDrop::new(stop.clone());
            let job = ListJob {
                execution,
                arguments,
                root,
                root_path: self.workspace.canonical_root.clone(),
                root_identity: self.root_identity,
                registry_directory: self.registry_directory.clone(),
                live_authority: Arc::clone(&self.live_authority),
                executor: self.executor.clone(),
                binding: self.binding.clone(),
                stop,
                output_limit,
                _permit: permit,
            };
            let result = tokio::task::spawn_blocking(move || run_list(job)).await;
            drop_cancellation.disarm();
            match result {
                Ok(state) => state,
                Err(_) => settled_error("list worker failed", output_limit),
            }
        })
    }
}

/// Stable model-facing declaration for one directory at a time.
pub fn native_list_binding() -> Result<ToolBinding, crate::ConfigError> {
    ToolBinding::new(
        ToolBindingId::new("list")?,
        ToolSpec {
            name: "list".into(),
            description: "List one workspace directory. Names are sorted and paged with after; symlinks are shown but never traversed. Pages are not a filesystem snapshot."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES},
                    "after": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_NATIVE_LIST_ENTRIES}
                }
            }),
        },
        SemanticCompatibilityId::new(IMPLEMENTATION_ID)?,
        ToolConcurrency::Serial,
        ToolRecoveryPolicy::RepeatAfterNotStartedOrNoMutation,
        EgressRealm::Local,
    )
}

#[derive(Debug, Error)]
pub enum NativeListError {
    #[error("invalid native list workspace binding")]
    InvalidWorkspace,
    #[error("failed to construct the native list binding: {0}")]
    InvalidBinding(#[source] crate::ConfigError),
    #[error("workspace registry binding is not current: {0}")]
    Registry(#[from] RegistryError),
    #[error("failed to open workspace root: {0}")]
    Io(#[from] std::io::Error),
}

fn root_path() -> String {
    ".".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    #[serde(default = "root_path")]
    path: String,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

impl ListArguments {
    fn valid(&self) -> bool {
        (self.path == "." || valid_relative_path(&self.path))
            && self.path.len() <= MAX_PATH_BYTES
            && self.after.as_ref().is_none_or(|after| {
                valid_relative_path(after) && !after.contains('/') && after.len() <= MAX_PATH_BYTES
            })
            && self
                .limit
                .is_none_or(|limit| (1..=MAX_NATIVE_LIST_ENTRIES as u64).contains(&limit))
    }
}

#[derive(Serialize)]
struct ListEntry {
    name: String,
    kind: &'static str,
}

fn list_result(
    path: &str,
    entries: &[ListEntry],
    has_more: bool,
    skipped_non_utf8: u64,
    revision: WorkspaceRevision,
) -> ToolResult {
    ToolResult {
        value: json!({
            "path": path,
            "entries": entries,
            "has_more": has_more,
            "next_after": entries.last().map(|entry| &entry.name),
            "skipped_non_utf8": skipped_non_utf8,
            "workspace_revision": {"files": revision.files, "repository": revision.repository},
        }),
        is_error: false,
        capture: crate::OutputCapture::CompleteInline,
    }
}

fn not_started(reason: &str) -> ToolAttemptState {
    ToolAttemptState::NotStarted {
        reason: reason.into(),
    }
}

fn settled_error(message: &str, output_limit: usize) -> ToolAttemptState {
    let result = ToolResult {
        value: json!({"error": message}),
        is_error: true,
        capture: crate::OutputCapture::CompleteInline,
    };
    if serde_json::to_vec(&result).is_ok_and(|encoded| encoded.len() <= output_limit) {
        settled(result)
    } else {
        not_started("tool output limit cannot fit an error result")
    }
}

fn settled(result: ToolResult) -> ToolAttemptState {
    ToolAttemptState::Settled {
        result,
        effect: crate::EffectSummary::NoMutation,
        receipt: None,
        retryable: true,
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn decode_authority(value: u8) -> LiveToolAuthority {
    match value {
        AUTHORITY_ALLOW => LiveToolAuthority::Allow,
        AUTHORITY_ASK => LiveToolAuthority::Ask,
        _ => LiveToolAuthority::Deny,
    }
}

struct CancelWorkerOnDrop {
    stop: CancellationToken,
    armed: bool,
}

impl CancelWorkerOnDrop {
    fn new(stop: CancellationToken) -> Self {
        Self { stop, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for CancelWorkerOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.stop.cancel();
        }
    }
}

struct ListJob {
    execution: ToolExecution,
    arguments: ListArguments,
    root: File,
    root_path: String,
    root_identity: RootIdentity,
    registry_directory: PathBuf,
    live_authority: Arc<AtomicU8>,
    executor: SemanticCompatibilityId,
    binding: ToolBinding,
    stop: CancellationToken,
    output_limit: usize,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl ListJob {
    fn admission_allowed(&self, policy: u8) -> bool {
        if self.execution.binding != self.binding
            || self.execution.workspace.canonical_root != self.root_path
            || self.execution.action.authority != ToolAuthority::ReadOnly
            || !self.execution.action.permitted_by(&self.execution.ceiling)
        {
            return false;
        }
        let approved = self.execution.approval.permits(
            &self.execution.action,
            &self.binding.implementation,
            &self.executor,
            &self.execution.workspace,
            now_unix_ms(),
        );
        match policy {
            AUTHORITY_ALLOW => {
                !matches!(
                    self.execution.approval,
                    ApprovalState::Pending | ApprovalState::Denied { .. }
                ) && (!matches!(self.execution.approval, ApprovalState::Approved { .. })
                    || approved)
            }
            AUTHORITY_ASK => approved,
            _ => false,
        }
    }
}

fn run_list(job: ListJob) -> ToolAttemptState {
    if job.stop.is_cancelled() {
        return not_started("list cancelled before admission");
    }
    match open_absolute_directory(&job.root_path).and_then(|file| identity(&file)) {
        Ok(identity) if identity == job.root_identity => {}
        _ => return not_started("workspace root identity changed before list admission"),
    }
    // This policy load is the admission point for this read-only operation.
    if !job.admission_allowed(job.live_authority.load(Ordering::SeqCst)) {
        return not_started("live list authority denied before admission");
    }
    if job.stop.is_cancelled() {
        return not_started("list cancelled before admission");
    }
    let registry = match WorkspaceRegistry::open(&job.registry_directory) {
        Ok(registry) => registry,
        Err(_) => return settled_error("workspace revision is unavailable", job.output_limit),
    };
    let revision = match registry.revision(&job.execution.workspace) {
        Ok(revision) => revision,
        Err(_) => return settled_error("workspace revision is unavailable", job.output_limit),
    };
    let directory = match open_relative_directory(&job.root, &job.arguments.path) {
        Ok(directory) => directory,
        Err(_) => {
            return settled_error("path could not be opened as a directory", job.output_limit);
        }
    };
    let stream = match Dir::read_from(&directory) {
        Ok(stream) => stream,
        Err(_) => return settled_error("directory could not be listed", job.output_limit),
    };
    let wanted = job.arguments.limit.unwrap_or(DEFAULT_LIST_ENTRIES as u64) as usize;
    let mut page = BTreeMap::new();
    let mut skipped_non_utf8 = 0_u64;
    let mut scanned = 0_usize;
    for entry in stream {
        if job.stop.is_cancelled() {
            return settled_error("list cancelled after admission", job.output_limit);
        }
        scanned += 1;
        if scanned > MAX_SCANNED_ENTRIES {
            return settled_error("directory exceeds bounded scan limit", job.output_limit);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return settled_error("directory could not be listed", job.output_limit),
        };
        let name = match entry.file_name().to_str() {
            Ok(name) => name,
            Err(_) => {
                skipped_non_utf8 += 1;
                continue;
            }
        };
        if name == "." || name == ".." {
            continue;
        }
        if job
            .arguments
            .after
            .as_ref()
            .is_some_and(|after| name <= after.as_str())
        {
            continue;
        }
        let kind = match statat(&directory, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => "file",
                FileType::Directory => "directory",
                FileType::Symlink => "symlink",
                _ => "other",
            },
            Err(_) => return settled_error("directory changed while listing", job.output_limit),
        };
        page.insert(name.to_owned(), kind);
        if page.len() > wanted + 1 {
            page.pop_last();
        }
    }
    let candidates: Vec<_> = page.into_iter().collect();
    let mut entries = Vec::new();
    for (name, kind) in candidates.iter().take(wanted) {
        entries.push(ListEntry {
            name: name.clone(),
            kind,
        });
        let has_more = candidates.len() > entries.len();
        let result = list_result(
            &job.arguments.path,
            &entries,
            has_more,
            skipped_non_utf8,
            revision,
        );
        if serde_json::to_vec(&result).map_or(true, |encoded| encoded.len() > job.output_limit) {
            entries.pop();
            if entries.is_empty() {
                return settled_error(
                    "tool output limit cannot fit a directory entry",
                    job.output_limit,
                );
            }
            break;
        }
    }
    let has_more = candidates.len() > entries.len();
    if !matches!(registry.revision(&job.execution.workspace), Ok(current) if current == revision) {
        return settled_error("workspace changed during list", job.output_limit);
    }
    let result = list_result(
        &job.arguments.path,
        &entries,
        has_more,
        skipped_non_utf8,
        revision,
    );
    if serde_json::to_vec(&result).map_or(true, |encoded| encoded.len() > job.output_limit) {
        return settled_error(
            "list result exceeded its configured output bound",
            job.output_limit,
        );
    }
    settled(result)
}

fn open_relative_directory(root: &File, path: &str) -> std::io::Result<File> {
    let mut current = root.try_clone()?;
    if path == "." {
        return Ok(current);
    }
    for component in path.split('/') {
        current = File::from(openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Ok(current)
}

#[cfg(test)]
mod tests;

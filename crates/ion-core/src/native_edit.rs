//! Native bounded single-file replacement ToolBoundary.
//!
//! This is host-protected filesystem access, not an OS sandbox or race-free
//! compare-and-swap against uncooperative writers. The host must protect the
//! workspace namespace and registry from third-party concurrent renames.

use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::OwnedFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ion_ai::ToolSpec;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fchmod, fstat, open, openat, renameat, statat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    ApprovalState, BaseFact, ContentDigest, EgressRealm, LiveToolAuthority, PreparedAction,
    SemanticCompatibilityId, StartReceipt, ToolAttempt, ToolAttemptState, ToolAuthority,
    ToolBinding, ToolBindingId, ToolBoundary, ToolBoundaryError, ToolConcurrency, ToolExecution,
    ToolRecoveryPolicy, ToolResult, WorkspaceBinding,
    workspace_registry::{
        ClaimKey, EditAction, EditContent, EditPhysicalIdentity, EditRenameArmed, EditStaged,
        EditTermination, RegistryError, RegistryReceipt, TerminalEvidence, WorkspaceClaim,
        WorkspaceRegistry, WorkspaceResources, WorkspaceRevision,
    },
};

/// Hard maximum for the complete source and replacement target.
pub const MAX_NATIVE_EDIT_BYTES: usize = 16 * 1024;

const MAX_PATH_BYTES: usize = 4096;
const IMPLEMENTATION_ID: &str = "native-edit-private-v2";
const MAX_STAGE_FILES: usize = 64;
const CUSTODY_LEAF: &str = "native-edit-custody.lock";
const AUTHORITY_ALLOW: u8 = 0;
const AUTHORITY_ASK: u8 = 1;
const AUTHORITY_DENY: u8 = 2;

#[cfg(test)]
const FAULT_AFTER_RENAME: u8 = 1;
#[cfg(test)]
const FAULT_AFTER_REGISTRY_COMMIT: u8 = 2;
#[cfg(test)]
const FAULT_RECOVERY_DIRECTORY_SYNC: u8 = 3;

/// Native exact-text editor bound to one frozen tool and workspace.
pub struct NativeEditBoundary {
    binding: ToolBinding,
    workspace: WorkspaceBinding,
    executor: SemanticCompatibilityId,
    root: Option<File>,
    root_identity: Option<PhysicalIdentity>,
    staging: Option<File>,
    registry_directory: PathBuf,
    resources: WorkspaceResources,
    protected_paths: Vec<PathBuf>,
    max_file_bytes: usize,
    live_authority: Arc<AtomicU8>,
    permits: Arc<Semaphore>,
    #[cfg(test)]
    fault: Arc<AtomicU8>,
    #[cfg(test)]
    pause: Arc<TestPause>,
}

impl std::fmt::Debug for NativeEditBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEditBoundary")
            .field("binding", &self.binding.id)
            .field("workspace", &self.workspace.id)
            .field("resources", &self.resources)
            .field("max_file_bytes", &self.max_file_bytes)
            .finish_non_exhaustive()
    }
}

impl NativeEditBoundary {
    /// Authenticate a registry-owned binding and pin the root descriptor. The
    /// host must protect the namespace from external concurrent renames. Creation
    /// time is required so recovery can distinguish inode reuse; unsupported
    /// filesystems fail closed instead of exposing a weaker recovery proof.
    /// `staging_root` must be preexisting, canonical, private (0700), a strict
    /// descendant of the registry's protected namespace, and on the same
    /// supported local filesystem. The host protects it and the permanent registry custody inode
    /// from replacement, including by same-user tools. Retained staging is bounded;
    /// this backend never cleans up unknown artifacts or falls back to workspace staging.
    pub fn new(
        registry: &WorkspaceRegistry,
        workspace: WorkspaceBinding,
        max_file_bytes: usize,
        staging_root: &Path,
    ) -> Result<Self, NativeEditError> {
        Self::construct(registry, workspace, max_file_bytes, Some(staging_root))
    }

    /// Adopt already durable host evidence when the frozen workspace is gone.
    /// This boundary can never admit new execution or infer an unresolved rename.
    pub fn new_recovery(
        registry: &WorkspaceRegistry,
        workspace: WorkspaceBinding,
        max_file_bytes: usize,
    ) -> Result<Self, NativeEditError> {
        Self::construct(registry, workspace, max_file_bytes, None)
    }

    fn construct(
        registry: &WorkspaceRegistry,
        workspace: WorkspaceBinding,
        max_file_bytes: usize,
        staging_root: Option<&Path>,
    ) -> Result<Self, NativeEditError> {
        let live = staging_root.is_some();
        if !(1..=MAX_NATIVE_EDIT_BYTES).contains(&max_file_bytes) {
            return Err(NativeEditError::InvalidFileLimit);
        }
        if workspace.id.is_empty()
            || workspace.object_identity.is_empty()
            || workspace.canonical_root.len() > MAX_PATH_BYTES
        {
            return Err(NativeEditError::InvalidWorkspace);
        }
        let executor = SemanticCompatibilityId::new(workspace.backend.clone())
            .map_err(|_| NativeEditError::InvalidWorkspace)?;
        let (root, root_identity) = if live {
            registry.verify_current(&workspace)?;
            let root = open_absolute_directory(&workspace.canonical_root)?;
            let identity = physical_identity(&root).map_err(|error| {
                if error.kind() == std::io::ErrorKind::Unsupported {
                    NativeEditError::UnsupportedIdentity
                } else {
                    NativeEditError::Io(error)
                }
            })?;
            (Some(root), Some(identity))
        } else {
            (None, None)
        };
        let resources = registry.mutation_resources(&workspace)?;
        let protected_paths = registry.protected_mutation_paths(&workspace)?;
        if protected_paths
            .iter()
            .any(|admin| Path::new(&workspace.canonical_root).starts_with(admin))
        {
            return Err(NativeEditError::InvalidWorkspace);
        }
        let staging = if let Some(path) = staging_root {
            let text = path.to_str().ok_or(NativeEditError::InvalidStaging)?;
            let stage = open_absolute_directory(text)?;
            let canonical = path.canonicalize()?;
            if canonical != path
                || canonical == registry.directory()
                || !canonical.starts_with(registry.directory())
                || canonical.starts_with(&workspace.canonical_root)
                || Path::new(&workspace.canonical_root).starts_with(&canonical)
                || protected_paths
                    .iter()
                    .any(|admin| canonical.starts_with(admin) || admin.starts_with(&canonical))
                || stage.metadata()?.permissions().mode() & 0o077 != 0
            {
                return Err(NativeEditError::InvalidStaging);
            }
            let root = root.as_ref().expect("live root");
            qualify_filesystem(root, &stage)?;
            physical_identity(&stage)?;
            stage.sync_all()?;
            root.sync_all()?;
            Some(stage)
        } else {
            None
        };
        if live {
            registry.verify_current(&workspace)?;
        }

        Ok(Self {
            binding: native_edit_binding().map_err(NativeEditError::InvalidBinding)?,
            workspace,
            executor,
            root,
            root_identity,
            staging,
            registry_directory: registry.directory().to_path_buf(),
            resources,
            protected_paths,
            max_file_bytes,
            live_authority: Arc::new(AtomicU8::new(AUTHORITY_DENY)),
            permits: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            fault: Arc::new(AtomicU8::new(0)),
            #[cfg(test)]
            pause: Arc::new(TestPause::default()),
        })
    }

    /// Set current host policy. Admission is rechecked after durable registry
    /// claim and staging, immediately before the atomic replacement.
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

    fn ordinary_path(&self, path: &str) -> bool {
        valid_relative_path(path)
            && !self.protected_paths.iter().any(|admin| {
                Path::new(&self.workspace.canonical_root)
                    .join(path)
                    .starts_with(admin)
            })
    }

    fn prepared_arguments(&self, action: &PreparedAction) -> Option<EditArguments> {
        if action.binding != self.binding.id
            || action.authority != ToolAuthority::WorkspaceMutation
            || action.egress != EgressRealm::Local
            || action.workspace_revision.is_none()
        {
            return None;
        }
        let arguments: EditArguments = serde_json::from_value(action.arguments.clone()).ok()?;
        if !self.ordinary_path(&arguments.path)
            || arguments.path.len() > MAX_PATH_BYTES
            || arguments.expected_content.len() > self.max_file_bytes
            || arguments.old_text.len() > self.max_file_bytes
            || arguments.new_text.len() > self.max_file_bytes
            || arguments.expected_digest != digest_text(&arguments.expected_content)
            || action.workspace_revision != Some(arguments.base_revision.files)
        {
            return None;
        }
        let replacement = replacement(&arguments, self.max_file_bytes).ok()?;
        let facts = vec![BaseFact {
            path: arguments.path.clone(),
            digest: ContentDigest::of_bytes(arguments.expected_content.as_bytes()),
        }];
        if action.base_facts != facts {
            return None;
        }
        let canonical = serde_json::to_value(&arguments).ok()?;
        let expected = PreparedAction::new(
            self.binding.id.clone(),
            canonical,
            EgressRealm::Local,
            ToolAuthority::WorkspaceMutation,
            Some(arguments.base_revision.files),
            facts,
        )
        .ok()?;
        (expected == *action && replacement.len() <= self.max_file_bytes).then_some(arguments)
    }

    fn sync_recovered_directory(&self, parent: &File, source: bool) -> std::io::Result<()> {
        let _ = source;
        #[cfg(test)]
        if self
            .fault
            .compare_exchange(
                if source {
                    33
                } else {
                    FAULT_RECOVERY_DIRECTORY_SYNC
                },
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return Err(std::io::Error::other(
                "injected recovery directory sync failure",
            ));
        }
        parent.sync_all()
    }

    fn workspace_is_current(&self, workspace: &WorkspaceBinding) -> bool {
        let Some(pinned) = self.root_identity else {
            return false;
        };
        if workspace != &self.workspace
            || !matches!(
                open_absolute_directory(&self.workspace.canonical_root)
                    .and_then(|root| physical_identity(&root)),
                Ok(identity) if identity == pinned
            )
        {
            return false;
        }
        WorkspaceRegistry::open(&self.registry_directory)
            .and_then(|registry| registry.verify_current(workspace))
            .is_ok()
    }

    #[cfg(test)]
    fn inject_fault(&self, fault: u8) {
        self.fault.store(fault, Ordering::SeqCst);
    }
}

impl ToolBoundary for NativeEditBoundary {
    fn binding(&self) -> ToolBinding {
        self.binding.clone()
    }

    fn executor(&self) -> SemanticCompatibilityId {
        self.executor.clone()
    }

    fn prepare(&self, arguments: Value) -> Result<PreparedAction, ToolBoundaryError> {
        let mut arguments: EditArguments =
            serde_json::from_value(arguments).map_err(|_| ToolBoundaryError::InvalidArguments)?;
        // The model supplies the exact base, not a redundant digest it cannot
        // reliably compute. Freeze the digest in the canonical prepared action.
        arguments.expected_digest = digest_text(&arguments.expected_content);
        if !self.ordinary_path(&arguments.path)
            || arguments.path.len() > MAX_PATH_BYTES
            || arguments.expected_content.len() > self.max_file_bytes
            || arguments.old_text.len() > self.max_file_bytes
            || arguments.new_text.len() > self.max_file_bytes
            || arguments.expected_digest != digest_text(&arguments.expected_content)
        {
            return Err(ToolBoundaryError::InvalidArguments);
        }
        replacement(&arguments, self.max_file_bytes)
            .map_err(|_| ToolBoundaryError::InvalidArguments)?;
        let facts = vec![BaseFact {
            path: arguments.path.clone(),
            digest: ContentDigest::of_bytes(arguments.expected_content.as_bytes()),
        }];
        let action = PreparedAction::new(
            self.binding.id.clone(),
            serde_json::to_value(arguments.clone())
                .map_err(|_| ToolBoundaryError::InvalidAction)?,
            EgressRealm::Local,
            ToolAuthority::WorkspaceMutation,
            Some(arguments.base_revision.files),
            facts,
        )
        .map_err(|_| ToolBoundaryError::InvalidAction)?;
        crate::tool_boundary::bounded(&action)?;
        Ok(action)
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
                return not_started("invalid or incompatible prepared edit action");
            };
            if execution.binding != self.binding
                || execution.workspace != self.workspace
                || execution.action.authority != ToolAuthority::WorkspaceMutation
                || !execution.action.permitted_by(&execution.ceiling)
            {
                return not_started("edit binding, workspace, or authority changed");
            }
            if stop.is_cancelled() {
                return not_started("edit cancelled before admission");
            }
            let permit = tokio::select! {
                biased;
                () = stop.cancelled() => return not_started("edit cancelled before admission"),
                permit = Arc::clone(&self.permits).acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return not_started("edit backend is shutting down"),
                },
            };
            if stop.is_cancelled() {
                return not_started("edit cancelled before admission");
            }
            let Some(pinned_root) = self.root.as_ref() else {
                return not_started("recovery-only edit boundary cannot execute");
            };
            let root = match pinned_root.try_clone() {
                Ok(root) => root,
                Err(_) => return not_started("workspace root is no longer available"),
            };
            let staging = match self
                .staging
                .as_ref()
                .and_then(|stage| stage.try_clone().ok())
            {
                Some(stage) => stage,
                None => return not_started("private staging is unavailable"),
            };
            let drop_cancellation = CancelWorkerOnDrop::new(stop.clone());
            let job = EditJob {
                execution,
                arguments,
                root,
                staging,
                root_identity: self.root_identity.expect("live root has physical identity"),
                registry_directory: self.registry_directory.clone(),
                resources: self.resources,
                live_authority: Arc::clone(&self.live_authority),
                binding: self.binding.clone(),
                executor: self.executor.clone(),
                stop,
                max_file_bytes: self.max_file_bytes,
                _permit: permit,
                #[cfg(test)]
                fault: Arc::clone(&self.fault),
                #[cfg(test)]
                pause: Arc::clone(&self.pause),
            };
            let result = tokio::task::spawn_blocking(move || run_edit(job)).await;
            drop_cancellation.disarm();
            match result {
                Ok(state) => state,
                Err(_) => indeterminate("edit worker ended without terminal evidence", None),
            }
        })
    }

    fn reconcile<'a>(
        &'a self,
        execution: ToolExecution,
        attempt: ToolAttempt,
    ) -> ion_ai::BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move { reconcile_edit(self, execution, attempt).await })
    }
}

/// Construct the frozen declaration for the native `edit` tool.
pub fn native_edit_binding() -> Result<ToolBinding, crate::ConfigError> {
    ToolBinding::new(
        ToolBindingId::new("edit")?,
        ToolSpec {
            name: "edit".into(),
            description: "Replace one exact occurrence in a regular file of at most 16 KiB under the bound workspace root. Provide the complete expected file and workspace_revision from read.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "expected_content", "workspace_revision", "old_text", "new_text"],
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES},
                    "expected_content": {"type": "string", "maxLength": MAX_NATIVE_EDIT_BYTES},
                    "workspace_revision": {
                        "type": "object", "additionalProperties": false,
                        "required": ["files", "repository"],
                        "properties": {
                            "files": {"type": "integer", "minimum": 0, "maximum": i64::MAX},
                            "repository": {"type": "integer", "minimum": 0, "maximum": i64::MAX}
                        }
                    },
                    "old_text": {"type": "string", "minLength": 1, "maxLength": MAX_NATIVE_EDIT_BYTES},
                    "new_text": {"type": "string", "maxLength": MAX_NATIVE_EDIT_BYTES}
                }
            }),
        },
        SemanticCompatibilityId::new(IMPLEMENTATION_ID)?,
        ToolConcurrency::Serial,
        ToolRecoveryPolicy::NeverRepeat,
        EgressRealm::Local,
    )
}

/// Constructor and frozen-workspace validation errors.
#[derive(Debug, Error)]
pub enum NativeEditError {
    #[error("invalid native edit workspace binding")]
    InvalidWorkspace,
    #[error("native edit file limit must be between 1 and 16384 bytes")]
    InvalidFileLimit,
    #[error(
        "staging must be a preexisting private directory outside workspace and Git administration"
    )]
    InvalidStaging,
    #[error("filesystem does not provide trustworthy creation-time identity")]
    UnsupportedIdentity,
    #[error("failed to construct the native edit binding: {0}")]
    InvalidBinding(#[source] crate::ConfigError),
    #[error("workspace registry binding is not current: {0}")]
    Registry(#[from] RegistryError),
    #[error("failed to open canonical workspace root: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EditArguments {
    path: String,
    expected_content: String,
    #[serde(default)]
    expected_digest: String,
    #[serde(rename = "workspace_revision")]
    base_revision: EditRevision,
    old_text: String,
    new_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EditRevision {
    files: u64,
    repository: u64,
}

impl From<WorkspaceRevision> for EditRevision {
    fn from(value: WorkspaceRevision) -> Self {
        Self {
            files: value.files,
            repository: value.repository,
        }
    }
}

impl From<EditRevision> for WorkspaceRevision {
    fn from(value: EditRevision) -> Self {
        Self {
            files: value.files,
            repository: value.repository,
        }
    }
}

type PhysicalIdentity = EditPhysicalIdentity;

struct EditJob {
    execution: ToolExecution,
    arguments: EditArguments,
    root: File,
    staging: File,
    root_identity: PhysicalIdentity,
    registry_directory: PathBuf,
    resources: WorkspaceResources,
    live_authority: Arc<AtomicU8>,
    binding: ToolBinding,
    executor: SemanticCompatibilityId,
    stop: CancellationToken,
    max_file_bytes: usize,
    _permit: tokio::sync::OwnedSemaphorePermit,
    #[cfg(test)]
    fault: Arc<AtomicU8>,
    #[cfg(test)]
    pause: Arc<TestPause>,
}

fn run_edit(job: EditJob) -> ToolAttemptState {
    // The permanent registry lock is opened independently for every operation.
    // The blocking owner retains it even if its async waiter is dropped.
    let _custody = match acquire_custody(&job.registry_directory) {
        Ok(lock) => lock,
        Err(_) => return not_started("edit custody is unavailable before admission"),
    };
    let mut registry = match WorkspaceRegistry::open(&job.registry_directory) {
        Ok(registry) => registry,
        Err(_) => return indeterminate("workspace registry is unavailable", None),
    };
    // A separate open description locks the permanent staging directory itself.
    // This also bounds shared staging across independently configured registries.
    let _stage_custody = match lock_staging(&job.staging) {
        Ok(lock) => lock,
        Err(_) => return not_started("private staging custody is unavailable before admission"),
    };
    let key = claim_key(&job.execution);
    match registry.claim(key) {
        Ok(claim) => {
            return indeterminate(
                "existing attempt is never executed again",
                claim.start.as_ref(),
            );
        }
        Err(RegistryError::EvidenceConflict) => {} // Authoritative missing key.
        Err(_) => return indeterminate("prior admission is unreadable", None),
    }
    // Returning from the effect worker is the quiescence boundary. Only this
    // supervisor settles NoMutation; the worker cannot perform any later rename.
    let state = run_effect_worker(&job, &mut registry);
    let ToolAttemptState::Settled {
        effect,
        receipt: Some(receipt),
        ..
    } = &state
    else {
        return state;
    };
    let Ok(start) = serde_json::from_value::<RegistryReceipt>(receipt.data.clone()) else {
        return indeterminate("invalid worker receipt", None);
    };
    let proof = match effect {
        crate::EffectSummary::NoMutation => EditTermination::JoinedWithoutRename,
        crate::EffectSummary::KnownChanges { .. } => EditTermination::Replaced {
            destination_parent_synced: true,
            staging_parent_synced: true,
        },
        _ => return indeterminate("invalid worker terminal effect", Some(&start)),
    };
    #[cfg(test)]
    if job.fault.load(Ordering::SeqCst) == 9 {
        registry.lose_commit_ack();
    }
    if !resolve_confirmed(&mut registry, key, &start, effect.clone(), proof) {
        return indeterminate("workspace registry resolution is uncertain", Some(&start));
    }
    #[cfg(test)]
    trip_fault(&job.fault, FAULT_AFTER_REGISTRY_COMMIT);
    state
}

fn run_effect_worker(job: &EditJob, registry: &mut WorkspaceRegistry) -> ToolAttemptState {
    if job.stop.is_cancelled() {
        return not_started("edit cancelled before admission");
    }
    match open_absolute_directory(&job.execution.workspace.canonical_root)
        .and_then(|root| physical_identity(&root))
    {
        Ok(identity) if identity == job.root_identity => {}
        _ => return not_started("workspace root identity changed before edit admission"),
    }
    if registry.verify_current(&job.execution.workspace).is_err() {
        return not_started("workspace binding changed before edit admission");
    }
    let current_revision = match registry.revision(&job.execution.workspace) {
        Ok(revision) => revision,
        Err(_) => return not_started("workspace revision is unavailable"),
    };
    if EditRevision::from(current_revision) != job.arguments.base_revision
        || current_revision.files != job.execution.action.workspace_revision.unwrap_or(u64::MAX)
    {
        return not_started("workspace base revision is stale");
    }
    if !matches!(
        registry.mutation_resources(&job.execution.workspace),
        Ok(resources) if resources == job.resources
    ) {
        return not_started("workspace resource scope changed");
    }
    let replacement = match replacement(&job.arguments, job.max_file_bytes) {
        Ok(replacement) => replacement,
        Err(_) => return not_started("edit does not describe one bounded exact replacement"),
    };
    if !result_fits(
        &job.arguments.path,
        &replacement,
        job.execution.output_limit,
    ) {
        return not_started("edit result exceeds its persisted output bound");
    }
    let (parent, leaf) = match open_relative_parent(&job.root, &job.arguments.path) {
        Ok(parent) => parent,
        Err(_) => return not_started("edit target parent is unavailable"),
    };
    let (base_identity, base_mode, base_bytes) =
        match read_target(&parent, &leaf, job.max_file_bytes) {
            Ok(target) => target,
            Err(_) => return not_started("edit target is not a bounded regular file"),
        };
    if base_bytes != job.arguments.expected_content.as_bytes()
        || digest_text_bytes(&base_bytes) != job.arguments.expected_digest
    {
        return not_started("edit target content differs from the prepared base");
    }
    let key = claim_key(&job.execution);
    if !job.admission_allowed(job.live_authority.load(Ordering::SeqCst)) {
        return not_started("live edit authority denied before admission");
    }
    if job.stop.is_cancelled() {
        return not_started("edit cancelled before admission");
    }
    if stage_capacity(&job.staging).is_err() || qualify_filesystem(&parent, &job.staging).is_err() {
        return not_started("private staging capacity or filesystem is unavailable");
    }
    let action = edit_action(&job.execution, &job.arguments, &replacement);
    #[cfg(test)]
    if job.fault.load(Ordering::SeqCst) == 6 {
        registry.lose_commit_ack();
    }
    let claim = match registry.admit_edit(
        &job.execution.workspace,
        key,
        job.resources,
        current_revision,
        action.clone(),
    ) {
        Ok(claim) => claim,
        Err(_) => match registry.claim(key) {
            Ok(claim)
                if claim_matches(
                    &claim,
                    &job.execution,
                    &job.arguments,
                    &action,
                    job.resources,
                ) =>
            {
                claim
            }
            // An authoritative missing key proves this call never crossed edit
            // admission. An unreadable claim, unlike a refused write, is not proof.
            Err(RegistryError::EvidenceConflict) => {
                return not_started("edit admission was refused before physical start");
            }
            _ => return indeterminate("edit admission could not be confirmed", None),
        },
    };
    let start = claim.start.as_ref().expect("edit admission mints receipt");
    let edit = claim.edit.as_ref().expect("edit admission mints manifest");
    if edit.staged.is_some() || edit.rename_armed.is_some() || claim.terminal.is_some() {
        return indeterminate("existing edit cannot authorize execution", Some(start));
    }
    if job.stop.is_cancelled() || !job.admission_allowed(job.live_authority.load(Ordering::SeqCst))
    {
        return aborted_edit(start);
    }
    let temporary = &edit.manifest.stage_slot;
    #[cfg(test)]
    if job.fault.load(Ordering::SeqCst) == 10 {
        create_temporary(&job.staging, temporary)
            .unwrap()
            .write_all(b"occupant")
            .unwrap();
    }
    let mut staged = match create_temporary(&job.staging, temporary) {
        Ok(file) => file,
        // An existing occupant is never opened, adopted, truncated or removed.
        Err(_) => return aborted_edit(start),
    };
    #[cfg(test)]
    if job.fault.load(Ordering::SeqCst) == 20 {
        staged.write_all(b"partial").unwrap();
        return aborted_edit(start);
    }
    if staged.write_all(&replacement).is_err()
        || fchmod(&staged, Mode::from_raw_mode(base_mode as _)).is_err()
        || staged.sync_all().is_err()
        || job.staging.sync_all().is_err()
    {
        return aborted_edit(start);
    }
    let (Ok(staged_identity), Ok(stage_parent), Ok(target_parent)) = (
        physical_identity(&staged),
        physical_identity(&job.staging),
        physical_identity(&parent),
    ) else {
        return aborted_edit(start);
    };
    let fact = EditStaged {
        file: staged_identity,
        parent: stage_parent,
        content: action.replacement,
        file_synced: true,
        parent_synced: true,
    };
    #[cfg(test)]
    if job.fault.load(Ordering::SeqCst) == 7 {
        registry.lose_commit_ack();
    }
    if registry
        .record_edit_staged(key, start, fact.clone())
        .is_err()
        && !registry.claim(key).is_ok_and(|claim| {
            claim
                .edit
                .is_some_and(|edit| edit.staged.as_ref() == Some(&fact))
        })
    {
        return aborted_edit(start);
    }
    #[cfg(test)]
    test_phase(job, 4);
    if job.stop.is_cancelled() || !job.admission_allowed(job.live_authority.load(Ordering::SeqCst))
    {
        return aborted_edit(start);
    }
    match read_target(&parent, &leaf, job.max_file_bytes) {
        Ok((identity, _, content))
            if identity == base_identity
                && content == job.arguments.expected_content.as_bytes() => {}
        _ => return aborted_edit(start),
    }
    if !target_matches_staged(
        &job.staging,
        temporary,
        staged_identity,
        &replacement,
        job.max_file_bytes,
    ) {
        return aborted_edit(start);
    }
    let armed = EditRenameArmed {
        staged_file: staged_identity,
        target_file: base_identity,
        target_parent,
    };
    #[cfg(test)]
    if job.fault.load(Ordering::SeqCst) == 8 {
        registry.lose_commit_ack();
    }
    if registry
        .record_edit_rename_armed(key, start, armed.clone())
        .is_err()
        && !registry.claim(key).is_ok_and(|claim| {
            claim
                .edit
                .is_some_and(|edit| edit.rename_armed.as_ref() == Some(&armed))
        })
    {
        return aborted_edit(start);
    }
    #[cfg(test)]
    test_phase(job, 5);
    if job.stop.is_cancelled() || !job.admission_allowed(job.live_authority.load(Ordering::SeqCst))
    {
        return aborted_edit(start);
    }
    // Exactly one invocation, only by this worker after a confirmed durable arm.
    // Errors are ambiguous and must never fall back to copying or another rename.
    if renameat(&job.staging, temporary, &parent, &leaf).is_err() {
        return indeterminate("atomic replacement outcome is unknown", Some(start));
    }
    #[cfg(test)]
    trip_fault(&job.fault, FAULT_AFTER_RENAME);
    if parent.sync_all().is_err() || job.staging.sync_all().is_err() {
        return indeterminate("replacement directory barriers failed", Some(start));
    }
    settled_edit(
        &job.arguments,
        &replacement,
        known_change(&job.arguments.path),
        start,
    )
}

async fn reconcile_edit(
    boundary: &NativeEditBoundary,
    execution: ToolExecution,
    attempt: ToolAttempt,
) -> ToolAttemptState {
    // Keep the exact Session receipt on EVERY uncertain exit, even malformed or
    // conflicting receipts. Host evidence never rewrites immutable Session truth.
    let retained = match &attempt.state {
        ToolAttemptState::IntentCommitted { start_receipt } => start_receipt.clone(),
        ToolAttemptState::Indeterminate { receipt, .. }
        | ToolAttemptState::Settled { receipt, .. } => receipt.clone(),
        ToolAttemptState::NotStarted { .. } => None,
    };
    let uncertain =
        |reason: &str, host: Option<&RegistryReceipt>| ToolAttemptState::Indeterminate {
            reason: reason.into(),
            receipt: retained.clone().or_else(|| host.map(session_receipt)),
        };
    let _custody = match acquire_custody(&boundary.registry_directory) {
        Ok(lock) => lock,
        Err(_) => {
            return uncertain(
                "edit worker still owns custody or custody is unavailable",
                None,
            );
        }
    };
    let Some(arguments) = boundary.prepared_arguments(&execution.action) else {
        return uncertain("persisted prepared edit action is invalid", None);
    };
    if execution.binding != boundary.binding
        || execution.workspace != boundary.workspace
        || execution.action.authority != ToolAuthority::WorkspaceMutation
        || attempt.id != execution.attempt
        || attempt.invocation != execution.invocation
        || attempt.executor != boundary.executor
    {
        return uncertain("edit reconciliation binding changed", None);
    }
    let key = claim_key(&execution);
    let mut registry = match WorkspaceRegistry::open(&boundary.registry_directory) {
        Ok(registry) => registry,
        Err(_) => {
            return uncertain("workspace registry is unavailable", None);
        }
    };
    let claim = match registry.claim(key) {
        Ok(claim) => claim,
        Err(_) => {
            return uncertain("durable edit claim is unavailable", None);
        }
    };
    let replacement = match replacement(&arguments, boundary.max_file_bytes) {
        Ok(replacement) => replacement,
        Err(_) => return uncertain("persisted replacement is invalid", None),
    };
    let action = edit_action(&execution, &arguments, &replacement);
    if !claim_matches(&claim, &execution, &arguments, &action, boundary.resources) {
        return uncertain("durable edit manifest does not match", None);
    }
    let (Some(start), Some(edit)) = (&claim.start, &claim.edit) else {
        return uncertain("edit receipt or manifest is unavailable", None);
    };
    if retained
        .as_ref()
        .is_some_and(|receipt| receipt != &session_receipt(start))
    {
        return uncertain("Session and host edit receipts conflict", Some(start));
    }
    if !result_fits(&arguments.path, &replacement, execution.output_limit) {
        return uncertain(
            "edit result exceeds its persisted output bound",
            Some(start),
        );
    }
    let effect = known_change(&arguments.path);
    if let Some(terminal) = &claim.terminal {
        if terminal.receipt == *start {
            match (&terminal.effect, &edit.termination) {
                (crate::EffectSummary::NoMutation, Some(EditTermination::JoinedWithoutRename)) => {
                    return aborted_edit(start);
                }
                (
                    actual,
                    Some(EditTermination::Replaced {
                        destination_parent_synced: true,
                        staging_parent_synced: true,
                    }),
                ) if actual == &effect => {
                    return settled_edit(&arguments, &replacement, effect, start);
                }
                _ => {}
            }
        }
        return uncertain("terminal host evidence conflicts with edit", Some(start));
    }
    // Process loss is not the owning worker's no-rename attestation. Even a
    // pre-armed orphan stays quarantined in this deliberately conservative slice.
    let (Some(staged), Some(armed), Some(root), Some(stage_parent)) = (
        &edit.staged,
        &edit.rename_armed,
        &boundary.root,
        &boundary.staging,
    ) else {
        return uncertain("unresolved edit lacks physical rename proof", Some(start));
    };
    if !boundary.workspace_is_current(&execution.workspace)
        || physical_identity(stage_parent).ok() != Some(staged.parent)
    {
        return uncertain("workspace or staging identity changed", Some(start));
    }
    let (parent, leaf) = match open_relative_parent(root, &arguments.path) {
        Ok(parent) => parent,
        Err(_) => return uncertain("target parent is unavailable", Some(start)),
    };
    if physical_identity(&parent).ok() != Some(armed.target_parent)
        || !target_matches_staged(
            &parent,
            &leaf,
            staged.file,
            &replacement,
            boundary.max_file_bytes,
        )
    {
        return uncertain(
            "replacement identity or exact content is unproven",
            Some(start),
        );
    }
    let _stage_custody = match lock_staging(stage_parent) {
        Ok(lock) => lock,
        Err(_) => return uncertain("private staging is busy", Some(start)),
    };
    if boundary.sync_recovered_directory(&parent, false).is_err()
        || boundary
            .sync_recovered_directory(stage_parent, true)
            .is_err()
    {
        return uncertain("replacement directory barriers are uncertain", Some(start));
    }
    if !resolve_confirmed(
        &mut registry,
        key,
        start,
        effect.clone(),
        EditTermination::Replaced {
            destination_parent_synced: true,
            staging_parent_synced: true,
        },
    ) {
        return uncertain("workspace registry resolution is uncertain", Some(start));
    }
    settled_edit(&arguments, &replacement, effect, start)
}

fn target_matches_staged(
    parent: &File,
    leaf: &str,
    staged: PhysicalIdentity,
    replacement: &[u8],
    max_file_bytes: usize,
) -> bool {
    read_target(parent, leaf, max_file_bytes).is_ok_and(|(identity, _, content)| {
        identity == staged
            && content == replacement
            && digest_text_bytes(&content) == ContentDigest::of_bytes(replacement).to_string()
    })
}

fn result_fits(path: &str, replacement: &[u8], output_limit: usize) -> bool {
    let result = ToolResult {
        value: json!({
            "path": path,
            "bytes": replacement.len(),
            "digest": ContentDigest::of_bytes(replacement).to_string(),
        }),
        is_error: false,
        capture: crate::OutputCapture::CompleteInline,
    };
    serde_json::to_vec(&result)
        .is_ok_and(|bytes| bytes.len() <= output_limit.min(crate::MAX_TOOL_RECORD_BYTES))
}

fn settled_edit(
    arguments: &EditArguments,
    replacement: &[u8],
    effect: crate::EffectSummary,
    start: &RegistryReceipt,
) -> ToolAttemptState {
    let result = ToolResult {
        value: json!({
            "path": arguments.path,
            "bytes": replacement.len(),
            "digest": ContentDigest::of_bytes(replacement).to_string(),
        }),
        is_error: false,
        capture: crate::OutputCapture::CompleteInline,
    };
    ToolAttemptState::Settled {
        result,
        effect,
        retryable: false,
        receipt: Some(session_receipt(start)),
    }
}

fn session_receipt(receipt: &RegistryReceipt) -> StartReceipt {
    StartReceipt {
        kind: IMPLEMENTATION_ID.to_owned(),
        data: serde_json::to_value(receipt).unwrap_or(Value::Null),
    }
}

fn edit_action(
    execution: &ToolExecution,
    arguments: &EditArguments,
    replacement: &[u8],
) -> EditAction {
    EditAction {
        action_digest: execution.action.digest,
        tool_binding: ContentDigest::of(&execution.binding).expect("serializable frozen binding"),
        target: arguments.path.clone(),
        expected: EditContent {
            digest: ContentDigest::of_bytes(arguments.expected_content.as_bytes()),
            bytes: arguments.expected_content.len() as u64,
        },
        replacement: EditContent {
            digest: ContentDigest::of_bytes(replacement),
            bytes: replacement.len() as u64,
        },
    }
}

fn claim_matches(
    claim: &WorkspaceClaim,
    execution: &ToolExecution,
    arguments: &EditArguments,
    action: &EditAction,
    resources: WorkspaceResources,
) -> bool {
    claim.key == claim_key(execution)
        && claim.binding == execution.workspace
        && claim.resources == resources
        && claim.base == arguments.base_revision.into()
        && claim
            .edit
            .as_ref()
            .is_some_and(|edit| edit.manifest.action == *action)
}

fn aborted_edit(start: &RegistryReceipt) -> ToolAttemptState {
    ToolAttemptState::Settled {
        result: ToolResult {
            value: json!("Edit stopped before rename."),
            is_error: true,
            capture: crate::OutputCapture::CompleteInline,
        },
        effect: crate::EffectSummary::NoMutation,
        retryable: false,
        receipt: Some(session_receipt(start)),
    }
}

fn resolve_confirmed(
    registry: &mut WorkspaceRegistry,
    key: ClaimKey,
    receipt: &RegistryReceipt,
    effect: crate::EffectSummary,
    proof: EditTermination,
) -> bool {
    let evidence = TerminalEvidence {
        receipt: receipt.clone(),
        effect,
    };
    registry
        .resolve_edit(key, evidence.clone(), proof.clone())
        .is_ok()
        || registry.claim(key).is_ok_and(|claim| {
            claim.terminal.as_ref() == Some(&evidence)
                && claim
                    .edit
                    .is_some_and(|edit| edit.termination.as_ref() == Some(&proof))
        })
}

/// Host must protect this permanent inode and its ancestors from removal and
/// replacement. It is never unlinked, including on process loss or cleanup.
fn acquire_custody(directory: &Path) -> std::io::Result<File> {
    let parent = open_absolute_directory(
        directory
            .to_str()
            .ok_or_else(|| std::io::Error::other("non-UTF8 registry"))?,
    )?;
    let lock = File::from(openat(
        &parent,
        CUSTODY_LEAF,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    let meta = lock.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.len() != 0
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::other("invalid permanent custody inode"));
    }
    lock.try_lock()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    parent.sync_all()?;
    Ok(lock)
}

fn lock_staging(parent: &File) -> std::io::Result<File> {
    let lock = File::from(openat(
        parent,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    lock.try_lock()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(lock)
}

fn stage_capacity(parent: &File) -> std::io::Result<()> {
    let mut count = 0;
    for entry in rustix::fs::Dir::read_from(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        count += 1;
        if count >= MAX_STAGE_FILES {
            return Err(std::io::Error::other("private staging count limit"));
        }
        let stat = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_size < 0
            || stat.st_size as u64 > MAX_NATIVE_EDIT_BYTES as u64
        {
            return Err(std::io::Error::other("unqualified staging occupant"));
        }
    }
    Ok(())
}

fn qualify_filesystem(target: &File, stage: &File) -> std::io::Result<()> {
    let a = rustix::fs::fstatfs(target)?;
    let b = rustix::fs::fstatfs(stage)?;
    if target.metadata()?.dev() != stage.metadata()?.dev() || a.f_type != b.f_type {
        return Err(std::io::Error::other("staging is on another filesystem"));
    }
    // A bind-mounted alias may have the same device and filesystem type while
    // naming an agent-writable directory or producing EXDEV at rename. Require
    // the exact same mount, not just the same underlying superblock.
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::StatxFlags;
        let mount_id = |directory: &File| -> std::io::Result<u64> {
            let stat = rustix::fs::statx(directory, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
            if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
                return Err(std::io::Error::other(
                    "filesystem mount identity is unavailable",
                ));
            }
            Ok(stat.stx_mnt_id)
        };
        if mount_id(target)? != mount_id(stage)? {
            return Err(std::io::Error::other("staging and target mounts differ"));
        }
    }
    // Deliberately narrow local-filesystem support. No network/FUSE/overlay
    // durability claims. Rename errors still remain uncertain, never copied.
    #[cfg(target_os = "linux")]
    let supported = matches!(a.f_type as u64, 0xef53 | 0x58465342 | 0x9123683e);
    #[cfg(target_os = "macos")]
    let supported = a
        .f_fstypename
        .iter()
        .map(|c| *c as u8)
        .take_while(|c| *c != 0)
        .eq(b"apfs".iter().copied());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let supported = false;
    if !supported {
        return Err(std::io::Error::other(
            "filesystem is not qualified for native edit",
        ));
    }
    Ok(())
}

fn claim_key(execution: &ToolExecution) -> ClaimKey {
    ClaimKey {
        session: execution.session,
        invocation: execution.invocation,
        attempt: execution.attempt,
    }
}

fn known_change(path: &str) -> crate::EffectSummary {
    crate::EffectSummary::KnownChanges {
        paths: vec![path.to_owned()],
    }
}

fn replacement(arguments: &EditArguments, maximum: usize) -> Result<Vec<u8>, ()> {
    if arguments.old_text.is_empty()
        || arguments.old_text == arguments.new_text
        || arguments.expected_content.len() > maximum
        || arguments.expected_digest != digest_text(&arguments.expected_content)
        || arguments.expected_content.len() > maximum
    {
        return Err(());
    }
    let mut found = None;
    for (index, _) in arguments.expected_content.char_indices() {
        if arguments.expected_content[index..].starts_with(&arguments.old_text)
            && found.replace(index).is_some()
        {
            return Err(());
        }
    }
    let start = found.ok_or(())?;
    let end = start.checked_add(arguments.old_text.len()).ok_or(())?;
    let target_len = arguments
        .expected_content
        .len()
        .checked_sub(arguments.old_text.len())
        .and_then(|length| length.checked_add(arguments.new_text.len()))
        .filter(|length| *length <= maximum)
        .ok_or(())?;
    if arguments.expected_content[start..end] != arguments.old_text {
        return Err(());
    }
    let mut target = String::with_capacity(target_len);
    target.push_str(&arguments.expected_content[..start]);
    target.push_str(&arguments.new_text);
    target.push_str(&arguments.expected_content[end..]);
    Ok(target.into_bytes())
}

fn digest_text(text: &str) -> String {
    ContentDigest::of_bytes(text.as_bytes()).to_string()
}

fn digest_text_bytes(text: &[u8]) -> String {
    ContentDigest::of_bytes(text).to_string()
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\0')
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.eq_ignore_ascii_case(".git")
        })
}

fn open_absolute_directory(path: &str) -> std::io::Result<File> {
    if path.len() > MAX_PATH_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root path is too long",
        ));
    }
    let relative = path.strip_prefix('/').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root is not absolute",
        )
    })?;
    let mut current = File::from(open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if relative.is_empty() {
        return Ok(current);
    }
    for component in relative.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace root is not canonical",
            ));
        }
        current = File::from(openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Ok(current)
}

fn open_relative_parent(root: &File, path: &str) -> std::io::Result<(File, String)> {
    if !valid_relative_path(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid relative path",
        ));
    }
    let mut components = path.split('/').peekable();
    let mut parent = root.try_clone()?;
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            return Ok((parent, component.to_owned()));
        }
        parent = File::from(openat(
            &parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "empty relative path",
    ))
}

fn read_target(
    parent: &File,
    leaf: &str,
    maximum: usize,
) -> std::io::Result<(PhysicalIdentity, u32, Vec<u8>)> {
    let before = statat(parent, leaf, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target is not a regular file",
        ));
    }
    let fd: OwnedFd = openat(
        parent,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let file = File::from(fd);
    if FileType::from_raw_mode(fstat(&file)?.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target is not a regular file",
        ));
    }
    let metadata = file.metadata()?;
    let identity = physical_identity(&file)?;
    if identity.device != before.st_dev as u64 || identity.inode != before.st_ino as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target changed during descriptor open",
        ));
    }
    let mut content = Vec::with_capacity(maximum.saturating_add(1));
    file.take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut content)?;
    if content.len() > maximum || std::str::from_utf8(&content).is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target exceeds its text bound or is not UTF-8",
        ));
    }
    let mode = metadata.permissions().mode() & 0o7777;
    Ok((identity, mode, content))
}

fn create_temporary(parent: &File, leaf: &str) -> std::io::Result<File> {
    let fd = openat(
        parent,
        leaf,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?;
    Ok(File::from(fd))
}

fn physical_identity(file: &File) -> std::io::Result<PhysicalIdentity> {
    let metadata = file.metadata()?;
    let birth = metadata.created().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem creation time is unavailable",
        )
    })?;
    let since_epoch = birth.duration_since(UNIX_EPOCH).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem creation time predates Unix epoch",
        )
    })?;
    Ok(PhysicalIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        birth_seconds: since_epoch.as_secs(),
        birth_nanos: since_epoch.subsec_nanos(),
    })
}

fn not_started(reason: &str) -> ToolAttemptState {
    ToolAttemptState::NotStarted {
        reason: reason.to_owned(),
    }
}

fn indeterminate(reason: &str, receipt: Option<&RegistryReceipt>) -> ToolAttemptState {
    ToolAttemptState::Indeterminate {
        reason: reason.to_owned(),
        receipt: receipt.map(session_receipt),
    }
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

impl EditJob {
    fn admission_allowed(&self, policy: u8) -> bool {
        if self.execution.binding != self.binding
            || self.execution.action.authority != ToolAuthority::WorkspaceMutation
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
            AUTHORITY_DENY => false,
            _ => false,
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
fn test_phase(job: &EditJob, point: u8) {
    if job.pause.point.load(Ordering::SeqCst) == point {
        job.pause.entered.wait();
        job.pause.release.wait();
    }
    if job.fault.load(Ordering::SeqCst) == point + 10 {
        job.stop.cancel();
        return;
    }
    trip_fault(&job.fault, point);
}

#[cfg(test)]
struct TestPause {
    point: AtomicU8,
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
}

#[cfg(test)]
impl Default for TestPause {
    fn default() -> Self {
        Self {
            point: AtomicU8::new(0),
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        }
    }
}

#[cfg(test)]
fn trip_fault(fault: &AtomicU8, point: u8) {
    if fault.load(Ordering::SeqCst) == 31 && point == FAULT_AFTER_RENAME {
        std::process::exit(73);
    }
    if fault
        .compare_exchange(point, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        panic!("injected native-edit crash point {point}");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::{AttemptId, AuthorityCeiling, InvocationId, SessionId};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ion-native-edit-{}-{id}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        root: TestRoot,
        host: TestRoot,
        registry: WorkspaceRegistry,
        boundary: NativeEditBoundary,
    }

    impl Fixture {
        fn new(git: bool) -> Self {
            let root = TestRoot::new();
            if git {
                fs::create_dir(root.path().join(".git")).unwrap();
            }
            fs::write(root.path().join("file.txt"), "alpha beta alpha\n").unwrap();
            let host = TestRoot::new();
            let mut registry = WorkspaceRegistry::open(host.path()).unwrap();
            let workspace = registry
                .bind("test-workspace", root.path(), "local-v1")
                .unwrap();
            let stage = host.path().join("staging");
            fs::create_dir(&stage).unwrap();
            fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
            let boundary = NativeEditBoundary::new(&registry, workspace, 1024, &stage).unwrap();
            boundary.set_live_authority(LiveToolAuthority::Allow);
            Self {
                root,
                host,
                registry,
                boundary,
            }
        }

        fn arguments(&self, old_text: &str, new_text: &str) -> Value {
            let revision = self
                .registry
                .revision(self.boundary.workspace_binding())
                .unwrap();
            let content = fs::read_to_string(self.root.path().join("file.txt")).unwrap();
            json!({
                "path": "file.txt",
                "expected_content": content,
                "workspace_revision": revision,
                "old_text": old_text,
                "new_text": new_text,
            })
        }

        fn execution(&self, action: PreparedAction) -> ToolExecution {
            ToolExecution {
                session: SessionId::new(),
                invocation: InvocationId::new(1).unwrap(),
                attempt: AttemptId::new(1).unwrap(),
                effect_key: "native-edit-test".into(),
                binding: self.boundary.binding(),
                action,
                workspace: self.boundary.workspace.clone(),
                ceiling: AuthorityCeiling {
                    workspace_mutation: true,
                    unconfined_execution: false,
                    remote_tools: false,
                    egress_realms: vec![EgressRealm::Local],
                },
                approval: ApprovalState::NotRequired,
                output_limit: 4096,
                artifacts: crate::ArtifactPublisher::closed(),
            }
        }
    }

    fn attempt(execution: &ToolExecution, state: ToolAttemptState) -> ToolAttempt {
        ToolAttempt {
            id: execution.attempt,
            invocation: execution.invocation,
            ordinal: 1,
            generation: 0,
            executor: SemanticCompatibilityId::new("local-v1").unwrap(),
            progress: None,
            state,
        }
    }

    #[tokio::test]
    async fn edit_requires_exact_expected_content_and_one_occurrence() {
        let fixture = Fixture::new(false);
        let binding = fixture.boundary.binding();
        let arguments = fixture.arguments("alpha", "gamma");
        assert!(matches!(
            crate::tool_boundary::prepare_action(&fixture.boundary, &binding, arguments),
            Err(ToolBoundaryError::InvalidArguments)
        ));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha beta alpha\n"
        );
        let one = fixture.arguments("beta", "gamma");
        let action = fixture.boundary.prepare(one).unwrap();
        assert_eq!(
            action.arguments["expected_digest"],
            digest_text("alpha beta alpha\n")
        );
        assert_eq!(action.authority, ToolAuthority::WorkspaceMutation);
        assert_eq!(action.workspace_revision, Some(0));
        assert_eq!(action.base_facts.len(), 1);
        let state = fixture
            .boundary
            .execute(fixture.execution(action), CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
    }

    #[test]
    fn protected_git_metadata_cannot_be_prepared_for_mutation() {
        let fixture = Fixture::new(true);
        fs::write(
            fixture.root.path().join(".git/config"),
            "alpha beta alpha\n",
        )
        .unwrap();
        for path in [".git/config", ".GiT/config", "foo/.git/config"] {
            let mut arguments = fixture.arguments("beta", "gamma");
            arguments["path"] = json!(path);
            assert!(matches!(
                crate::tool_boundary::prepare_action(
                    &fixture.boundary,
                    &fixture.boundary.binding(),
                    arguments
                ),
                Err(ToolBoundaryError::InvalidArguments)
            ));
        }
    }

    #[tokio::test]
    async fn redirected_git_and_common_admin_roots_cannot_be_edited_or_bound_as_ordinary_files() {
        let root = TestRoot::new();
        let host = TestRoot::new();
        fs::create_dir(root.path().join("admin")).unwrap();
        fs::create_dir(root.path().join("shared")).unwrap();
        fs::write(root.path().join(".git"), "gitdir: admin\n").unwrap();
        fs::write(root.path().join("admin/commondir"), "../shared\n").unwrap();
        fs::write(root.path().join("admin/config"), "alpha beta alpha\n").unwrap();
        fs::write(root.path().join("shared/HEAD"), "alpha beta alpha\n").unwrap();
        let mut registry = WorkspaceRegistry::open(host.path()).unwrap();
        let workspace = registry.bind("workspace", root.path(), "local-v1").unwrap();
        let revision = registry.revision(&workspace).unwrap();
        let stage = host.path().join("staging");
        fs::create_dir(&stage).unwrap();
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
        let boundary = NativeEditBoundary::new(&registry, workspace, 1024, &stage).unwrap();
        for path in ["admin/config", "shared/HEAD", ".git"] {
            let arguments = json!({
                "path": path,
                "expected_content": "alpha beta alpha\n",
                "workspace_revision": revision,
                "old_text": "beta",
                "new_text": "gamma",
            });
            assert!(matches!(
                crate::tool_boundary::prepare_action(&boundary, &boundary.binding(), arguments),
                Err(ToolBoundaryError::InvalidArguments)
            ));
        }
        if root.path().join("ADMIN/config").is_file() {
            // Case-insensitive APFS resolves this spelling to admin/config.
            // Reject it at registry admission before staging or any claim.
            boundary.set_live_authority(LiveToolAuthority::Allow);
            let action = boundary
                .prepare(json!({
                    "path": "ADMIN/config", "expected_content": "alpha beta alpha\n",
                    "workspace_revision": revision, "old_text": "beta", "new_text": "gamma"
                }))
                .unwrap();
            let execution = ToolExecution {
                session: SessionId::new(),
                invocation: InvocationId::new(1).unwrap(),
                attempt: AttemptId::new(1).unwrap(),
                effect_key: "case-alias".into(),
                binding: boundary.binding(),
                action,
                workspace: boundary.workspace.clone(),
                ceiling: AuthorityCeiling {
                    workspace_mutation: true,
                    unconfined_execution: false,
                    remote_tools: false,
                    egress_realms: vec![EgressRealm::Local],
                },
                approval: ApprovalState::NotRequired,
                output_limit: 4096,
                artifacts: crate::ArtifactPublisher::closed(),
            };
            assert!(matches!(
                boundary.execute(execution, CancellationToken::new()).await,
                ToolAttemptState::NotStarted { .. }
            ));
            assert!(registry.unresolved(None, 8).unwrap().is_empty());
            assert_eq!(
                fs::read_to_string(root.path().join("admin/config")).unwrap(),
                "alpha beta alpha\n"
            );
            assert_eq!(fs::read_dir(&stage).unwrap().count(), 0);
        }
        let admin = registry
            .bind("admin", root.path().join("admin"), "local-v1")
            .unwrap();
        assert!(matches!(
            NativeEditBoundary::new(&registry, admin, 1024, &stage),
            Err(NativeEditError::InvalidWorkspace)
        ));
    }

    #[tokio::test]
    async fn approval_and_live_policy_are_rechecked_before_atomic_replacement() {
        let fixture = Fixture::new(false);
        fixture.boundary.set_live_authority(LiveToolAuthority::Ask);
        let action = fixture
            .boundary
            .prepare(fixture.arguments("beta", "gamma"))
            .unwrap();
        let mut execution = fixture.execution(action.clone());
        assert_eq!(
            fixture
                .boundary
                .live_authority(&action, &execution.workspace),
            LiveToolAuthority::Ask
        );
        assert!(matches!(
            fixture
                .boundary
                .execute(execution.clone(), CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        execution.approval = ApprovalState::Approved {
            action_digest: action.digest,
            implementation: execution.binding.implementation.clone(),
            executor: fixture.boundary.executor(),
            workspace: execution.workspace.clone(),
            expires_at_unix_ms: i64::MAX,
        };
        fixture.boundary.set_live_authority(LiveToolAuthority::Deny);
        assert!(matches!(
            fixture
                .boundary
                .execute(execution, CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha beta alpha\n"
        );
    }

    #[tokio::test]
    async fn crash_after_rename_before_registry_commit_reconciles_only_matching_inode_and_hash() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        let registry = WorkspaceRegistry::open(fixture.host.path()).unwrap();
        let claim = registry
            .claim(claim_key(&execution))
            .expect("durable start survives the fault");
        assert!(claim.start.is_some());
        assert!(claim.terminal.is_none());
        let uncertain_attempt = attempt(&execution, uncertain);
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), uncertain_attempt)
            .await;
        assert!(matches!(recovered, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
        assert!(
            WorkspaceRegistry::open(fixture.host.path())
                .unwrap()
                .unresolved(None, 8)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recovery_refuses_to_settle_visible_rename_until_directory_is_durable() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        fixture.boundary.inject_fault(FAULT_RECOVERY_DIRECTORY_SYNC);
        let again = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(again, ToolAttemptState::Indeterminate { .. }));
        let registry = WorkspaceRegistry::open(fixture.host.path()).unwrap();
        assert!(
            registry
                .claim(claim_key(&execution))
                .unwrap()
                .terminal
                .is_none()
        );
        assert_eq!(registry.revision(&execution.workspace).unwrap().files, 0);
        let settled = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, again))
            .await;
        assert!(matches!(settled, ToolAttemptState::Settled { .. }));
        assert_eq!(registry.revision(&execution.workspace).unwrap().files, 1);
    }

    #[tokio::test]
    async fn restored_base_or_wrong_hash_keeps_claim_unknown_without_retry() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        fs::write(fixture.root.path().join("file.txt"), "alpha beta alpha\n").unwrap();
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Indeterminate { .. }));
        assert_eq!(
            WorkspaceRegistry::open(fixture.host.path())
                .unwrap()
                .unresolved(None, 8)
                .unwrap()
                .len(),
            1
        );
        let retry = fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        assert!(matches!(retry, ToolAttemptState::Indeterminate { .. }));
    }

    #[tokio::test]
    async fn crash_after_registry_commit_before_session_evidence_reconciles_without_execute() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_REGISTRY_COMMIT);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        let claim = WorkspaceRegistry::open(fixture.host.path())
            .unwrap()
            .claim(claim_key(&execution))
            .unwrap();
        assert!(claim.terminal.is_some());
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
    }

    #[tokio::test]
    async fn committed_terminal_edit_reconciles_without_the_old_workspace() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_REGISTRY_COMMIT);
        let lost_reply = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(lost_reply, ToolAttemptState::Indeterminate { .. }));
        fs::remove_dir_all(fixture.root.path()).unwrap();
        assert!(
            NativeEditBoundary::new(
                &fixture.registry,
                execution.workspace.clone(),
                1024,
                &fixture.host.path().join("staging")
            )
            .is_err()
        );
        let recovery =
            NativeEditBoundary::new_recovery(&fixture.registry, execution.workspace.clone(), 1024)
                .unwrap();
        recovery.set_live_authority(LiveToolAuthority::Allow);
        assert_eq!(
            recovery.live_authority(&execution.action, &execution.workspace),
            LiveToolAuthority::Deny,
        );
        let settled = recovery
            .reconcile(execution.clone(), attempt(&execution, lost_reply))
            .await;
        assert!(matches!(settled, ToolAttemptState::Settled { .. }));
        assert!(matches!(
            recovery.execute(execution, CancellationToken::new()).await,
            ToolAttemptState::NotStarted { .. }
        ));
    }

    #[tokio::test]
    async fn committed_terminal_edit_reconciles_after_a_later_cooperating_mutation() {
        let fixture = Fixture::new(false);
        let first = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_REGISTRY_COMMIT);
        let lost_session_reply = fixture
            .boundary
            .execute(first.clone(), CancellationToken::new())
            .await;
        assert!(matches!(
            lost_session_reply,
            ToolAttemptState::Indeterminate { .. }
        ));

        let mut second = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("gamma", "delta"))
                .unwrap(),
        );
        second.invocation = InvocationId::new(2).unwrap();
        second.attempt = AttemptId::new(2).unwrap();
        let state = fixture
            .boundary
            .execute(second, CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha delta alpha\n"
        );

        let recovered = fixture
            .boundary
            .reconcile(first.clone(), attempt(&first, lost_session_reply))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha delta alpha\n"
        );
    }

    #[test]
    fn resource_scope_distinguishes_git_metadata_from_non_git_roots() {
        let non_git = Fixture::new(false);
        assert_eq!(non_git.boundary.resources, WorkspaceResources::Files);
        let git = Fixture::new(true);
        assert_eq!(
            git.boundary.resources,
            WorkspaceResources::FilesAndRepository
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bind_mounted_stage_alias_is_rejected_before_admission() {
        let Ok(home) = std::env::var("ION_EDIT_MOUNT_ALIAS_HOME") else {
            return; // The dedicated Docker test supplies a nested second mount.
        };
        let home = Path::new(&home);
        let root = home.join("workspace");
        let host = home.join("host");
        let stage = host.join("staging");
        let target = root.join("file.txt");
        fs::write(&target, b"alpha beta alpha\n").unwrap();
        let before = fs::read_dir(&root).unwrap().count();
        let mut registry = WorkspaceRegistry::open(&host).unwrap();
        let binding = registry.bind("workspace", &root, "local-v1").unwrap();
        let root_fd = open_absolute_directory(root.to_str().unwrap()).unwrap();
        let stage_fd = open_absolute_directory(stage.to_str().unwrap()).unwrap();
        let flags = rustix::fs::StatxFlags::MNT_ID;
        let mount = |fd: &File| {
            rustix::fs::statx(fd, "", AtFlags::EMPTY_PATH, flags)
                .unwrap()
                .stx_mnt_id
        };
        assert_ne!(
            mount(&root_fd),
            mount(&stage_fd),
            "test needs distinct bind mounts"
        );
        match NativeEditBoundary::new(&registry, binding, 1024, &stage) {
            Err(NativeEditError::Io(error))
                if error.to_string() == "staging and target mounts differ" => {}
            Err(other) => panic!("unexpected alias rejection: {other:?}"),
            Ok(_) => panic!("bind-mounted staging alias was accepted"),
        }
        assert_eq!(fs::read(&target).unwrap(), b"alpha beta alpha\n");
        assert_eq!(fs::read_dir(root).unwrap().count(), before);
        assert!(registry.unresolved(None, 8).unwrap().is_empty());
    }

    #[test]
    fn staging_constructor_refuses_missing_writable_workspace_and_symlink_roots() {
        let fixture = Fixture::new(false);
        let workspace = fixture.boundary.workspace.clone();
        let missing = fixture.host.path().join("absent");
        assert!(
            NativeEditBoundary::new(&fixture.registry, workspace.clone(), 1024, &missing).is_err()
        );
        assert!(!missing.exists());
        let internal = fixture.root.path().join("stage");
        fs::create_dir(&internal).unwrap();
        fs::set_permissions(&internal, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            NativeEditBoundary::new(&fixture.registry, workspace.clone(), 1024, &internal).is_err()
        );
        let unrelated = fixture
            .root
            .path()
            .parent()
            .unwrap()
            .join(format!("ion-unrelated-stage-{}", SessionId::new()));
        fs::create_dir(&unrelated).unwrap();
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            NativeEditBoundary::new(&fixture.registry, workspace.clone(), 1024, &unrelated)
                .is_err()
        );
        fs::remove_dir(&unrelated).unwrap();
        let stage = fixture.host.path().join("staging");
        let link = fixture.host.path().join("linked-stage");
        std::os::unix::fs::symlink(&stage, &link).unwrap();
        assert!(
            NativeEditBoundary::new(&fixture.registry, workspace.clone(), 1024, &link).is_err()
        );
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(NativeEditBoundary::new(&fixture.registry, workspace, 1024, &stage).is_err());
    }

    #[tokio::test]
    async fn source_directory_sync_failure_retains_claim_and_file_mode() {
        let fixture = Fixture::new(false);
        fs::set_permissions(
            fixture.root.path().join("file.txt"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        fixture.boundary.inject_fault(33);
        let state = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, state))
            .await;
        assert!(matches!(state, ToolAttemptState::Indeterminate { .. }));
        assert_eq!(
            fixture
                .registry
                .revision(&execution.workspace)
                .unwrap()
                .files,
            0
        );
        assert!(
            fixture
                .registry
                .claim(claim_key(&execution))
                .unwrap()
                .terminal
                .is_none()
        );
        let state = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, state))
            .await;
        assert!(matches!(state, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::metadata(fixture.root.path().join("file.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }

    #[tokio::test]
    async fn lost_commit_acknowledgements_are_read_back_at_every_phase() {
        for point in [6, 7, 8, 9] {
            let fixture = Fixture::new(false);
            let execution = fixture.execution(
                fixture
                    .boundary
                    .prepare(fixture.arguments("beta", "gamma"))
                    .unwrap(),
            );
            fixture.boundary.inject_fault(point);
            let state = fixture
                .boundary
                .execute(execution.clone(), CancellationToken::new())
                .await;
            assert!(
                matches!(
                    state,
                    ToolAttemptState::Settled {
                        effect: crate::EffectSummary::KnownChanges { .. },
                        ..
                    }
                ),
                "{point}: {state:?}"
            );
            let claim = fixture.registry.claim(claim_key(&execution)).unwrap();
            assert!(claim.edit.unwrap().rename_armed.is_some());
            assert_eq!(
                fixture
                    .registry
                    .revision(&execution.workspace)
                    .unwrap()
                    .files,
                1
            );
        }
    }

    #[tokio::test]
    async fn collision_preserves_occupant_and_abort_retention_is_bounded() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(10);
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(
            state,
            ToolAttemptState::Settled {
                effect: crate::EffectSummary::NoMutation,
                ..
            }
        ));
        let claim = fixture.registry.claim(claim_key(&execution)).unwrap();
        let stage = fixture.host.path().join("staging");
        assert_eq!(
            fs::read(stage.join(claim.edit.unwrap().manifest.stage_slot)).unwrap(),
            b"occupant"
        );
        for n in 1..MAX_STAGE_FILES {
            fs::write(stage.join(format!("occupied-{n}")), b"x").unwrap();
        }
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        let state = fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::NotStarted { .. }));
        assert_eq!(fs::read_dir(stage).unwrap().count(), MAX_STAGE_FILES);
        assert_eq!(fs::read_dir(fixture.root.path()).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_waiter_keeps_permanent_custody_until_worker_quiescence() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        let recovery = NativeEditBoundary::new(
            &fixture.registry,
            execution.workspace.clone(),
            1024,
            &fixture.host.path().join("staging"),
        )
        .unwrap();
        fixture.boundary.pause.point.store(5, Ordering::SeqCst);
        let pause = Arc::clone(&fixture.boundary.pause);
        let boundary = Arc::new(fixture.boundary);
        let worker = Arc::clone(&boundary);
        let owned_execution = execution.clone();
        let task = tokio::spawn(async move {
            worker
                .execute(owned_execution, CancellationToken::new())
                .await
        });
        pause.entered.wait();
        let inode = fs::metadata(fixture.host.path().join(CUSTODY_LEAF))
            .unwrap()
            .ino();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(acquire_custody(fixture.host.path()).is_err());
        let uncertain = ToolAttemptState::Indeterminate {
            reason: "waiter dropped".into(),
            receipt: None,
        };
        let state = recovery
            .reconcile(execution.clone(), attempt(&execution, uncertain.clone()))
            .await;
        assert!(matches!(state, ToolAttemptState::Indeterminate { .. }));
        assert!(
            fixture
                .registry
                .claim(claim_key(&execution))
                .unwrap()
                .terminal
                .is_none()
        );
        pause.release.wait();
        // Acquiring the permanent lock, not a timer, proves worker quiescence.
        let mut joined = false;
        for _ in 0..1000 {
            if let Ok(lock) = acquire_custody(fixture.host.path()) {
                drop(lock);
                joined = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(joined);
        assert_eq!(
            fs::metadata(fixture.host.path().join(CUSTODY_LEAF))
                .unwrap()
                .ino(),
            inode
        );
        let state = recovery
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(
            state,
            ToolAttemptState::Settled {
                effect: crate::EffectSummary::NoMutation,
                ..
            }
        ));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha beta alpha\n"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_custody_before_admission_proves_nonexecution() {
        let fixture = Fixture::new(false);
        let first = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        let second_boundary = NativeEditBoundary::new(
            &fixture.registry,
            first.workspace.clone(),
            1024,
            &fixture.host.path().join("staging"),
        )
        .unwrap();
        second_boundary.set_live_authority(LiveToolAuthority::Allow);
        let second_action = second_boundary
            .prepare(fixture.arguments("beta", "delta"))
            .unwrap();
        let mut second = fixture.execution(second_action);
        second.invocation = InvocationId::new(2).unwrap();
        second.attempt = AttemptId::new(2).unwrap();
        fixture.boundary.pause.point.store(5, Ordering::SeqCst);
        let pause = Arc::clone(&fixture.boundary.pause);
        let boundary = Arc::new(fixture.boundary);
        let running = Arc::clone(&boundary);
        let first_task =
            tokio::spawn(async move { running.execute(first, CancellationToken::new()).await });
        pause.entered.wait();

        let outcome = second_boundary
            .execute(second.clone(), CancellationToken::new())
            .await;
        assert!(matches!(outcome, ToolAttemptState::NotStarted { .. }));
        assert!(matches!(
            fixture.registry.claim(claim_key(&second)),
            Err(RegistryError::EvidenceConflict)
        ));
        pause.release.wait();
        assert!(matches!(
            first_task.await.unwrap(),
            ToolAttemptState::Settled { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_revocation_at_staged_and_armed_barriers_aborts_without_rename() {
        for point in [4, 5] {
            let fixture = Fixture::new(false);
            let execution = fixture.execution(
                fixture
                    .boundary
                    .prepare(fixture.arguments("beta", "gamma"))
                    .unwrap(),
            );
            fixture.boundary.pause.point.store(point, Ordering::SeqCst);
            let pause = Arc::clone(&fixture.boundary.pause);
            let boundary = Arc::new(fixture.boundary);
            let worker = Arc::clone(&boundary);
            let owned = execution.clone();
            let task =
                tokio::spawn(async move { worker.execute(owned, CancellationToken::new()).await });
            pause.entered.wait();
            let claim = fixture.registry.claim(claim_key(&execution)).unwrap();
            assert!(claim.edit.as_ref().unwrap().staged.is_some());
            assert_eq!(
                claim.edit.as_ref().unwrap().rename_armed.is_some(),
                point == 5
            );
            boundary.set_live_authority(LiveToolAuthority::Deny);
            pause.release.wait();
            assert!(matches!(
                task.await.unwrap(),
                ToolAttemptState::Settled {
                    effect: crate::EffectSummary::NoMutation,
                    ..
                }
            ));
            assert_eq!(
                fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
                "alpha beta alpha\n"
            );
            assert_eq!(
                fixture
                    .registry
                    .revision(&execution.workspace)
                    .unwrap()
                    .files,
                0
            );
        }
    }

    #[tokio::test]
    async fn identical_replacement_bytes_on_another_inode_are_not_recovery_proof() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        fs::write(fixture.root.path().join("other"), "alpha gamma alpha\n").unwrap();
        fs::rename(
            fixture.root.path().join("other"),
            fixture.root.path().join("file.txt"),
        )
        .unwrap();
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, state))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Indeterminate { .. }));
        assert!(
            fixture
                .registry
                .claim(claim_key(&execution))
                .unwrap()
                .terminal
                .is_none()
        );
    }

    #[tokio::test]
    async fn armed_crash_never_replays_and_conflicting_session_receipt_is_preserved() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(5);
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        let state = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, state))
            .await;
        assert!(matches!(state, ToolAttemptState::Indeterminate { .. }));
        assert!(
            fixture
                .registry
                .claim(claim_key(&execution))
                .unwrap()
                .terminal
                .is_none()
        );
        let receipt = StartReceipt {
            kind: "incompatible".into(),
            data: json!("immutable"),
        };
        let old = ToolAttemptState::Indeterminate {
            reason: "unknown".into(),
            receipt: Some(receipt.clone()),
        };
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, old))
            .await;
        assert!(
            matches!(recovered, ToolAttemptState::Indeterminate { receipt: Some(r), .. } if r == receipt)
        );
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha beta alpha\n"
        );
    }

    #[tokio::test]
    async fn process_loss_child() {
        let Ok(host) = std::env::var("ION_EDIT_TEST_HOST") else {
            return;
        };
        let workspace: WorkspaceBinding =
            serde_json::from_str(&std::env::var("ION_EDIT_TEST_WORKSPACE").unwrap()).unwrap();
        let registry = WorkspaceRegistry::open(&host).unwrap();
        let boundary = NativeEditBoundary::new(
            &registry,
            workspace.clone(),
            1024,
            &Path::new(&host).join("staging"),
        )
        .unwrap();
        boundary.set_live_authority(LiveToolAuthority::Allow);
        boundary.inject_fault(31);
        let fixture = Fixture {
            root: TestRoot(workspace.canonical_root.into()),
            host: TestRoot(host.into()),
            registry,
            boundary,
        };
        let mut execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        execution.session =
            serde_json::from_str(&std::env::var("ION_EDIT_TEST_SESSION").unwrap()).unwrap();
        fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        panic!("child must exit inside rename worker without destructors");
    }

    #[tokio::test]
    async fn actual_process_loss_after_rename_recovers_without_replay() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_edit::tests::process_loss_child",
                "--nocapture",
            ])
            .env("ION_EDIT_TEST_HOST", fixture.host.path())
            .env(
                "ION_EDIT_TEST_WORKSPACE",
                serde_json::to_string(&execution.workspace).unwrap(),
            )
            .env(
                "ION_EDIT_TEST_SESSION",
                serde_json::to_string(&execution.session).unwrap(),
            )
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(73));
        let claim = fixture.registry.claim(claim_key(&execution)).unwrap();
        assert!(claim.terminal.is_none());
        assert!(claim.edit.as_ref().unwrap().rename_armed.is_some());
        let state = ToolAttemptState::IntentCommitted {
            start_receipt: Some(session_receipt(claim.start.as_ref().unwrap())),
        };
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, state))
            .await;
        assert!(matches!(
            recovered,
            ToolAttemptState::Settled {
                effect: crate::EffectSummary::KnownChanges { .. },
                ..
            }
        ));
        assert_eq!(
            fixture
                .registry
                .revision(&execution.workspace)
                .unwrap()
                .files,
            1
        );
        assert_eq!(
            fs::read_dir(fixture.host.path().join("staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn immutable_receipt_survives_staged_and_armed_cancellation() {
        for point in [14, 15, 20] {
            let fixture = Fixture::new(false);
            let execution = fixture.execution(
                fixture
                    .boundary
                    .prepare(fixture.arguments("beta", "gamma"))
                    .unwrap(),
            );
            fixture.boundary.inject_fault(point);
            let state = fixture
                .boundary
                .execute(execution.clone(), CancellationToken::new())
                .await;
            let claim = fixture.registry.claim(claim_key(&execution)).unwrap();
            assert!(
                matches!(&state, ToolAttemptState::Settled { effect: crate::EffectSummary::NoMutation, receipt: Some(r), .. } if *r == session_receipt(claim.start.as_ref().unwrap()))
            );
            assert_eq!(
                claim.edit.as_ref().unwrap().termination,
                Some(EditTermination::JoinedWithoutRename)
            );
            assert_eq!(
                fixture
                    .registry
                    .revision(&execution.workspace)
                    .unwrap()
                    .files,
                0
            );
            assert_eq!(
                fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
                "alpha beta alpha\n"
            );
            assert_eq!(fs::read_dir(fixture.root.path()).unwrap().count(), 1);
            let recovered = fixture
                .boundary
                .reconcile(execution.clone(), attempt(&execution, state.clone()))
                .await;
            assert_eq!(state, recovered);
        }
    }
}

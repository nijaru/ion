//! Linux native command execution in a per-attempt Bubblewrap PID namespace.
//!
//! The registry claim precedes launch. Bubblewrap blocks on stdin after namespace
//! setup, so the command cannot start until a durable start receipt is recorded.
//! Its namespace init has a retained pidfd; only an observed terminal pidfd and
//! joined Bubblewrap process permit settlement. A crashed owner leaves the claim
//! quarantined. Commands see a bounded private workspace view, never the live
//! checkout or host registry. A stopped command's ordinary file changes are
//! imported only after the scope is positively terminal.
//! The protected registry is authoritative for each attempt's start/terminal
//! facts; recovery adopts only a matching terminal claim and never infers stop
//! from a missing process.

#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            process::ExitStatusExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ion_ai::ToolSpec;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat},
    io::{FdFlags, fcntl_setfd},
    pipe::pipe,
    process::{Pid, PidfdFlags, pidfd_open},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::command_workspace::{
    import::{ImportFailure, apply_import, persist_import_plan, plan_import, remove_import_plan},
    snapshot,
    workspace_view::{self, WorkspaceView},
};
use crate::{
    ApprovalState, EffectSummary, EgressRealm, LiveToolAuthority, OutputCapture, OutputLoss,
    PreparedAction, SemanticCompatibilityId, StartReceipt, StartReceiptCapability, ToolAttempt,
    ToolAttemptState, ToolAuthority, ToolBinding, ToolBindingId, ToolBoundary, ToolBoundaryError,
    ToolConcurrency, ToolExecution, ToolOutputStream, ToolProgressPublisher, ToolRecoveryPolicy,
    ToolResult, WorkspaceBinding,
    workspace_registry::{
        ClaimKey, RegistryError, RegistryReceipt, TerminalEvidence, WorkspaceRegistry,
        WorkspaceResources, WorkspaceRevision,
    },
};

const BWRAP: &str = "/usr/bin/bwrap";
const IMPLEMENTATION_ID: &str = "linux-bwrap-private-view-v2";
const MAX_COMMAND_BYTES: usize = 8192;
const MAX_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const AUTHORITY_ALLOW: u8 = 0;
const AUTHORITY_ASK: u8 = 1;
const AUTHORITY_DENY: u8 = 2;

/// Native Linux exec with a protected host registry and a scoped filesystem.
pub struct NativeExecBoundary {
    binding: ToolBinding,
    workspace: WorkspaceBinding,
    executor: SemanticCompatibilityId,
    registry_directory: PathBuf,
    staging_directory: PathBuf,
    resources: WorkspaceResources,
    live_authority: Arc<AtomicU8>,
    rust_toolchain: Option<ReadonlyRoot>,
    cargo_registry: Option<ReadonlyRoot>,
}

impl NativeExecBoundary {
    pub fn new(
        registry: &WorkspaceRegistry,
        workspace: WorkspaceBinding,
        staging_directory: &Path,
        rust_toolchain: Option<&Path>,
        cargo_registry: Option<&Path>,
    ) -> Result<Self, NativeExecError> {
        registry.verify_current(&workspace)?;
        let metadata = fs::metadata(BWRAP)?;
        if !metadata.is_file() {
            return Err(NativeExecError::Unavailable);
        }
        if cargo_registry.is_some() && rust_toolchain.is_none() {
            return Err(NativeExecError::InvalidToolchain);
        }
        let rust_toolchain =
            qualify_readonly_root(rust_toolchain, &workspace, registry.directory())?;
        let cargo_registry =
            qualify_readonly_root(cargo_registry, &workspace, registry.directory())?;
        if let (Some(toolchain), Some(cache)) = (&rust_toolchain, &cargo_registry)
            && (toolchain.path.starts_with(&cache.path) || cache.path.starts_with(&toolchain.path))
        {
            return Err(NativeExecError::InvalidToolchain);
        }
        let identity = format!(
            "{}:{:x}",
            IMPLEMENTATION_ID,
            Sha256::digest(
                format!(
                    "{}:{rust_toolchain:?}:{cargo_registry:?}",
                    workspace.backend
                )
                .as_bytes()
            )
        );
        let executor = SemanticCompatibilityId::new(workspace.backend.clone())
            .map_err(|_| NativeExecError::InvalidWorkspace)?;
        let binding = native_exec_binding(&identity).map_err(NativeExecError::InvalidBinding)?;
        let resources = registry.mutation_resources(&workspace)?;
        let stage = staging_directory
            .canonicalize()
            .map_err(NativeExecError::Io)?;
        let root = Path::new(&workspace.canonical_root);
        let stage_meta = fs::metadata(&stage)?;
        let root_meta = fs::metadata(root)?;
        if stage != staging_directory
            || stage == registry.directory()
            || !stage.starts_with(registry.directory())
            || stage.starts_with(root)
            || root.starts_with(&stage)
            || !stage_meta.is_dir()
            || stage_meta.permissions().mode() & 0o077 != 0
            || stage_meta.dev() != root_meta.dev()
        {
            return Err(NativeExecError::InvalidWorkspace);
        }
        Ok(Self {
            binding,
            workspace,
            executor,
            registry_directory: registry.directory().to_path_buf(),
            staging_directory: stage,
            resources,
            live_authority: Arc::new(AtomicU8::new(AUTHORITY_DENY)),
            rust_toolchain,
            cargo_registry,
        })
    }

    pub fn set_live_authority(&self, authority: LiveToolAuthority) {
        self.live_authority.store(
            match authority {
                LiveToolAuthority::Allow => AUTHORITY_ALLOW,
                LiveToolAuthority::Ask => AUTHORITY_ASK,
                LiveToolAuthority::Deny => AUTHORITY_DENY,
            },
            Ordering::SeqCst,
        );
    }

    #[must_use]
    pub fn tool_binding(&self) -> &ToolBinding {
        &self.binding
    }

    fn prepared_arguments(&self, action: &PreparedAction) -> Option<ExecAction> {
        if action.binding != self.binding.id
            || action.egress != EgressRealm::Local
            || action.authority != ToolAuthority::WorkspaceMutation
            || !action.base_facts.is_empty()
        {
            return None;
        }
        let arguments: ExecAction = serde_json::from_value(action.arguments.clone()).ok()?;
        if !valid_command(&arguments.command)
            || !(1..=MAX_TIMEOUT_MS).contains(&arguments.timeout_ms)
            || action.workspace_revision != Some(arguments.base_revision.files)
        {
            return None;
        }
        let expected = PreparedAction::new(
            self.binding.id.clone(),
            serde_json::to_value(&arguments).ok()?,
            EgressRealm::Local,
            ToolAuthority::WorkspaceMutation,
            Some(arguments.base_revision.files),
            Vec::new(),
        )
        .ok()?;
        (expected == *action).then_some(arguments)
    }
}

impl ToolBoundary for NativeExecBoundary {
    fn binding(&self) -> ToolBinding {
        self.binding.clone()
    }

    fn executor(&self) -> SemanticCompatibilityId {
        self.executor.clone()
    }

    fn prepare(&self, arguments: Value) -> Result<PreparedAction, ToolBoundaryError> {
        let input: ExecInput =
            serde_json::from_value(arguments).map_err(|_| ToolBoundaryError::InvalidArguments)?;
        if !valid_command(&input.command)
            || !input
                .timeout_ms
                .is_none_or(|timeout| (1..=MAX_TIMEOUT_MS).contains(&timeout))
        {
            return Err(ToolBoundaryError::InvalidArguments);
        }
        let registry = WorkspaceRegistry::open(&self.registry_directory)
            .map_err(|_| ToolBoundaryError::Unavailable)?;
        registry
            .verify_current(&self.workspace)
            .map_err(|_| ToolBoundaryError::Unavailable)?;
        let revision = registry
            .revision(&self.workspace)
            .map_err(|_| ToolBoundaryError::Unavailable)?;
        let action = ExecAction {
            command: input.command,
            timeout_ms: input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            base_revision: revision,
        };
        PreparedAction::new(
            self.binding.id.clone(),
            serde_json::to_value(action).map_err(|_| ToolBoundaryError::InvalidAction)?,
            EgressRealm::Local,
            ToolAuthority::WorkspaceMutation,
            Some(revision.files),
            Vec::new(),
        )
        .map_err(|_| ToolBoundaryError::InvalidAction)
    }

    fn live_authority(
        &self,
        action: &PreparedAction,
        workspace: &WorkspaceBinding,
    ) -> LiveToolAuthority {
        if workspace != &self.workspace || self.prepared_arguments(action).is_none() {
            return LiveToolAuthority::Deny;
        }
        match self.live_authority.load(Ordering::SeqCst) {
            AUTHORITY_ALLOW => LiveToolAuthority::Allow,
            AUTHORITY_ASK => LiveToolAuthority::Ask,
            _ => LiveToolAuthority::Deny,
        }
    }

    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        stop: CancellationToken,
    ) -> ion_ai::BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            let Some(arguments) = self.prepared_arguments(&execution.action) else {
                return not_started("invalid or incompatible prepared exec action");
            };
            if execution.binding != self.binding
                || execution.workspace != self.workspace
                || !execution.action.permitted_by(&execution.ceiling)
            {
                return not_started("exec binding, workspace, or authority changed");
            }
            if stop.is_cancelled() {
                return not_started("exec cancelled before admission");
            }
            let job = ExecJob {
                execution,
                arguments,
                registry_directory: self.registry_directory.clone(),
                staging_directory: self.staging_directory.clone(),
                resources: self.resources,
                binding: self.binding.clone(),
                executor: self.executor.clone(),
                live_authority: Arc::clone(&self.live_authority),
                stop: stop.clone(),
                rust_toolchain: self.rust_toolchain.clone(),
                cargo_registry: self.cargo_registry.clone(),
            };
            let cancellation = CancelWorkerOnDrop::new(stop);
            let result = tokio::task::spawn_blocking(move || run_exec(job)).await;
            cancellation.disarm();
            result.unwrap_or_else(|_| indeterminate("exec supervisor failed", None))
        })
    }

    fn reconcile<'a>(
        &'a self,
        execution: ToolExecution,
        attempt: ToolAttempt,
    ) -> ion_ai::BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move { self.reconcile_terminal_claim(&execution, attempt) })
    }
}

impl NativeExecBoundary {
    fn reconcile_terminal_claim(
        &self,
        execution: &ToolExecution,
        attempt: ToolAttempt,
    ) -> ToolAttemptState {
        let previous_receipt = match &attempt.state {
            ToolAttemptState::IntentCommitted { start_receipt }
            | ToolAttemptState::Indeterminate {
                receipt: start_receipt,
                ..
            } => start_receipt.clone(),
            _ => return attempt.state,
        };
        let unresolved = |reason: &str| ToolAttemptState::Indeterminate {
            reason: reason.into(),
            receipt: previous_receipt.clone(),
        };
        if execution.binding != self.binding
            || execution.workspace != self.workspace
            || attempt.id != execution.attempt
            || attempt.invocation != execution.invocation
            || attempt.executor != self.executor
            || self.prepared_arguments(&execution.action).is_none()
        {
            return unresolved("exec recovery identity changed");
        }
        let key = ClaimKey {
            session: execution.session,
            invocation: execution.invocation,
            attempt: execution.attempt,
        };
        let registry = match WorkspaceRegistry::open(&self.registry_directory) {
            Ok(registry) => registry,
            Err(_) => return unresolved("exec registry unavailable for recovery"),
        };
        let claim = match registry.claim(key) {
            Ok(claim) => claim,
            Err(_) => return unresolved("exec claim unavailable for recovery"),
        };
        if claim.binding != execution.workspace || claim.resources != self.resources {
            return unresolved("exec claim does not match the frozen workspace");
        }
        let base_identity = format!("{}-{}-{}", key.session, key.invocation, key.attempt);
        let recovered_start = claim
            .start
            .as_ref()
            .and_then(|start| recovered_exec_receipt(&base_identity, start));
        let Some(terminal) = claim.terminal else {
            return match (claim.start.as_ref(), recovered_start) {
                (Some(start), Some(receipt)) if start.backend == execution.workspace.backend => {
                    match compatible_exec_receipt(&previous_receipt, receipt, start) {
                        Some(receipt) => ToolAttemptState::Indeterminate {
                            reason: "exec claim has no terminal evidence".into(),
                            receipt: Some(receipt),
                        },
                        None => unresolved("exec start receipt differs from Session evidence"),
                    }
                }
                _ => unresolved("exec claim has no terminal evidence"),
            };
        };
        if terminal.receipt.backend != execution.workspace.backend {
            return unresolved("exec terminal receipt backend changed");
        }
        let Some(start) = claim.start else {
            return if previous_receipt.is_none()
                && terminal.receipt.identity == base_identity
                && terminal.effect == EffectSummary::NoMutation
            {
                ToolAttemptState::NotStarted {
                    reason: "exec host claim proves no command start".into(),
                }
            } else {
                unresolved("exec terminal evidence conflicts with missing start")
            };
        };
        if terminal.receipt != start {
            return unresolved("exec terminal receipt differs from start");
        }
        let Some(receipt) = recovered_start else {
            return unresolved("exec start receipt cannot be reconstructed");
        };
        let receipt = match compatible_exec_receipt(&previous_receipt, receipt, &start) {
            Some(receipt) => receipt,
            None => return unresolved("exec start receipt differs from Session evidence"),
        };
        ToolAttemptState::Settled {
            result: ToolResult {
                value: json!(
                    "Exec stopped; output and exit status lost on recovery. Inspect workspace."
                ),
                is_error: true,
                capture: OutputCapture::Incomplete {
                    reason: OutputLoss::LostOnRecovery,
                    retained_bytes: 0,
                    observed_bytes: None,
                },
            },
            effect: terminal.effect,
            receipt: Some(receipt),
            retryable: false,
        }
    }
}

fn partial_exec_receipt(receipt: &RegistryReceipt) -> StartReceipt {
    StartReceipt {
        kind: IMPLEMENTATION_ID.into(),
        data: json!({"registry": receipt}),
    }
}

fn compatible_exec_receipt(
    previous: &Option<StartReceipt>,
    recovered: StartReceipt,
    start: &RegistryReceipt,
) -> Option<StartReceipt> {
    match previous {
        Some(old) if old == &recovered || old == &partial_exec_receipt(start) => Some(old.clone()),
        Some(_) => None,
        None => Some(recovered),
    }
}

fn recovered_exec_receipt(base_identity: &str, receipt: &RegistryReceipt) -> Option<StartReceipt> {
    let suffix = receipt
        .identity
        .strip_prefix(base_identity)?
        .strip_prefix(':')?;
    let mut parts = suffix.split(':');
    let bwrap_pid = parts.next()?.parse::<u32>().ok()?;
    let init_pid = parts.next()?.parse::<i32>().ok()?;
    let pid_namespace = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() || bwrap_pid == 0 || init_pid <= 0 || pid_namespace == 0 {
        return None;
    }
    Some(StartReceipt {
        kind: IMPLEMENTATION_ID.into(),
        data: json!({"registry": receipt, "init_pid": init_pid,
            "pid_namespace": pid_namespace}),
    })
}

fn native_exec_binding(implementation_id: &str) -> Result<ToolBinding, crate::ConfigError> {
    let mut binding = ToolBinding::new(
        ToolBindingId::new("exec")?,
        ToolSpec {
            name: "exec".into(),
            description: "Run a native Linux shell command in a private copy of the workspace. Network access is disabled. Ordinary file changes are imported after the command and its descendants stop; inspect imported_paths and import_error to see what reached the workspace. Git metadata and ignored paths are not imported.".into(),
            input_schema: json!({
                "type": "object", "additionalProperties": false,
                "required": ["command"],
                "properties": {
                    "command": {"type": "string", "minLength": 1, "maxLength": MAX_COMMAND_BYTES},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS}
                }
            }),
        },
        SemanticCompatibilityId::new(implementation_id)?,
        ToolConcurrency::Serial,
        ToolRecoveryPolicy::NeverRepeat,
        EgressRealm::Local,
    )?;
    binding.start_receipts = StartReceiptCapability::Authoritative;
    Ok(binding)
}

#[derive(Debug, Error)]
pub enum NativeExecError {
    #[error("Linux Bubblewrap backend is unavailable")]
    Unavailable,
    #[error("invalid exec workspace binding")]
    InvalidWorkspace,
    #[error("unsafe or incompatible read-only command toolchain root")]
    InvalidToolchain,
    #[error("failed to construct exec binding: {0}")]
    InvalidBinding(#[source] crate::ConfigError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Syscall(#[from] rustix::io::Errno),
}

#[derive(Debug, Clone)]
struct ReadonlyRoot {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl ReadonlyRoot {
    fn still_qualified(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|meta| {
            meta.dev() == self.device && meta.ino() == self.inode && meta.is_dir()
        }) && validate_readonly_tree(&self.path).is_ok()
    }
}

fn qualify_readonly_root(
    candidate: Option<&Path>,
    workspace: &WorkspaceBinding,
    registry: &Path,
) -> Result<Option<ReadonlyRoot>, NativeExecError> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    let path = candidate.canonicalize()?;
    let workspace = Path::new(&workspace.canonical_root);
    if path == Path::new("/")
        || path.starts_with(workspace)
        || workspace.starts_with(&path)
        || path.starts_with(registry)
        || registry.starts_with(&path)
    {
        return Err(NativeExecError::InvalidToolchain);
    }
    let meta = fs::metadata(&path)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o7000 != 0 {
        return Err(NativeExecError::InvalidToolchain);
    }
    validate_readonly_tree(&path)?;
    Ok(Some(ReadonlyRoot {
        path,
        device: meta.dev(),
        inode: meta.ino(),
    }))
}

/// Refuse broker endpoints and nested mounts in a host-selected read-only tree.
/// The caller must also protect this host namespace against concurrent same-user
/// replacement, just as it does for the live workspace and private registry.
fn validate_readonly_tree(path: &Path) -> Result<(), NativeExecError> {
    let root = File::open(path)?;
    let device = root.metadata()?.dev();
    let mut remaining = 500_000_usize;
    fn walk(
        directory: &File,
        device: u64,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<(), NativeExecError> {
        if depth > 64 {
            return Err(NativeExecError::InvalidToolchain);
        }
        for entry in Dir::read_from(directory)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or(NativeExecError::InvalidToolchain)?;
            let name = std::str::from_utf8(name).map_err(|_| NativeExecError::InvalidToolchain)?;
            let before = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if before.st_dev != device {
                return Err(NativeExecError::InvalidToolchain);
            }
            match FileType::from_raw_mode(before.st_mode) {
                FileType::RegularFile | FileType::Symlink => {}
                FileType::Directory => {
                    let child = File::from(openat(
                        directory,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?);
                    let meta = child.metadata()?;
                    if meta.dev() != device || meta.ino() != before.st_ino {
                        return Err(NativeExecError::InvalidToolchain);
                    }
                    walk(&child, device, depth + 1, remaining)?;
                }
                _ => return Err(NativeExecError::InvalidToolchain),
            }
        }
        Ok(())
    }
    walk(&root, device, 0, &mut remaining)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    command: String,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecAction {
    command: String,
    timeout_ms: u64,
    base_revision: WorkspaceRevision,
}

fn valid_command(command: &str) -> bool {
    !command.is_empty() && command.len() <= MAX_COMMAND_BYTES && !command.contains('\0')
}

struct ExecJob {
    execution: ToolExecution,
    arguments: ExecAction,
    registry_directory: PathBuf,
    staging_directory: PathBuf,
    resources: WorkspaceResources,
    binding: ToolBinding,
    executor: SemanticCompatibilityId,
    live_authority: Arc<AtomicU8>,
    stop: CancellationToken,
    rust_toolchain: Option<ReadonlyRoot>,
    cargo_registry: Option<ReadonlyRoot>,
}

fn run_exec(job: ExecJob) -> ToolAttemptState {
    let mut registry = match WorkspaceRegistry::open(&job.registry_directory) {
        Ok(registry) => registry,
        Err(_) => return indeterminate("workspace registry is unavailable", None),
    };
    let key = ClaimKey {
        session: job.execution.session,
        invocation: job.execution.invocation,
        attempt: job.execution.attempt,
    };
    match registry.claim(key) {
        Ok(claim) => {
            return indeterminate(
                "existing exec attempt is never run again",
                claim.start.as_ref(),
            );
        }
        Err(RegistryError::EvidenceConflict) => {}
        Err(_) => return indeterminate("prior exec admission is unreadable", None),
    }
    if job.stop.is_cancelled() || !admission_allowed(&job) {
        return not_started("exec cancelled or live authority denied before admission");
    }
    if registry.verify_current(&job.execution.workspace).is_err()
        || registry.revision(&job.execution.workspace).ok() != Some(job.arguments.base_revision)
        || registry.mutation_resources(&job.execution.workspace).ok() != Some(job.resources)
    {
        return not_started("exec workspace binding or revision changed");
    }
    let output_limit = job.execution.output_limit.min(crate::MAX_TOOL_RECORD_BYTES);
    let Some(output_cap) = output_budget(output_limit) else {
        return not_started("exec result limit cannot fit its bounded result envelope");
    };
    let attempt_identity = format!("{}-{}-{}", key.session, key.invocation, key.attempt);
    let mut receipt = RegistryReceipt {
        backend: job.execution.workspace.backend.clone(),
        identity: attempt_identity.clone(),
    };
    if registry
        .admit(
            &job.execution.workspace,
            key,
            job.resources,
            job.arguments.base_revision,
        )
        .is_err()
    {
        return match registry.claim(key) {
            Ok(_) => indeterminate("exec admission acknowledgement is uncertain", None),
            Err(RegistryError::EvidenceConflict) => not_started("exec claim was not admitted"),
            Err(_) => indeterminate("exec claim is unreadable", None),
        };
    }
    if job.stop.is_cancelled() || !admission_allowed(&job) {
        return resolve_no_start(&mut registry, key, &receipt, "exec cancelled before launch");
    }
    let live_root = match File::open(&job.execution.workspace.canonical_root) {
        Ok(root) => root,
        Err(_) => {
            return resolve_no_start(&mut registry, key, &receipt, "workspace root unavailable");
        }
    };
    let stage_root = match File::open(&job.staging_directory) {
        Ok(stage) => stage,
        Err(_) => {
            return resolve_no_start(&mut registry, key, &receipt, "private stage unavailable");
        }
    };
    if registry.verify_current(&job.execution.workspace).is_err() {
        return resolve_no_start(
            &mut registry,
            key,
            &receipt,
            "workspace root changed before snapshot",
        );
    }
    let view = match WorkspaceView::create(
        &live_root,
        Path::new(&job.execution.workspace.canonical_root),
        &stage_root,
        &job.staging_directory,
        &attempt_identity,
    ) {
        Ok(view) => view,
        Err(workspace_view::ViewError::Snapshot(snapshot::SnapshotError::Cleanup { .. })) => {
            return indeterminate(
                "private workspace snapshot cleanup is uncertain",
                Some(&receipt),
            );
        }
        Err(_) => {
            return resolve_no_start(
                &mut registry,
                key,
                &receipt,
                "private workspace snapshot failed",
            );
        }
    };
    if job
        .rust_toolchain
        .as_ref()
        .is_some_and(|root| !root.still_qualified())
        || job
            .cargo_registry
            .as_ref()
            .is_some_and(|root| !root.still_qualified())
    {
        return cleanup_no_start(
            &view,
            &mut registry,
            key,
            &receipt,
            "selected read-only command tools changed or became unsafe",
        );
    }
    let (status_read, status_write) = match pipe() {
        Ok(pipe) => pipe,
        Err(_) => {
            return cleanup_no_start(
                &view,
                &mut registry,
                key,
                &receipt,
                "status pipe unavailable",
            );
        }
    };
    if fcntl_setfd(&status_write, FdFlags::empty()).is_err() {
        return cleanup_no_start(
            &view,
            &mut registry,
            key,
            &receipt,
            "status pipe cannot be inherited",
        );
    }
    let mut command = sandbox_command(&job, view.command_path(), status_write.as_raw_fd());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return cleanup_no_start(
                &view,
                &mut registry,
                key,
                &receipt,
                "Bubblewrap could not launch",
            );
        }
    };
    // The descriptor only needs to survive this spawn. Close the inheritance
    // window before waiting for scope setup or launching another host process.
    let _ = fcntl_setfd(&status_write, FdFlags::CLOEXEC);
    drop(status_write);
    let (status_tx, status_rx) = mpsc::channel();
    let status_thread = thread::spawn(move || read_status(File::from(status_read), status_tx));
    let first = status_rx.recv_timeout(Duration::from_secs(5));
    let Some(scope_start) = first.ok().and_then(|value| value) else {
        let joined = kill_and_wait(&mut child);
        let _ = status_thread.join();
        return if joined {
            cleanup_no_start(
                &view,
                &mut registry,
                key,
                &receipt,
                "Bubblewrap scope setup failed",
            )
        } else {
            indeterminate("Bubblewrap setup could not be joined", None)
        };
    };
    let Some(pid) = Pid::from_raw(scope_start.init_pid) else {
        let _ = kill_and_wait(&mut child);
        let _ = status_thread.join();
        return indeterminate("invalid PID namespace init receipt", None);
    };
    let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(pidfd) => pidfd,
        Err(_) => {
            let _ = kill_and_wait(&mut child);
            let _ = status_thread.join();
            return indeterminate("PID namespace init cannot be observed", None);
        }
    };
    receipt.identity = format!(
        "{}:{}:{}:{}",
        receipt.identity,
        child.id(),
        scope_start.init_pid,
        scope_start.pid_namespace,
    );
    if registry.record_start(key, receipt.clone()).is_err()
        && !registry
            .claim(key)
            .is_ok_and(|claim| claim.start.as_ref() == Some(&receipt))
    {
        let joined = kill_and_wait(&mut child);
        let _ = status_thread.join();
        return if joined && pidfd_terminal(&pidfd) {
            cleanup_no_start(
                &view,
                &mut registry,
                key,
                &receipt,
                "start receipt not durable",
            )
        } else {
            indeterminate("start receipt uncertain", Some(&receipt))
        };
    }
    let start_receipt = StartReceipt {
        kind: IMPLEMENTATION_ID.into(),
        data: json!({"registry": receipt, "init_pid": scope_start.init_pid,
            "pid_namespace": scope_start.pid_namespace}),
    };
    if job.stop.is_cancelled() || !admission_allowed(&job) {
        let joined = kill_and_wait(&mut child);
        let _ = status_thread.join();
        return if joined && pidfd_terminal(&pidfd) {
            cleanup_no_start(
                &view,
                &mut registry,
                key,
                &receipt,
                "exec cancelled before start gate",
            )
        } else {
            indeterminate(
                "stopped before start gate but scope is not proven terminal",
                Some(&receipt),
            )
        };
    }
    let gate_released = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(b"x").is_ok());
    if !gate_released {
        let joined = kill_and_wait(&mut child);
        let _ = status_thread.join();
        return if joined && pidfd_terminal(&pidfd) {
            cleanup_no_start(
                &view,
                &mut registry,
                key,
                &receipt,
                "exec start gate did not open",
            )
        } else {
            indeterminate(
                "start gate failed and scope is not terminal",
                Some(&receipt),
            )
        };
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_progress = job.execution.progress.clone();
    let stderr_progress = job.execution.progress.clone();
    let stdout_thread = thread::spawn(move || {
        collect(
            stdout,
            output_cap / 2,
            ToolOutputStream::Stdout,
            &stdout_progress,
        )
    });
    let stderr_thread = thread::spawn(move || {
        collect(
            stderr,
            output_cap / 2,
            ToolOutputStream::Stderr,
            &stderr_progress,
        )
    });
    let deadline = Instant::now() + Duration::from_millis(job.arguments.timeout_ms);
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Err(_) => break None,
            Ok(None) => {}
        }
        if job.stop.is_cancelled() || Instant::now() >= deadline {
            cancelled = job.stop.is_cancelled();
            timed_out = !cancelled;
            break kill_and_reap(&mut child);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_thread.join().ok();
    let stderr = stderr_thread.join().ok();
    let status_data = status_thread.join().ok().flatten();
    if status.is_none() || !pidfd_terminal(&pidfd) {
        return indeterminate(
            "exec scope has no positive terminal observation",
            Some(&receipt),
        );
    }
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return discard_terminal_private_view(
            &view,
            &mut registry,
            key,
            &receipt,
            start_receipt,
            "command output is unavailable",
            output_limit,
        );
    };
    let Some(scope) = status_data else {
        return discard_terminal_private_view(
            &view,
            &mut registry,
            key,
            &receipt,
            start_receipt,
            "command status is unavailable",
            output_limit,
        );
    };
    if scope.start != scope_start {
        return indeterminate("Bubblewrap status identity changed", Some(&receipt));
    }
    if !cancelled && !timed_out && scope.exit_code.is_none() {
        return discard_terminal_private_view(
            &view,
            &mut registry,
            key,
            &receipt,
            start_receipt,
            "command exit status is unavailable",
            output_limit,
        );
    }
    let observed_output = stdout.observed.saturating_add(stderr.observed);
    let mut result = command_result(
        status.expect("checked"),
        stdout,
        stderr,
        cancelled,
        timed_out,
    );
    let plan_name = format!("exec-{attempt_identity}-plan.json");
    let mut plan_persisted = false;
    let (imported, mut import_error, output_omissions, output_bytes, git_metadata_changed) =
        match view.capture_output() {
            Err(error) => (
                Vec::new(),
                Some(format!("private output could not be captured: {error}")),
                0,
                0,
                false,
            ),
            Ok(output) => {
                let omissions = output.omitted.len();
                let bytes = output.total_bytes;
                match plan_import(&view.source.manifest, &output.manifest) {
                    Err(error) => (
                        Vec::new(),
                        Some(format!("private changes were not imported: {error}")),
                        omissions,
                        bytes,
                        false,
                    ),
                    Ok(plan) => {
                        let git_metadata_changed = plan.git_metadata_changed;
                        if !plan.changes.is_empty() {
                            if let Err(error) = persist_import_plan(&stage_root, &plan_name, &plan)
                            {
                                return indeterminate(
                                    &format!("durable import plan unavailable: {error}"),
                                    Some(&receipt),
                                );
                            }
                            plan_persisted = true;
                        }
                        let (applied, error) = match apply_import(
                            &plan,
                            &view.source.manifest,
                            &live_root,
                            &output.root,
                            &stage_root,
                        ) {
                            Ok(applied) => (applied, None),
                            Err(ImportFailure::Preflight(reason)) => (
                                Vec::new(),
                                Some(format!("private changes were not imported: {reason}")),
                            ),
                            Err(ImportFailure::Partial { applied, reason }) => (
                                applied,
                                Some(format!(
                                    "private changes were only partly imported: {reason}"
                                )),
                            ),
                            Err(ImportFailure::Unknown { applied, reason }) => {
                                return indeterminate(
                                    &format!(
                                        "command import uncertain after confirmed paths {applied:?}: {reason}"
                                    ),
                                    Some(&receipt),
                                );
                            }
                        };
                        (applied, error, omissions, bytes, git_metadata_changed)
                    }
                }
            }
        };
    if git_metadata_changed {
        let warning = "Git metadata changed only in the private command view; index, ref and commit changes did not reach the live workspace";
        import_error = Some(match import_error {
            Some(error) => format!("{error}; {warning}"),
            None => warning.to_owned(),
        });
    }
    if view.cleanup().is_err() {
        return indeterminate(
            "private command workspace cleanup is uncertain",
            Some(&receipt),
        );
    }
    if let Some(value) = result.value.as_object_mut() {
        value.insert("imported_paths".into(), json!(&imported));
        value.insert("source_omissions".into(), json!(view.source.omitted.len()));
        value.insert("output_omissions".into(), json!(output_omissions));
        value.insert("source_bytes".into(), json!(view.source.total_bytes));
        value.insert("output_bytes".into(), json!(output_bytes));
        value.insert("git_metadata_imported".into(), json!(false));
        value.insert("git_metadata_changed".into(), json!(git_metadata_changed));
        if let Some(error) = &import_error {
            value.insert("import_error".into(), json!(error));
            result.is_error = true;
        }
    }
    result = fit_result(result, output_limit, observed_output);
    let effect = if imported.is_empty() {
        EffectSummary::NoMutation
    } else {
        EffectSummary::KnownChanges { paths: imported }
    };
    if registry
        .resolve(
            key,
            TerminalEvidence {
                receipt: receipt.clone(),
                effect: effect.clone(),
            },
        )
        .is_err()
        && !registry.claim(key).is_ok_and(|claim| {
            claim
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.receipt == receipt && terminal.effect == effect)
        })
    {
        return indeterminate("exec terminal registry write is uncertain", Some(&receipt));
    }
    if plan_persisted && remove_import_plan(&stage_root, &plan_name).is_err() {
        result.is_error = true;
        if let Some(value) = result.value.as_object_mut() {
            value.insert(
                "cleanup_error".into(),
                json!("durable import plan cleanup failed; host maintenance is required"),
            );
        }
        result = fit_result(result, output_limit, observed_output);
    }
    ToolAttemptState::Settled {
        result,
        effect,
        receipt: Some(start_receipt),
        retryable: false,
    }
}

fn sandbox_command(job: &ExecJob, private_workspace: &Path, status_fd: i32) -> Command {
    let mut command = Command::new(BWRAP);
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--disable-userns",
        "--cap-drop",
        "ALL",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--ro-bind",
        "/usr",
        "/usr",
        "--dir",
        "/etc",
        "--ro-bind-try",
        "/etc/alternatives",
        "/etc/alternatives",
        "--ro-bind-try",
        "/bin",
        "/bin",
        "--ro-bind-try",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--ro-bind-try",
        "/usr/local",
        "/usr/local",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/run",
    ]);
    if let Some(toolchain) = &job.rust_toolchain {
        command
            .arg("--ro-bind")
            .arg(&toolchain.path)
            .arg("/toolchain");
        command.args(["--dir", "/tmp/cargo"]);
        if let Some(cache) = &job.cargo_registry {
            command
                .arg("--ro-bind")
                .arg(&cache.path)
                .arg("/tmp/cargo/registry");
        }
    }
    command.arg("--bind");
    command.arg(private_workspace);
    command.args(["/work", "--chdir", "/work", "--setenv", "HOME", "/tmp"]);
    if job.rust_toolchain.is_some() {
        command.args([
            "--setenv",
            "CARGO_HOME",
            "/tmp/cargo",
            "--setenv",
            "CARGO_NET_OFFLINE",
            "true",
        ]);
    }
    command.args([
        "--setenv",
        "PATH",
        if job.rust_toolchain.is_some() {
            "/toolchain/bin:/usr/local/bin:/usr/bin:/bin"
        } else {
            "/usr/local/bin:/usr/bin:/bin"
        },
        "--json-status-fd",
    ]);
    command.arg(status_fd.to_string());
    command.args([
        "--block-fd",
        "0",
        "--",
        "/bin/sh",
        "-c",
        &job.arguments.command,
    ]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScopeStart {
    init_pid: i32,
    pid_namespace: u64,
}

struct ScopeStatus {
    start: ScopeStart,
    exit_code: Option<i32>,
}

fn read_status(file: File, first: mpsc::Sender<Option<ScopeStart>>) -> Option<ScopeStatus> {
    let mut lines = BufReader::new(file).lines();
    let start = lines.next()?.ok()?;
    let value: Value = serde_json::from_str(&start).ok()?;
    let init_pid = value
        .get("child-pid")?
        .as_i64()
        .and_then(|pid| i32::try_from(pid).ok())?;
    let pid_namespace = value.get("pid-namespace")?.as_u64()?;
    let start = ScopeStart {
        init_pid,
        pid_namespace,
    };
    let _ = first.send(Some(start));
    let exit_code = lines
        .next()
        .and_then(Result::ok)
        .and_then(|end| serde_json::from_str::<Value>(&end).ok())
        .and_then(|value| value.get("exit-code")?.as_i64())
        .and_then(|code| i32::try_from(code).ok());
    Some(ScopeStatus { start, exit_code })
}

struct Captured {
    bytes: Vec<u8>,
    observed: u64,
    truncated: bool,
}

fn collect(
    mut stream: impl Read,
    capacity: usize,
    output_stream: ToolOutputStream,
    progress: &ToolProgressPublisher,
) -> Captured {
    let mut result = Captured {
        bytes: Vec::new(),
        observed: 0,
        truncated: false,
    };
    let mut chunk = [0_u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                progress.output(output_stream, &chunk[..count]);
                result.observed = result.observed.saturating_add(count as u64);
                let keep = capacity.saturating_sub(result.bytes.len()).min(count);
                result.bytes.extend_from_slice(&chunk[..keep]);
                result.truncated |= keep < count;
            }
            Err(_) => {
                result.truncated = true;
                break;
            }
        }
    }
    result
}

fn command_result(
    status: std::process::ExitStatus,
    stdout: Captured,
    stderr: Captured,
    cancelled: bool,
    timed_out: bool,
) -> ToolResult {
    let retained = stdout.bytes.len().saturating_add(stderr.bytes.len()) as u64;
    let observed = stdout.observed.saturating_add(stderr.observed);
    let stdout_text = String::from_utf8_lossy(&stdout.bytes);
    let stderr_text = String::from_utf8_lossy(&stderr.bytes);
    let lost = stdout.truncated
        || stderr.truncated
        || matches!(stdout_text, std::borrow::Cow::Owned(_))
        || matches!(stderr_text, std::borrow::Cow::Owned(_));
    ToolResult {
        value: json!({
            "exit_code": status.code(), "signal": status.signal(),
            "stdout": stdout_text, "stderr": stderr_text,
            "cancelled": cancelled, "timed_out": timed_out,
        }),
        is_error: !status.success() || cancelled || timed_out,
        capture: if lost {
            OutputCapture::Incomplete {
                reason: OutputLoss::Quota,
                retained_bytes: retained,
                observed_bytes: Some(observed),
            }
        } else {
            OutputCapture::CompleteInline
        },
    }
}

fn output_budget(output_limit: usize) -> Option<usize> {
    let envelope = ToolResult {
        value: json!({
            "exit_code": i32::MAX, "signal": i32::MAX,
            "stdout": "", "stderr": "",
            "cancelled": true, "timed_out": true,
        }),
        is_error: true,
        capture: OutputCapture::Incomplete {
            reason: OutputLoss::Quota,
            retained_bytes: u64::MAX,
            observed_bytes: Some(u64::MAX),
        },
    };
    let fixed = serde_json::to_vec(&envelope).ok()?.len();
    output_limit.checked_sub(fixed)
}

fn fit_result(mut result: ToolResult, limit: usize, observed: u64) -> ToolResult {
    for _ in 0..32 {
        if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= limit) {
            return result;
        }
        let Some(value) = result.value.as_object_mut() else {
            break;
        };
        let stdout_len = value
            .get("stdout")
            .and_then(Value::as_str)
            .map_or(0, str::len);
        let stderr_len = value
            .get("stderr")
            .and_then(Value::as_str)
            .map_or(0, str::len);
        let field = if stdout_len >= stderr_len {
            "stdout"
        } else {
            "stderr"
        };
        let Some(text) = value
            .get_mut(field)
            .and_then(|entry| entry.as_str())
            .map(str::to_owned)
        else {
            break;
        };
        if text.is_empty() {
            break;
        }
        let mut end = text.len() / 2;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        value.insert(field.into(), Value::String(text[..end].to_owned()));
        let retained = value
            .get("stdout")
            .and_then(Value::as_str)
            .map_or(0, str::len)
            + value
                .get("stderr")
                .and_then(Value::as_str)
                .map_or(0, str::len);
        result.capture = OutputCapture::Incomplete {
            reason: OutputLoss::Quota,
            retained_bytes: retained as u64,
            observed_bytes: Some(observed),
        };
    }
    crate::tool_exec::output_capacity_result()
}

fn admission_allowed(job: &ExecJob) -> bool {
    let approved = job.execution.approval.permits(
        &job.execution.action,
        &job.binding.implementation,
        &job.executor,
        &job.execution.workspace,
        now_unix_ms(),
    );
    match job.live_authority.load(Ordering::SeqCst) {
        AUTHORITY_ALLOW => {
            !matches!(
                job.execution.approval,
                ApprovalState::Pending | ApprovalState::Denied { .. }
            ) && (!matches!(job.execution.approval, ApprovalState::Approved { .. }) || approved)
        }
        AUTHORITY_ASK => approved,
        _ => false,
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn pidfd_terminal(pidfd: &impl std::os::fd::AsFd) -> bool {
    let mut pollfd = [PollFd::new(pidfd, PollFlags::IN)];
    poll(
        &mut pollfd,
        Some(&Timespec {
            tv_sec: 5,
            tv_nsec: 0,
        }),
    )
    .is_ok_and(|ready| ready == 1 && pollfd[0].revents().contains(PollFlags::IN))
}

fn kill_and_wait(child: &mut Child) -> bool {
    let _ = child.kill();
    child.wait().is_ok()
}

fn kill_and_reap(child: &mut Child) -> Option<std::process::ExitStatus> {
    child.kill().ok()?;
    child.wait().ok()
}

fn cleanup_no_start(
    view: &WorkspaceView,
    registry: &mut WorkspaceRegistry,
    key: ClaimKey,
    receipt: &RegistryReceipt,
    reason: &str,
) -> ToolAttemptState {
    if view.cleanup().is_err() {
        return indeterminate(
            "private prestart workspace cleanup is uncertain",
            Some(receipt),
        );
    }
    resolve_no_start(registry, key, receipt, reason)
}

fn resolve_no_start(
    registry: &mut WorkspaceRegistry,
    key: ClaimKey,
    receipt: &RegistryReceipt,
    reason: &str,
) -> ToolAttemptState {
    let evidence = TerminalEvidence {
        receipt: receipt.clone(),
        effect: EffectSummary::NoMutation,
    };
    if registry.resolve(key, evidence.clone()).is_err()
        && !registry
            .claim(key)
            .is_ok_and(|claim| claim.terminal.as_ref() == Some(&evidence))
    {
        return indeterminate("prestart registry resolution is uncertain", Some(receipt));
    }
    not_started(reason)
}

/// A positively stopped command has affected only its private view until the
/// first import. Lost command evidence can therefore discard that view and
/// release the claim, but cannot claim that the command succeeded or retry it.
fn discard_terminal_private_view(
    view: &WorkspaceView,
    registry: &mut WorkspaceRegistry,
    key: ClaimKey,
    receipt: &RegistryReceipt,
    start_receipt: StartReceipt,
    reason: &str,
    output_limit: usize,
) -> ToolAttemptState {
    if view.cleanup().is_err() {
        return indeterminate(
            "private command workspace cleanup is uncertain",
            Some(receipt),
        );
    }
    let evidence = TerminalEvidence {
        receipt: receipt.clone(),
        effect: EffectSummary::NoMutation,
    };
    if registry.resolve(key, evidence.clone()).is_err()
        && !registry
            .claim(key)
            .is_ok_and(|claim| claim.terminal.as_ref() == Some(&evidence))
    {
        return indeterminate("exec terminal registry write is uncertain", Some(receipt));
    }
    let mut result = ToolResult {
        value: json!({"error": reason, "private_changes_imported": false}),
        is_error: true,
        capture: OutputCapture::Incomplete {
            reason: OutputLoss::BackendFailure,
            retained_bytes: 0,
            observed_bytes: None,
        },
    };
    if serde_json::to_vec(&result).map_or(true, |encoded| encoded.len() > output_limit) {
        result = crate::tool_exec::output_capacity_result();
    }
    ToolAttemptState::Settled {
        result,
        effect: EffectSummary::NoMutation,
        receipt: Some(start_receipt),
        retryable: false,
    }
}

fn not_started(reason: &str) -> ToolAttemptState {
    ToolAttemptState::NotStarted {
        reason: reason.into(),
    }
}

fn indeterminate(reason: &str, receipt: Option<&RegistryReceipt>) -> ToolAttemptState {
    ToolAttemptState::Indeterminate {
        reason: reason.into(),
        receipt: receipt.map(partial_exec_receipt),
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

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::DirBuilderExt,
        os::unix::net::UnixListener,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::{AttemptId, AuthorityCeiling, InvocationId, ProgressUpdate, SessionId, TurnId};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("ion-native-exec-{}-{ordinal}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        work: Temp,
        _host: Temp,
        registry: WorkspaceRegistry,
        boundary: Arc<NativeExecBoundary>,
    }

    impl Fixture {
        fn new() -> Option<Self> {
            Self::new_with_roots(None, None)
        }

        fn new_with_roots(toolchain: Option<&Path>, cache: Option<&Path>) -> Option<Self> {
            if !Path::new(BWRAP).is_file() {
                return None;
            }
            let work = Temp::new();
            let host = Temp::new();
            let mut registry = WorkspaceRegistry::open(host.path()).unwrap();
            let stage = host.path().join("staging");
            fs::DirBuilder::new().mode(0o700).create(&stage).unwrap();
            let workspace = registry
                .bind("test-workspace", work.path(), "local-v1")
                .unwrap();
            let boundary =
                match NativeExecBoundary::new(&registry, workspace, &stage, toolchain, cache) {
                    Ok(boundary) => Arc::new(boundary),
                    Err(NativeExecError::Unavailable) => return None,
                    Err(error) => panic!("exec setup failed: {error}"),
                };
            boundary.set_live_authority(LiveToolAuthority::Allow);
            Some(Self {
                work,
                _host: host,
                registry,
                boundary,
            })
        }

        fn execution(&self, command: &str, timeout_ms: u64) -> ToolExecution {
            let action = self
                .boundary
                .prepare(json!({
                    "command": command, "timeout_ms": timeout_ms,
                }))
                .unwrap();
            ToolExecution {
                session: SessionId::new(),
                invocation: InvocationId::new(1).unwrap(),
                attempt: AttemptId::new(1).unwrap(),
                effect_key: "native-exec-test".into(),
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
                progress: crate::ToolProgressPublisher::disabled(),
                artifacts: crate::ArtifactPublisher::closed(),
            }
        }

        fn claim(&self, execution: &ToolExecution) -> crate::workspace_registry::WorkspaceClaim {
            self.registry
                .claim(ClaimKey {
                    session: execution.session,
                    invocation: execution.invocation,
                    attempt: execution.attempt,
                })
                .unwrap()
        }
    }

    #[tokio::test]
    async fn terminal_claim_recovers_lost_session_result_without_reexecuting() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("printf recovered > result.txt", 5000);
        let original = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        let ToolAttemptState::Settled {
            effect,
            receipt: Some(receipt),
            ..
        } = original
        else {
            panic!("exec did not settle: {original:?}");
        };
        assert_eq!(
            effect,
            EffectSummary::KnownChanges {
                paths: vec!["result.txt".into()]
            }
        );
        assert_eq!(
            fs::read(fixture.work.path().join("result.txt")).unwrap(),
            b"recovered"
        );
        let attempt = ToolAttempt {
            id: execution.attempt,
            invocation: execution.invocation,
            ordinal: 1,
            generation: 0,
            executor: fixture.boundary.executor(),
            progress: None,
            state: ToolAttemptState::IntentCommitted {
                start_receipt: None,
            },
        };
        let conflicting = ToolAttempt {
            state: ToolAttemptState::IntentCommitted {
                start_receipt: Some(StartReceipt {
                    kind: IMPLEMENTATION_ID.into(),
                    data: json!({"registry": "other"}),
                }),
            },
            ..attempt.clone()
        };
        assert!(matches!(
            fixture
                .boundary
                .reconcile(execution.clone(), conflicting)
                .await,
            ToolAttemptState::Indeterminate { .. }
        ));
        let partial_receipt =
            partial_exec_receipt(fixture.claim(&execution).start.as_ref().unwrap());
        let partial = ToolAttempt {
            state: ToolAttemptState::Indeterminate {
                reason: "registry write reply lost".into(),
                receipt: Some(partial_receipt.clone()),
            },
            ..attempt.clone()
        };
        assert!(matches!(
            fixture.boundary.reconcile(execution.clone(), partial).await,
            ToolAttemptState::Settled {
                receipt: Some(actual), ..
            } if actual == partial_receipt
        ));
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt.clone())
            .await;
        let ToolAttemptState::Settled {
            result,
            effect: recovered_effect,
            receipt: Some(recovered_receipt),
            retryable: false,
        } = recovered
        else {
            panic!("terminal claim was not adopted: {recovered:?}");
        };
        assert_eq!(recovered_effect, effect);
        assert_eq!(recovered_receipt, receipt);
        assert!(result.is_error);
        assert!(
            result
                .value
                .as_str()
                .unwrap()
                .contains("output and exit status lost on recovery")
        );
        assert_eq!(
            result.capture,
            OutputCapture::Incomplete {
                reason: OutputLoss::LostOnRecovery,
                retained_bytes: 0,
                observed_bytes: None,
            }
        );
        assert!(
            serde_json::to_vec(&result).unwrap().len()
                <= serde_json::to_vec(&crate::tool_exec::output_capacity_result())
                    .unwrap()
                    .len()
        );
        assert_eq!(
            fs::read(fixture.work.path().join("result.txt")).unwrap(),
            b"recovered"
        );
        assert!(fixture.claim(&execution).terminal.is_some());
    }

    #[tokio::test]
    async fn unstarted_terminal_claim_recovers_only_with_exact_no_mutation_evidence() {
        let Some(mut fixture) = Fixture::new() else {
            return;
        };
        assert_eq!(
            fixture.boundary.binding().start_receipts,
            StartReceiptCapability::Authoritative
        );
        let execution = fixture.execution("printf should-not-run > file.txt", 5000);
        let key = ClaimKey {
            session: execution.session,
            invocation: execution.invocation,
            attempt: execution.attempt,
        };
        let attempt = ToolAttempt {
            id: execution.attempt,
            invocation: execution.invocation,
            ordinal: 1,
            generation: 0,
            executor: fixture.boundary.executor(),
            progress: None,
            state: ToolAttemptState::IntentCommitted {
                start_receipt: None,
            },
        };
        let revision = fixture.registry.revision(&execution.workspace).unwrap();
        fixture
            .registry
            .admit(
                &execution.workspace,
                key,
                fixture.boundary.resources,
                revision,
            )
            .unwrap();
        assert!(matches!(
            fixture
                .boundary
                .reconcile(execution.clone(), attempt.clone())
                .await,
            ToolAttemptState::Indeterminate { .. }
        ));
        fixture
            .registry
            .resolve(
                key,
                TerminalEvidence {
                    receipt: RegistryReceipt {
                        backend: execution.workspace.backend.clone(),
                        identity: format!("{}-{}-{}", key.session, key.invocation, key.attempt),
                    },
                    effect: EffectSummary::NoMutation,
                },
            )
            .unwrap();
        assert!(matches!(
            fixture.boundary.reconcile(execution, attempt).await,
            ToolAttemptState::NotStarted { .. }
        ));
        assert!(!fixture.work.path().join("file.txt").exists());
    }

    #[tokio::test]
    async fn start_only_claim_recovers_receipt_without_claiming_scope_stopped() {
        let Some(mut fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("printf uncertain > file.txt", 5000);
        let key = ClaimKey {
            session: execution.session,
            invocation: execution.invocation,
            attempt: execution.attempt,
        };
        let revision = fixture.registry.revision(&execution.workspace).unwrap();
        fixture
            .registry
            .admit(
                &execution.workspace,
                key,
                fixture.boundary.resources,
                revision,
            )
            .unwrap();
        let start = RegistryReceipt {
            backend: execution.workspace.backend.clone(),
            identity: format!(
                "{}-{}-{}:123:124:456",
                key.session, key.invocation, key.attempt
            ),
        };
        fixture.registry.record_start(key, start.clone()).unwrap();
        let attempt = ToolAttempt {
            id: execution.attempt,
            invocation: execution.invocation,
            ordinal: 1,
            generation: 0,
            executor: fixture.boundary.executor(),
            progress: None,
            state: ToolAttemptState::IntentCommitted {
                start_receipt: None,
            },
        };
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt.clone())
            .await;
        assert!(matches!(
            &recovered,
            ToolAttemptState::Indeterminate {
                receipt: Some(receipt), ..
            } if receipt == &recovered_exec_receipt(
                &format!("{}-{}-{}", key.session, key.invocation, key.attempt),
                &start
            ).unwrap()
        ));
        assert!(fixture.claim(&execution).terminal.is_none());
        assert!(!fixture.work.path().join("file.txt").exists());
        fixture
            .registry
            .resolve(
                key,
                TerminalEvidence {
                    receipt: start,
                    effect: EffectSummary::NoMutation,
                },
            )
            .unwrap();
        let prior = ToolAttempt {
            state: recovered.clone(),
            ..attempt
        };
        assert!(matches!(
            fixture.boundary.reconcile(execution, prior).await,
            ToolAttemptState::Settled {
                effect: EffectSummary::NoMutation,
                receipt: Some(_),
                retryable: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn command_output_is_visible_before_scope_settlement() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let hub = crate::progress::ProgressHub::new();
        let mut receiver = hub.subscribe();
        let mut execution = fixture.execution("printf ready; sleep 2; printf done", 5000);
        let (publisher, guard) = hub.tool_attempt(TurnId::new(1).unwrap(), execution.attempt);
        execution.progress = publisher;
        let boundary = Arc::clone(&fixture.boundary);
        let task =
            tokio::spawn(
                async move { boundary.execute(execution, CancellationToken::new()).await },
            );
        let preview = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("command preview before timeout")
            .expect("progress channel open");
        assert!(matches!(
            preview.update,
            ProgressUpdate::ToolOutput {
                stream: ToolOutputStream::Stdout,
                text,
                ..
            } if text == "ready"
        ));
        assert!(!task.is_finished(), "command must still be running");
        let settled = task.await.unwrap();
        assert!(matches!(settled, ToolAttemptState::Settled { .. }));
        drop(guard);
        let mut saw_end = false;
        while let Ok(event) = receiver.try_recv() {
            if matches!(event.update, ProgressUpdate::End) {
                saw_end = true;
            }
        }
        assert!(saw_end, "attempt end must clear provisional output");
    }

    #[test]
    fn arguments_and_authority_are_frozen() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        assert!(fixture.boundary.prepare(json!({"command": ""})).is_err());
        assert!(
            fixture
                .boundary
                .prepare(json!({"command": "true", "timeout_ms": 0}))
                .is_err()
        );
        let action = fixture
            .boundary
            .prepare(json!({"command": "true"}))
            .unwrap();
        assert_eq!(action.authority, ToolAuthority::WorkspaceMutation);
        assert_eq!(
            fixture
                .boundary
                .prepared_arguments(&action)
                .unwrap()
                .timeout_ms,
            DEFAULT_TIMEOUT_MS
        );
        let mut corrupted = action.clone();
        corrupted.arguments["command"] = json!("false");
        assert!(fixture.boundary.prepared_arguments(&corrupted).is_none());
    }

    #[tokio::test]
    async fn denied_action_never_claims_or_starts() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("printf denied > marker", 1000);
        fixture.boundary.set_live_authority(LiveToolAuthority::Deny);
        assert!(matches!(
            fixture
                .boundary
                .execute(execution.clone(), CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        assert!(fixture.registry.unresolved(None, 10).unwrap().is_empty());
        assert!(!fixture.work.path().join("marker").exists());
    }

    #[tokio::test]
    async fn orphaned_claim_blocks_replay_without_losing_quarantine() {
        let Some(mut fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("printf replay > marker", 1000);
        let key = ClaimKey {
            session: execution.session,
            invocation: execution.invocation,
            attempt: execution.attempt,
        };
        let revision = fixture.registry.revision(&execution.workspace).unwrap();
        fixture
            .registry
            .admit(
                &execution.workspace,
                key,
                fixture.boundary.resources,
                revision,
            )
            .unwrap();
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::Indeterminate { .. }));
        assert!(!fixture.work.path().join("marker").exists());
        assert_eq!(fixture.registry.unresolved(None, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn escaped_session_descendant_stops_before_terminal_evidence() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let command = "setsid sh -c 'while :; do printf x >> marker; sleep 0.02; done' >/dev/null 2>&1 & sleep 0.15";
        let execution = fixture.execution(command, 3000);
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(
            matches!(
                state,
                ToolAttemptState::Settled {
                    effect: EffectSummary::KnownChanges { .. },
                    ..
                }
            ),
            "{state:?}"
        );
        let before = fs::metadata(fixture.work.path().join("marker"))
            .unwrap()
            .len();
        thread::sleep(Duration::from_millis(150));
        let after = fs::metadata(fixture.work.path().join("marker"))
            .unwrap()
            .len();
        assert_eq!(
            before, after,
            "detached child kept writing after settlement"
        );
        assert!(fixture.claim(&execution).terminal.is_some());
        assert!(fixture.registry.unresolved(None, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancellation_joins_scope_and_bounds_output() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let command = "setsid sh -c 'while :; do printf x >> marker; sleep 0.02; done' >/dev/null 2>&1 & while :; do printf a; done";
        let execution = fixture.execution(command, 3000);
        let stop = CancellationToken::new();
        let boundary = Arc::clone(&fixture.boundary);
        let running = tokio::spawn({
            let execution = execution.clone();
            let stop = stop.clone();
            async move { boundary.execute(execution, stop).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        stop.cancel();
        let state = running.await.unwrap();
        let ToolAttemptState::Settled {
            result,
            effect: EffectSummary::KnownChanges { .. },
            ..
        } = state
        else {
            panic!("expected joined cancellation: {state:?}");
        };
        assert!(result.is_error);
        assert!(matches!(result.capture, OutputCapture::Incomplete { .. }));
        assert!(serde_json::to_vec(&result).unwrap().len() <= execution.output_limit);
        let before = fs::metadata(fixture.work.path().join("marker"))
            .unwrap()
            .len();
        thread::sleep(Duration::from_millis(150));
        let after = fs::metadata(fixture.work.path().join("marker"))
            .unwrap()
            .len();
        assert_eq!(before, after);
        assert!(fixture.claim(&execution).terminal.is_some());
    }

    #[tokio::test]
    async fn timeout_is_terminal_and_not_replayed() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("sleep 5", 100);
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        let ToolAttemptState::Settled { result, .. } = state else {
            panic!("timeout did not settle: {state:?}")
        };
        assert_eq!(result.value["timed_out"], true);
        let second = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(second, ToolAttemptState::Indeterminate { .. }));
        assert!(fixture.claim(&execution).terminal.is_some());
    }

    #[tokio::test]
    async fn private_git_success_does_not_claim_live_repository_mutation() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("git init -q", 3000);
        let state = fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        let ToolAttemptState::Settled {
            result,
            effect: EffectSummary::NoMutation,
            ..
        } = state
        else {
            panic!("private Git outcome did not settle: {state:?}");
        };
        assert_eq!(result.value["exit_code"], 0);
        assert_eq!(result.value["git_metadata_changed"], true);
        assert!(result.is_error);
        assert!(
            result.value["import_error"]
                .as_str()
                .unwrap()
                .contains("did not reach the live workspace")
        );
        assert!(!fixture.work.path().join(".git").exists());
    }

    #[tokio::test]
    async fn selected_readonly_toolchain_runs_with_private_writable_state() {
        let tools = Temp::new();
        fs::create_dir(tools.path().join("bin")).unwrap();
        let probe = tools.path().join("bin/probe-tool");
        fs::write(&probe, b"#!/bin/sh\nprintf 'tool-ok:%s' \"$HOME\"\n").unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
        let cache = Temp::new();
        fs::write(cache.path().join("cached-crate"), b"public code").unwrap();
        let Some(fixture) = Fixture::new_with_roots(Some(tools.path()), Some(cache.path())) else {
            return;
        };
        let plain = Fixture::new().unwrap();
        assert_ne!(
            plain.boundary.tool_binding().implementation,
            fixture.boundary.tool_binding().implementation
        );
        let execution = fixture.execution("probe-tool", 3000);
        let state = fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        let ToolAttemptState::Settled { result, .. } = state else {
            panic!("selected toolchain did not run: {state:?}");
        };
        assert_eq!(result.value["stdout"], "tool-ok:/tmp");
        assert_eq!(result.value["exit_code"], 0);
        assert!(fixture.registry.unresolved(None, 10).unwrap().is_empty());
    }

    #[test]
    fn readonly_toolchain_refuses_broker_socket() {
        let root = Temp::new();
        let _socket = UnixListener::bind(root.path().join("broker.sock")).unwrap();
        assert!(matches!(
            validate_readonly_tree(root.path()),
            Err(NativeExecError::InvalidToolchain)
        ));
        let Some(fixture) = Fixture::new() else {
            return;
        };
        assert!(matches!(
            NativeExecBoundary::new(
                &fixture.registry,
                fixture.boundary.workspace.clone(),
                &fixture._host.path().join("staging"),
                None,
                Some(root.path()),
            ),
            Err(NativeExecError::InvalidToolchain)
        ));
    }

    #[tokio::test]
    async fn unavailable_durable_import_plan_prevents_publication() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("printf private > marker", 3000);
        let plan_name = format!(
            "exec-{}-{}-{}-plan.json",
            execution.session, execution.invocation, execution.attempt
        );
        fs::write(
            fixture._host.path().join("staging").join(plan_name),
            b"collision",
        )
        .unwrap();
        let state = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::Indeterminate { .. }));
        assert!(!fixture.work.path().join("marker").exists());
        let claim = fixture.claim(&execution);
        assert!(claim.start.is_some());
        assert!(claim.terminal.is_none());
    }

    #[tokio::test]
    async fn owner_loss_child() {
        let Ok(root) = std::env::var("ION_EXEC_OWNER_LOSS_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let work = Temp(root.join("workspace"));
        let host = Temp(root.join("host"));
        let mut registry = WorkspaceRegistry::open(host.path()).unwrap();
        let workspace = registry
            .bind("owner-loss-workspace", work.path(), "local-v1")
            .unwrap();
        let boundary = Arc::new(
            NativeExecBoundary::new(
                &registry,
                workspace,
                &host.path().join("staging"),
                None,
                None,
            )
            .unwrap(),
        );
        boundary.set_live_authority(LiveToolAuthority::Allow);
        let fixture = Fixture {
            work,
            _host: host,
            registry,
            boundary,
        };
        let execution = fixture.execution(
            "setsid sh -c 'while :; do printf x >> marker; sleep 0.02; done' >/dev/null 2>&1 & sleep 60",
            120_000,
        );
        let _ = fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        panic!("owner-loss child unexpectedly reached terminal execution");
    }

    #[tokio::test]
    async fn stopped_supervisor_without_exit_status_discards_private_changes() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let execution = fixture.execution("printf private > marker; sleep 60", 10_000);
        let identity = format!(
            "{}-{}-{}",
            execution.session, execution.invocation, execution.attempt
        );
        let private_marker = fixture
            ._host
            .path()
            .join(format!("staging/exec-{identity}-command/marker"));
        let boundary = Arc::clone(&fixture.boundary);
        let submitted = execution.clone();
        let task =
            tokio::spawn(
                async move { boundary.execute(submitted, CancellationToken::new()).await },
            );
        let deadline = Instant::now() + Duration::from_secs(5);
        let supervisor_pid = loop {
            let started = fixture.registry.claim(ClaimKey {
                session: execution.session,
                invocation: execution.invocation,
                attempt: execution.attempt,
            });
            if fs::metadata(&private_marker).is_ok_and(|meta| meta.len() > 0)
                && let Ok(Some(start)) = started.map(|claim| claim.start)
            {
                break start
                    .identity
                    .rsplit(':')
                    .nth(2)
                    .and_then(|pid| pid.parse::<i32>().ok())
                    .and_then(Pid::from_raw)
                    .expect("durable supervisor pid");
            }
            assert!(Instant::now() < deadline, "private command did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let supervisor = pidfd_open(supervisor_pid, PidfdFlags::empty()).unwrap();
        rustix::process::pidfd_send_signal(&supervisor, rustix::process::Signal::KILL).unwrap();
        let state = task.await.unwrap();
        let ToolAttemptState::Settled {
            result,
            effect: EffectSummary::NoMutation,
            retryable: false,
            ..
        } = state
        else {
            panic!("stopped supervisor did not settle: {state:?}");
        };
        assert!(result.is_error);
        assert_eq!(result.value["private_changes_imported"], false);
        assert!(matches!(
            result.capture,
            OutputCapture::Incomplete {
                reason: OutputLoss::BackendFailure,
                ..
            }
        ));
        assert!(!fixture.work.path().join("marker").exists());
        assert!(fixture.registry.unresolved(None, 10).unwrap().is_empty());
    }

    #[test]
    fn owner_loss_stops_detached_descendant_without_importing() {
        if !Path::new(BWRAP).is_file() {
            return;
        }
        let root = Temp::new();
        fs::create_dir(root.path().join("workspace")).unwrap();
        fs::create_dir(root.path().join("host")).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path().join("host/staging"))
            .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_exec::tests::owner_loss_child",
                "--nocapture",
            ])
            .env("ION_EXEC_OWNER_LOSS_ROOT", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let marker = loop {
            let found = fs::read_dir(root.path().join("host/staging"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("marker"))
                .find(|path| fs::metadata(path).is_ok_and(|meta| meta.len() > 0));
            if let Some(marker) = found {
                break marker;
            }
            if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owner-loss child did not start writing in its private scope");
            }
            thread::sleep(Duration::from_millis(20));
        };
        child.kill().unwrap();
        child.wait().unwrap();
        thread::sleep(Duration::from_millis(200));
        let before = fs::metadata(&marker).unwrap().len();
        thread::sleep(Duration::from_millis(200));
        let after = fs::metadata(&marker).unwrap().len();
        assert_eq!(before, after, "detached descendant survived owner death");
        assert!(!root.path().join("workspace/marker").exists());
        let registry = WorkspaceRegistry::open(root.path().join("host")).unwrap();
        let claims = registry.unresolved(None, 10).unwrap();
        assert_eq!(claims.len(), 1);
        assert!(claims[0].start.is_some());
        assert!(claims[0].terminal.is_none());
    }

    #[test]
    fn command_preview_uses_allowance_and_fits_escaped_output() {
        let status = Command::new("/usr/bin/true").status().unwrap();
        let readable = Captured {
            bytes: vec![b'a'; 1500],
            observed: 1500,
            truncated: false,
        };
        let empty = Captured {
            bytes: Vec::new(),
            observed: 0,
            truncated: false,
        };
        let fitted = fit_result(
            command_result(status, readable, empty, false, false),
            4096,
            1500,
        );
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= 4096);
        assert_eq!(fitted.value["stdout"].as_str().unwrap().len(), 1500);

        let escaped = Captured {
            bytes: vec![b'\n'; 3000],
            observed: 3000,
            truncated: false,
        };
        let fitted = fit_result(
            command_result(
                Command::new("/usr/bin/true").status().unwrap(),
                escaped,
                Captured {
                    bytes: Vec::new(),
                    observed: 0,
                    truncated: false,
                },
                false,
                false,
            ),
            4096,
            3000,
        );
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= 4096);
        assert!(matches!(fitted.capture, OutputCapture::Incomplete { .. }));
        assert!(!fitted.value["stdout"].as_str().unwrap().is_empty());
    }
}

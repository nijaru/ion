//! Native bounded read-only filesystem ToolBoundary.
//!
//! Paths use descriptor-relative no-follow traversal. The host must protect the
//! workspace namespace from concurrent renames by untrusted writers: this is not an
//! OS sandbox, complete race-free beneath-root guarantee, or snapshot.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::{fd::OwnedFd, unix::fs::MetadataExt},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ion_ai::ToolSpec;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, open, openat, statat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    ApprovalState, EgressRealm, LiveToolAuthority, PreparedAction, SemanticCompatibilityId,
    ToolAttemptState, ToolAuthority, ToolBinding, ToolBindingId, ToolBoundary, ToolBoundaryError,
    ToolConcurrency, ToolExecution, ToolRecoveryPolicy, ToolResult, WorkspaceBinding,
};

/// Hard maximum for one configured file range before the smaller inline-result limit.
pub const MAX_NATIVE_READ_BYTES: usize = crate::MAX_TOOL_RECORD_BYTES;

const MAX_PATH_BYTES: usize = 4096;
const MAX_CONCURRENT_READS: usize = 4;
const READ_CHUNK_BYTES: usize = 8192;
const IMPLEMENTATION_ID: &str = "native-read-v1";
const AUTHORITY_ALLOW: u8 = 0;
const AUTHORITY_ASK: u8 = 1;
const AUTHORITY_DENY: u8 = 2;

/// Native bounded file reader bound to one exact frozen tool and workspace.
pub struct NativeReadBoundary {
    binding: ToolBinding,
    workspace: WorkspaceBinding,
    executor: SemanticCompatibilityId,
    root: File,
    root_identity: RootIdentity,
    max_read_bytes: usize,
    live_authority: Arc<AtomicU8>,
    permits: Arc<Semaphore>,
}

impl std::fmt::Debug for NativeReadBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeReadBoundary")
            .field("binding", &self.binding.id)
            .field("workspace", &self.workspace.id)
            .field("max_read_bytes", &self.max_read_bytes)
            .finish_non_exhaustive()
    }
}

impl NativeReadBoundary {
    /// Pin the configured canonical root and freeze the built-in read binding.
    /// Live authority starts denied; the host must explicitly set its current policy.
    pub fn new(
        workspace: WorkspaceBinding,
        max_read_bytes: usize,
    ) -> Result<Self, NativeReadError> {
        if !(1..=MAX_NATIVE_READ_BYTES).contains(&max_read_bytes) {
            return Err(NativeReadError::InvalidReadLimit);
        }
        if workspace.id.is_empty()
            || workspace.object_identity.is_empty()
            || workspace.canonical_root.len() > MAX_PATH_BYTES
        {
            return Err(NativeReadError::InvalidWorkspace);
        }
        let executor = SemanticCompatibilityId::new(workspace.backend.clone())
            .map_err(|_| NativeReadError::InvalidWorkspace)?;
        let binding = native_read_binding().map_err(NativeReadError::InvalidBinding)?;
        let root = open_absolute_directory(&workspace.canonical_root)?;
        let root_identity = identity(&root)?;
        // A caller-supplied identity is not independently verifiable here. Hosts must
        // obtain bindings from WorkspaceRegistry and protect the namespace from rename.

        Ok(Self {
            binding,
            workspace,
            executor,
            root,
            root_identity,
            max_read_bytes,
            live_authority: Arc::new(AtomicU8::new(AUTHORITY_DENY)),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_READS)),
        })
    }

    /// Set the current host policy. The atomic read immediately before opening the
    /// requested file is this backend's admission point; a later revocation cannot
    /// undo a read already admitted there.
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

    fn prepared_arguments(&self, action: &PreparedAction) -> Option<ReadArguments> {
        if action.binding != self.binding.id
            || action.authority != ToolAuthority::ReadOnly
            || action.egress != EgressRealm::Local
            || action.workspace_revision.is_some()
            || !action.base_facts.is_empty()
        {
            return None;
        }
        let arguments: ReadArguments = serde_json::from_value(action.arguments.clone()).ok()?;
        let limit = usize::try_from(arguments.limit?).ok()?;
        if !valid_relative_path(&arguments.path)
            || arguments.path.len() > MAX_PATH_BYTES
            || limit == 0
            || limit > self.max_read_bytes
        {
            return None;
        }
        let mut arguments = arguments;
        arguments.limit = Some(limit as u64);
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

impl ToolBoundary for NativeReadBoundary {
    fn binding(&self) -> ToolBinding {
        self.binding.clone()
    }

    fn executor(&self) -> SemanticCompatibilityId {
        self.executor.clone()
    }

    fn prepare(&self, arguments: Value) -> Result<PreparedAction, ToolBoundaryError> {
        if arguments
            .as_object()
            .is_some_and(|object| object.get("limit").is_some_and(Value::is_null))
        {
            return Err(ToolBoundaryError::InvalidArguments);
        }
        let mut arguments: ReadArguments =
            serde_json::from_value(arguments).map_err(|_| ToolBoundaryError::InvalidArguments)?;
        let limit = arguments
            .limit
            .map(usize::try_from)
            .transpose()
            .map_err(|_| ToolBoundaryError::InvalidArguments)?
            .unwrap_or(self.max_read_bytes);
        if !valid_relative_path(&arguments.path)
            || arguments.path.len() > MAX_PATH_BYTES
            || limit == 0
            || limit > self.max_read_bytes
        {
            return Err(ToolBoundaryError::InvalidArguments);
        }
        arguments.limit = Some(limit as u64);
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
                return not_started("invalid or incompatible prepared read action");
            };
            if execution.binding != self.binding
                || execution.workspace != self.workspace
                || execution.action.authority != ToolAuthority::ReadOnly
                || !execution.action.permitted_by(&execution.ceiling)
            {
                return not_started("read binding, workspace, or authority changed");
            }
            if stop.is_cancelled() {
                return not_started("read cancelled before admission");
            }

            let output_limit = execution.output_limit.min(crate::MAX_TOOL_RECORD_BYTES / 2);
            let Some(content_limit) = content_budget(output_limit, arguments.offset) else {
                return not_started("tool output limit cannot fit a bounded read result");
            };
            let content_limit = content_limit.min(self.max_read_bytes);

            let permit = tokio::select! {
                biased;
                () = stop.cancelled() => return not_started("read cancelled before admission"),
                permit = Arc::clone(&self.permits).acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return not_started("read backend is shutting down"),
                },
            };
            if stop.is_cancelled() {
                return not_started("read cancelled before admission");
            }
            let root = match self.root.try_clone() {
                Ok(root) => root,
                Err(_) => return not_started("workspace root is no longer available"),
            };
            let drop_cancellation = CancelWorkerOnDrop::new(stop.clone());
            let job = ReadJob {
                execution,
                arguments,
                root,
                root_path: self.workspace.canonical_root.clone(),
                root_identity: self.root_identity,
                live_authority: Arc::clone(&self.live_authority),
                executor: self.executor.clone(),
                binding: self.binding.clone(),
                stop,
                content_limit,
                output_limit,
                _permit: permit,
            };
            let result = tokio::task::spawn_blocking(move || run_read(job)).await;
            drop_cancellation.disarm();
            match result {
                Ok(state) => state,
                Err(_) => settled_error("read worker failed", output_limit),
            }
        })
    }
}

/// Construct the stable frozen declaration for the native `read` tool.
pub fn native_read_binding() -> Result<ToolBinding, crate::ConfigError> {
    ToolBinding::new(
        ToolBindingId::new("read")?,
        ToolSpec {
            name: "read".into(),
            description:
                "Read a bounded byte range from a regular file under the bound workspace root."
                    .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_NATIVE_READ_BYTES}
                }
            }),
        },
        SemanticCompatibilityId::new(IMPLEMENTATION_ID)?,
        // Host must ensure the namespace cannot be concurrently renamed; the core
        // descriptor walk alone cannot establish parallel-safe confinement.
        ToolConcurrency::Serial,
        ToolRecoveryPolicy::RepeatAfterNotStartedOrNoMutation,
        EgressRealm::Local,
    )
}

/// Constructor and frozen-root validation errors.
#[derive(Debug, Error)]
pub enum NativeReadError {
    #[error("invalid native read workspace binding")]
    InvalidWorkspace,
    #[error("native read byte limit must be between 1 and 65536")]
    InvalidReadLimit,
    #[error("failed to construct the native read binding: {0}")]
    InvalidBinding(#[source] crate::ConfigError),
    #[error("failed to open canonical workspace root: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

fn identity(file: &File) -> std::io::Result<RootIdentity> {
    let metadata = file.metadata()?;
    Ok(RootIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
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
        let fd = openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        current = File::from(fd);
    }
    Ok(current)
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn content_budget(output_limit: usize, offset: u64) -> Option<usize> {
    let maximum = output_limit.min(crate::MAX_TOOL_RECORD_BYTES);
    let baseline = read_result(
        String::new(),
        offset,
        u64::MAX,
        crate::OutputCapture::CompleteInline,
    );
    let baseline_size = serde_json::to_vec(&baseline).ok()?.len();
    maximum
        .checked_sub(baseline_size)
        .map(|remaining| remaining / 6)
}

fn not_started(reason: &str) -> ToolAttemptState {
    ToolAttemptState::NotStarted {
        reason: reason.to_owned(),
    }
}

fn error_result(message: &str) -> ToolResult {
    ToolResult {
        value: json!({"error": message}),
        is_error: true,
        capture: crate::OutputCapture::CompleteInline,
    }
}

fn settled_error(message: &str, output_limit: usize) -> ToolAttemptState {
    let result = error_result(message);
    if serde_json::to_vec(&result)
        .is_ok_and(|encoded| encoded.len() <= output_limit.min(crate::MAX_TOOL_RECORD_BYTES))
    {
        ToolAttemptState::Settled {
            result,
            effect: crate::EffectSummary::NoMutation,
            receipt: None,
            retryable: true,
        }
    } else {
        not_started("tool output limit cannot fit an error result")
    }
}

fn read_result(
    content: String,
    offset: u64,
    bytes_read: u64,
    capture: crate::OutputCapture,
) -> ToolResult {
    ToolResult {
        value: json!({"content": content, "offset": offset, "bytes_read": bytes_read}),
        is_error: false,
        capture,
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

struct ReadJob {
    execution: ToolExecution,
    arguments: ReadArguments,
    root: File,
    root_path: String,
    root_identity: RootIdentity,
    live_authority: Arc<AtomicU8>,
    executor: SemanticCompatibilityId,
    binding: ToolBinding,
    stop: CancellationToken,
    content_limit: usize,
    output_limit: usize,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

fn run_read(job: ReadJob) -> ToolAttemptState {
    if job.stop.is_cancelled() {
        return not_started("read cancelled before admission");
    }
    match open_absolute_directory(&job.root_path).and_then(|file| identity(&file)) {
        Ok(identity) if identity == job.root_identity => {}
        _ => return not_started("workspace root identity changed before read admission"),
    }
    if !job.admission_snapshot_valid() {
        return not_started("live read authority denied before admission");
    }
    if job.stop.is_cancelled() {
        return not_started("read cancelled before admission");
    }

    // This policy load is the live-authority admission point; the read itself is
    // immediately below and stays anchored to the constructor-pinned root descriptor.
    let policy = job.live_authority.load(Ordering::SeqCst);
    if !job.admission_allowed(policy) {
        return not_started("live read authority denied before admission");
    }
    if job.stop.is_cancelled() {
        return not_started("read cancelled before admission");
    }

    let mut file = match open_relative_regular_file(&job.root, &job.arguments.path) {
        Ok(file) => file,
        Err(_) => {
            return settled_error(
                "path could not be opened as a regular file",
                job.output_limit,
            );
        }
    };
    if file.seek(SeekFrom::Start(job.arguments.offset)).is_err() {
        return settled_error("file range could not be read", job.output_limit);
    }

    let read_limit = job
        .arguments
        .limit
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(0)
        .min(job.content_limit);
    let Some(capture_limit) = read_limit.checked_add(1) else {
        return settled_error("file range is too large", job.output_limit);
    };
    let mut captured = Vec::with_capacity(capture_limit);
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    while captured.len() < capture_limit {
        if job.stop.is_cancelled() {
            return settled_error("read cancelled after admission", job.output_limit);
        }
        let remaining = capture_limit - captured.len();
        let chunk_len = remaining.min(READ_CHUNK_BYTES);
        match file.read(&mut chunk[..chunk_len]) {
            Ok(0) => break,
            Ok(read) => captured.extend_from_slice(&chunk[..read]),
            Err(_) => return settled_error("file range could not be read", job.output_limit),
        }
    }
    let truncated = captured.len() > read_limit;
    captured.truncate(read_limit);
    let content = match std::str::from_utf8(&captured) {
        Ok(content) => content.to_owned(),
        Err(error) if truncated && error.error_len().is_none() => {
            captured.truncate(error.valid_up_to());
            match String::from_utf8(captured) {
                Ok(content) => content,
                Err(_) => {
                    return settled_error("file content is not valid UTF-8", job.output_limit);
                }
            }
        }
        Err(_) => return settled_error("file content is not valid UTF-8", job.output_limit),
    };
    let bytes_read = match u64::try_from(content.len()) {
        Ok(bytes_read) => bytes_read,
        Err(_) => return settled_error("file range is too large", job.output_limit),
    };
    // A short requested range is complete. Only a quota-limited range is incomplete.
    let quota_limited = truncated && read_limit < job.arguments.limit.unwrap_or(0) as usize;
    let capture = if quota_limited {
        crate::OutputCapture::Incomplete {
            reason: crate::OutputLoss::Quota,
            retained_bytes: bytes_read,
            observed_bytes: bytes_read.checked_add(1),
        }
    } else {
        crate::OutputCapture::CompleteInline
    };
    let result = read_result(content, job.arguments.offset, bytes_read, capture);
    if serde_json::to_vec(&result).map_or(true, |encoded| {
        encoded.len() > job.output_limit.min(crate::MAX_TOOL_RECORD_BYTES)
    }) {
        return settled_error(
            "read result exceeded its configured output bound",
            job.output_limit,
        );
    }
    ToolAttemptState::Settled {
        result,
        effect: crate::EffectSummary::NoMutation,
        receipt: None,
        retryable: true,
    }
}

impl ReadJob {
    fn admission_snapshot_valid(&self) -> bool {
        self.execution.binding == self.binding
            && self.execution.workspace.canonical_root == self.root_path
            && self.execution.action.authority == ToolAuthority::ReadOnly
            && self.execution.action.permitted_by(&self.execution.ceiling)
    }

    fn admission_allowed(&self, policy: u8) -> bool {
        if !self.admission_snapshot_valid() {
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

fn open_relative_regular_file(root: &File, path: &str) -> std::io::Result<File> {
    let mut components = path.split('/').peekable();
    let mut parent = root.try_clone()?;
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            let fd = openat(
                &parent,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            parent = File::from(fd);
            continue;
        }
        // Reject special files before open to avoid opening a device/FIFO with side
        // effects. This check remains subject to a hostile concurrent rename race.
        let before = statat(&parent, component, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        let fd: OwnedFd = openat(
            &parent,
            component,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let file = File::from(fd);
        if FileType::from_raw_mode(fstat(&file)?.st_mode) != FileType::RegularFile {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        return Ok(file);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "empty relative path",
    ))
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
                std::env::temp_dir().join(format!("ion-native-read-{}-{id}", std::process::id()));
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

    fn workspace(root: &Path) -> WorkspaceBinding {
        WorkspaceBinding {
            id: "test-workspace".into(),
            canonical_root: root.to_string_lossy().into_owned(),
            backend: "local-v1".into(),
            object_identity: "test-object".into(),
        }
    }

    fn boundary(root: &Path, max_read_bytes: usize) -> NativeReadBoundary {
        let boundary = NativeReadBoundary::new(workspace(root), max_read_bytes).unwrap();
        boundary.set_live_authority(LiveToolAuthority::Allow);
        boundary
    }

    fn execution(
        boundary: &NativeReadBoundary,
        action: PreparedAction,
        output_limit: usize,
    ) -> ToolExecution {
        ToolExecution {
            session: SessionId::new(),
            invocation: InvocationId::new(1).unwrap(),
            attempt: AttemptId::new(1).unwrap(),
            effect_key: "native-read-test".into(),
            binding: boundary.binding(),
            action,
            workspace: boundary.workspace.clone(),
            ceiling: AuthorityCeiling {
                workspace_mutation: false,
                unconfined_execution: false,
                remote_tools: false,
                egress_realms: vec![EgressRealm::Local],
            },
            approval: ApprovalState::NotRequired,
            output_limit,
        }
    }

    async fn read(
        boundary: &NativeReadBoundary,
        args: Value,
        output_limit: usize,
    ) -> ToolAttemptState {
        let action = boundary.prepare(args).unwrap();
        boundary
            .execute(
                execution(boundary, action, output_limit),
                CancellationToken::new(),
            )
            .await
    }

    fn result(state: ToolAttemptState) -> ToolResult {
        match state {
            ToolAttemptState::Settled { result, .. } => result,
            other => panic!("expected settled read, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reads_exact_offset_range_and_reports_range_truncation() {
        let root = TestRoot::new();
        fs::write(root.path().join("source.txt"), "zero\none\n").unwrap();
        let boundary = boundary(root.path(), 32);

        let exact = result(
            read(
                &boundary,
                json!({"path":"source.txt", "offset":5, "limit":4}),
                4096,
            )
            .await,
        );
        assert!(!exact.is_error);
        assert_eq!(exact.capture, crate::OutputCapture::CompleteInline);
        assert_eq!(exact.value["content"], "one\n");
        assert_eq!(exact.value["bytes_read"], 4);

        let truncated = result(
            read(
                &boundary,
                json!({"path":"source.txt", "offset":5, "limit":3}),
                4096,
            )
            .await,
        );
        assert!(!truncated.is_error);
        assert_eq!(truncated.capture, crate::OutputCapture::CompleteInline);
        assert_eq!(truncated.value["content"], "one");
    }

    #[tokio::test]
    async fn symlink_leaf_and_parent_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new();
        let outside = TestRoot::new();
        fs::write(outside.path().join("secret.txt"), "outside").unwrap();
        fs::create_dir(root.path().join("subdir")).unwrap();
        symlink(
            outside.path().join("secret.txt"),
            root.path().join("leaf-link"),
        )
        .unwrap();
        symlink(outside.path(), root.path().join("parent-link")).unwrap();
        let boundary = boundary(root.path(), 32);

        for path in ["leaf-link", "parent-link/secret.txt"] {
            let response = result(read(&boundary, json!({"path":path}), 4096).await);
            assert!(response.is_error);
            assert!(response.value["error"].is_string());
            assert_ne!(response.value["content"], "outside");
        }
        assert_eq!(
            result(read(&boundary, json!({"path":"subdir"}), 4096).await).value["error"],
            "path could not be opened as a regular file"
        );
    }

    #[tokio::test]
    async fn preparation_freezes_canonical_typed_arguments_to_the_exact_read_binding() {
        let root = TestRoot::new();
        let boundary = boundary(root.path(), 32);
        let binding = boundary.binding();
        let action =
            crate::tool_boundary::prepare_action(&boundary, &binding, json!({"path":"source.txt"}))
                .unwrap();
        assert_eq!(action.binding, binding.id);
        assert_eq!(action.authority, ToolAuthority::ReadOnly);
        assert_eq!(
            action.arguments,
            json!({"path":"source.txt", "offset":0, "limit":32})
        );

        let mut wrong_workspace = execution(&boundary, action.clone(), 4096);
        wrong_workspace.workspace.object_identity = "replacement-object".into();
        assert!(matches!(
            boundary
                .execute(wrong_workspace, CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));

        let mut changed = action;
        changed.arguments["path"] = json!("other.txt");
        assert!(matches!(
            boundary
                .execute(
                    execution(&boundary, changed, 4096),
                    CancellationToken::new()
                )
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
    }

    #[tokio::test]
    async fn root_replacement_is_rejected_before_opening_the_replacement() {
        let root = TestRoot::new();
        fs::write(root.path().join("data.txt"), "original").unwrap();
        let boundary = boundary(root.path(), 32);
        let moved = root.path().with_extension("old");
        fs::rename(root.path(), &moved).unwrap();
        fs::create_dir(root.path()).unwrap();
        fs::write(root.path().join("data.txt"), "replacement").unwrap();

        let action = boundary.prepare(json!({"path":"data.txt"})).unwrap();
        assert!(matches!(
            boundary
                .execute(execution(&boundary, action, 4096), CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        fs::remove_dir_all(moved).unwrap();
    }

    #[tokio::test]
    async fn cancellation_and_live_revocation_prevent_admission() {
        let root = TestRoot::new();
        fs::write(root.path().join("data.txt"), "must not be read").unwrap();
        let boundary = boundary(root.path(), 32);
        let action = boundary.prepare(json!({"path":"data.txt"})).unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(matches!(
            boundary
                .execute(execution(&boundary, action.clone(), 4096), stop)
                .await,
            ToolAttemptState::NotStarted { .. }
        ));

        boundary.set_live_authority(LiveToolAuthority::Deny);
        assert_eq!(
            boundary.live_authority(&action, &boundary.workspace),
            LiveToolAuthority::Deny
        );
        assert!(matches!(
            boundary
                .execute(execution(&boundary, action, 4096), CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
    }

    #[tokio::test]
    async fn ask_policy_requires_exact_unexpired_approval() {
        let root = TestRoot::new();
        fs::write(root.path().join("data.txt"), "approved").unwrap();
        let boundary = boundary(root.path(), 32);
        boundary.set_live_authority(LiveToolAuthority::Ask);
        let action = boundary.prepare(json!({"path":"data.txt"})).unwrap();
        let mut execution = execution(&boundary, action.clone(), 4096);
        assert!(matches!(
            boundary
                .execute(execution.clone(), CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        execution.approval = ApprovalState::Approved {
            action_digest: action.digest,
            implementation: execution.binding.implementation.clone(),
            executor: boundary.executor(),
            workspace: execution.workspace.clone(),
            expires_at_unix_ms: i64::MAX,
        };
        assert_eq!(
            result(boundary.execute(execution, CancellationToken::new()).await).value["content"],
            "approved"
        );
    }

    #[tokio::test]
    async fn inline_output_stays_within_output_limit_and_marks_capacity_truncation() {
        let root = TestRoot::new();
        fs::write(root.path().join("large.txt"), "x".repeat(10_000)).unwrap();
        let boundary = boundary(root.path(), 10_000);
        let result = result(read(&boundary, json!({"path":"large.txt"}), 512).await);
        assert!(!result.is_error);
        assert!(matches!(
            result.capture,
            crate::OutputCapture::Incomplete { .. }
        ));
        assert!(!result.value["content"].as_str().unwrap().is_empty());
        assert!(serde_json::to_vec(&result).unwrap().len() <= 512);
    }

    #[tokio::test]
    async fn invalid_utf8_is_an_explicit_bounded_error() {
        let root = TestRoot::new();
        fs::write(root.path().join("binary.bin"), [0xff, 0xfe]).unwrap();
        let boundary = boundary(root.path(), 16);
        let result = result(read(&boundary, json!({"path":"binary.bin"}), 512).await);
        assert!(result.is_error);
        assert_eq!(result.value["error"], "file content is not valid UTF-8");
    }

    #[test]
    fn preparation_rejects_absolute_traversal_and_noncanonical_paths() {
        let root = TestRoot::new();
        let boundary = boundary(root.path(), 32);
        for path in ["/etc/passwd", "../secret", "a/../secret", "a//b", "./file"] {
            assert!(matches!(
                boundary.prepare(json!({"path":path})),
                Err(ToolBoundaryError::InvalidArguments)
            ));
        }
        assert!(matches!(
            boundary.prepare(json!({"path":"file", "limit":null})),
            Err(ToolBoundaryError::InvalidArguments)
        ));
    }
}

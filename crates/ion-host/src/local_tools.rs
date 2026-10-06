//! Direct coding tools with the caller's host permissions.
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::OpenOptionsExt, fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use ion_ai::{BoxFuture, ImageMime, MAX_SOURCE_BYTES, ToolCall, ToolSpec, normalize_image};
use rustix::process::{Pid, Signal, kill_process_group};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use ion_core::{
    CodingToolOutput as ToolOutput, CodingToolSource as ToolSource, ToolActivityKind,
    ToolDefinition, ToolExecutor, ToolExposure, ToolPresentation, ToolRegistration,
};

const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const POST_EXIT_OUTPUT_IDLE: Duration = Duration::from_millis(100);

pub struct LocalTools {
    cwd: PathBuf,
}
impl LocalTools {
    pub fn new(cwd: impl AsRef<Path>) -> std::io::Result<Self> {
        let cwd = cwd.as_ref().canonicalize()?;
        if !cwd.is_dir() {
            return Err(std::io::Error::other(
                "working directory is not a directory",
            ));
        }
        Ok(Self { cwd })
    }
    fn path(&self, raw: &str) -> Result<PathBuf, String> {
        if raw.is_empty() || raw.contains('\0') {
            return Err("invalid path".into());
        }
        Ok(self.cwd.join(raw))
    }

    /// Use the same shell execution path for a user's direct terminal command.
    pub async fn run_user_shell(&self, command: &str, stop: CancellationToken) -> ToolOutput {
        self.exec(&json!({"command": command}), stop).await
    }
}

impl ToolSource for LocalTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        specs()
            .into_iter()
            .map(|spec| {
                let (operation, kind, key) = match spec.name.as_str() {
                    "read" => (LocalOperation::Read, ToolActivityKind::Read, "path"),
                    "edit" => (LocalOperation::Edit, ToolActivityKind::Edit, "path"),
                    "write" => (LocalOperation::Write, ToolActivityKind::Write, "path"),
                    "exec" => (LocalOperation::Exec, ToolActivityKind::Command, "command"),
                    _ => unreachable!("native specs and operations have one owner"),
                };
                ToolRegistration::new(
                    ToolDefinition {
                        spec,
                        presentation: ToolPresentation::argument(kind, key),
                        exposure: ToolExposure::Direct,
                    },
                    Arc::new(LocalExecutor {
                        tools: self.clone(),
                        operation,
                    }),
                )
            })
            .collect()
    }
}

enum LocalOperation {
    Read,
    Edit,
    Write,
    Exec,
}

struct LocalExecutor {
    tools: Arc<LocalTools>,
    operation: LocalOperation,
}

impl ToolExecutor for LocalExecutor {
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if stop.is_cancelled() {
                return error("cancelled before tool start");
            }
            match self.operation {
                LocalOperation::Read => self.tools.read(&call.arguments),
                LocalOperation::Edit => self.tools.edit(&call.arguments),
                LocalOperation::Write => self.tools.write(&call.arguments),
                LocalOperation::Exec => self.tools.exec(&call.arguments, stop).await,
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    offset: Option<u64>,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    edits: Vec<TextEdit>,
    base_digest: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextEdit {
    old_text: String,
    new_text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    command: String,
    timeout_ms: Option<u64>,
}

impl LocalTools {
    fn read(&self, args: &Value) -> ToolOutput {
        let input: ReadInput = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(e) => return error(format!("invalid read arguments: {e}")),
        };
        let limit = input.limit.unwrap_or(16 * 1024);
        let offset = input.offset.unwrap_or(0);
        if !(1..=MAX_READ_BYTES).contains(&limit) {
            return error("read limit must be between 1 and 65536 bytes");
        }
        let path = match self.path(&input.path) {
            Ok(path) => path,
            Err(e) => return error(e),
        };
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(e) => return error(format!("read {}: {e}", input.path)),
        };
        let size = match file.metadata() {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) => return error("read target is not a regular file"),
            Err(e) => return error(e.to_string()),
        };
        let mut header = [0u8; 16];
        let header_len = match file.read(&mut header) {
            Ok(len) => len,
            Err(e) => return error(format!("read {}: {e}", input.path)),
        };
        let image_extension = path
            .extension()
            .and_then(|part| part.to_str())
            .is_some_and(|part| {
                matches!(
                    part.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp"
                )
            });
        if image_extension || ImageMime::detect(&header[..header_len]).is_some() {
            if input.offset.is_some() || input.limit.is_some() {
                return error("image read does not accept offset or limit");
            }
            if size > MAX_SOURCE_BYTES as u64 {
                return error("image exceeds 32 MiB source bound");
            }
            if let Err(e) = file.seek(SeekFrom::Start(0)) {
                return error(e.to_string());
            }
            let mut bytes = Vec::new();
            if let Err(e) = file
                .take(MAX_SOURCE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
            {
                return error(format!("read {}: {e}", input.path));
            }
            let image = match normalize_image(&bytes) {
                Ok(image) => image,
                Err(e) => return error(format!("read image {}: {e}", input.path)),
            };
            return ToolOutput {
                value: json!({"path":input.path,"content":format!("Read image file [{}]", image.content.mime_type().as_str()),"note":image.note}),
                images: vec![image.content],
                is_error: false,
            };
        }
        if offset > size {
            return error(format!(
                "offset {offset} is beyond end of file ({size} bytes)"
            ));
        }
        let (bytes, digest, file_bytes) = if size <= MAX_FILE_BYTES as u64 {
            if let Err(e) = file.seek(SeekFrom::Start(0)) {
                return error(e.to_string());
            }
            let mut all = Vec::new();
            if let Err(e) = Read::by_ref(&mut file)
                .take(MAX_FILE_BYTES as u64 + 1)
                .read_to_end(&mut all)
            {
                return error(e.to_string());
            }
            if all.len() > MAX_FILE_BYTES {
                return error("text file grew beyond the 8 MiB digest bound during read");
            }
            let file_bytes = all.len() as u64;
            if offset > file_bytes {
                return error(format!(
                    "offset {offset} is beyond end of file ({file_bytes} bytes)"
                ));
            }
            let start = offset as usize;
            let end = start.saturating_add(limit).min(all.len());
            (all[start..end].to_vec(), Some(hex_digest(&all)), file_bytes)
        } else {
            if let Err(e) = file.seek(SeekFrom::Start(offset)) {
                return error(e.to_string());
            }
            let mut bytes = Vec::new();
            if let Err(e) = file.take(limit as u64).read_to_end(&mut bytes) {
                return error(e.to_string());
            }
            (bytes, None, size)
        };
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content,
            Err(utf8_error) if utf8_error.error_len().is_none() && utf8_error.valid_up_to() > 0 => {
                std::str::from_utf8(&bytes[..utf8_error.valid_up_to()]).expect("validated prefix")
            }
            Err(utf8_error) if utf8_error.error_len().is_none() => {
                return error("read limit ends before the next UTF-8 character; increase limit");
            }
            Err(_) => return error("read range is not UTF-8; use exec for binary data"),
        };
        let next_offset = offset.saturating_add(content.len() as u64);
        success(
            json!({"path": input.path, "offset": offset, "next_offset": next_offset, "content": content, "file_bytes": file_bytes, "has_more": next_offset < file_bytes, "base_digest": digest}),
        )
    }

    fn edit(&self, args: &Value) -> ToolOutput {
        let input: EditInput = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(e) => return error(format!("invalid edit arguments: {e}")),
        };
        if input.edits.is_empty() {
            return error("edits must contain at least one replacement");
        }
        let path = match self.path(&input.path) {
            Ok(path) => path,
            Err(e) => return error(e),
        };
        let bytes = match read_file(&path) {
            Ok(bytes) => bytes,
            Err(e) => return error(format!("edit {}: {e}", input.path)),
        };
        if let Err(e) = ensure_writable(&path) {
            return error(format!("edit {}: {e}", input.path));
        }
        let actual = hex_digest(&bytes);
        if input
            .base_digest
            .as_ref()
            .is_some_and(|digest| digest != &actual)
        {
            return error("file changed since supplied base_digest");
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return error("edit target is not UTF-8"),
        };
        let replacement = match replace_text_preserving_format(&text, &input.edits) {
            Ok(replacement) => replacement,
            Err(message) => return error(message),
        };
        if replacement.len() > MAX_FILE_BYTES {
            return error("edited file exceeds 8 MiB");
        }
        let permissions = match fs::metadata(&path) {
            Ok(meta) => meta.permissions(),
            Err(e) => return error(e.to_string()),
        };
        match replace_file(&path, replacement.as_bytes(), Some(permissions)) {
            Ok(()) => success(
                json!({"path": input.path, "replacements": input.edits.len(), "base_digest": actual, "new_digest": hex_digest(replacement.as_bytes())}),
            ),
            Err(e) => error(format!("edit write failed: {e}")),
        }
    }

    fn write(&self, args: &Value) -> ToolOutput {
        let input: WriteInput = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(e) => return error(format!("invalid write arguments: {e}")),
        };
        if input.content.len() > MAX_FILE_BYTES {
            return error("content exceeds 8 MiB");
        }
        let path = match self.path(&input.path) {
            Ok(path) => path,
            Err(e) => return error(e),
        };
        let previous = match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => Some(meta.permissions()),
            Ok(_) => return error("write target is not a regular file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return error(e.to_string()),
        };
        if previous.is_some()
            && let Err(e) = ensure_writable(&path)
        {
            return error(format!("write {}: {e}", input.path));
        }
        let created = previous.is_none();
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            return error(format!("create parent: {e}"));
        }
        match replace_file(&path, input.content.as_bytes(), previous) {
            Ok(()) => success(
                json!({"path": input.path, "created": created, "bytes": input.content.len()}),
            ),
            Err(e) => error(format!("write failed: {e}")),
        }
    }

    async fn exec(&self, args: &Value, stop: CancellationToken) -> ToolOutput {
        let input: ExecInput = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(e) => return error(format!("invalid exec arguments: {e}")),
        };
        if input.command.is_empty() || input.command.contains('\0') {
            return error("invalid command");
        }
        if input.timeout_ms == Some(0) {
            return error("timeout_ms must be positive");
        }
        if stop.is_cancelled() {
            return error("cancelled before command start");
        }
        let mut command = Command::new(shell_program());
        command
            .arg("-c")
            .arg(input.command)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => return error(format!("command did not start: {e}")),
        };
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw);
        let stdout = OutputCapture::start(child.stdout.take().expect("piped"));
        let stderr = OutputCapture::start(child.stderr.take().expect("piped"));
        let mut cancelled = false;
        let mut timed_out = false;
        let deadline = async {
            if let Some(timeout) = input.timeout_ms {
                tokio::time::sleep(Duration::from_millis(timeout)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(deadline);
        let status = tokio::select! {
            status = child.wait() => status,
            () = stop.cancelled() => { cancelled = true; stop_child(&mut child, pid).await },
            () = &mut deadline => { timed_out = true; stop_child(&mut child, pid).await },
        };
        // A descendant may hold the pipes after the direct command exits.
        // Continue reading while output arrives, then release idle pipes.
        // Timeout already requested process stop too: neither stream may
        // follow an endlessly writing descendant during the final drain.
        let already_stopped = cancelled || timed_out;
        let captures = async {
            tokio::join!(
                stdout.finish(&stop, already_stopped),
                stderr.finish(&stop, already_stopped),
            )
        };
        tokio::pin!(captures);
        let ((stdout, stdout_cancelled), (stderr, stderr_cancelled)) = tokio::select! {
            result = &mut captures => result,
            () = stop.cancelled(), if !cancelled => {
                if let Some(pid) = pid {
                    let _ = kill_process_group(pid, Signal::TERM);
                }
                captures.await
            }
        };
        cancelled |= stdout_cancelled || stderr_cancelled || stop.is_cancelled();
        let (exit_code, signal, wait_error, succeeded) = match status {
            Ok(status) => (status.code(), status.signal(), None, status.success()),
            Err(error) => (
                None,
                None,
                Some(format!(
                    "command started, but direct-child exit is unknown: {error}"
                )),
                false,
            ),
        };
        let result = json!({"exit_code": exit_code, "signal": signal, "wait_error": wait_error, "stdout": String::from_utf8_lossy(&stdout.bytes), "stderr": String::from_utf8_lossy(&stderr.bytes), "stdout_truncated": !stdout.complete || stdout.omitted_bytes != Some(0), "stderr_truncated": !stderr.complete || stderr.omitted_bytes != Some(0), "stdout_omitted_bytes": stdout.omitted_bytes, "stderr_omitted_bytes": stderr.omitted_bytes, "stdout_full_path": stdout.full_path, "stderr_full_path": stderr.full_path, "stdout_full_error": stdout.full_error, "stderr_full_error": stderr.full_error, "cancelled": cancelled, "timed_out": timed_out});
        ToolOutput {
            value: result,
            images: Vec::new(),
            is_error: !succeeded || cancelled || timed_out,
        }
    }
}

fn shell_program() -> PathBuf {
    let primary = PathBuf::from("/bin/bash");
    if is_executable(&primary) {
        return primary;
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("bash");
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    PathBuf::from("/bin/sh")
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn success(value: Value) -> ToolOutput {
    ToolOutput {
        value,
        images: Vec::new(),
        is_error: false,
    }
}
fn error(message: impl Into<String>) -> ToolOutput {
    ToolOutput {
        value: json!({"error": message.into()}),
        images: Vec::new(),
        is_error: true,
    }
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn advance_raw_offset(bytes: &[u8], raw: &mut usize, normalized: &mut usize, target: usize) {
    while *normalized < target {
        if bytes[*raw] == b'\r' && bytes.get(*raw + 1) == Some(&b'\n') {
            *raw += 2;
        } else {
            *raw += 1;
        }
        *normalized += 1;
    }
}

fn line_ending(text: &str) -> &'static str {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\r' {
            return if bytes.get(index + 1) == Some(&b'\n') {
                "\r\n"
            } else {
                "\r"
            };
        }
        if *byte == b'\n' {
            return "\n";
        }
    }
    "\n"
}

fn replace_text_preserving_format(source: &str, edits: &[TextEdit]) -> Result<String, String> {
    struct Matched<'a> {
        start: usize,
        end: usize,
        replacement: &'a str,
        index: usize,
    }
    let bom_bytes = if source.starts_with('\u{feff}') { 3 } else { 0 };
    let body = &source[bom_bytes..];
    let normalized = normalize_newlines(body);
    let mut matched = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let old = normalize_newlines(&edit.old_text);
        if old.is_empty() {
            return Err(format!("edits[{index}].old_text must not be empty"));
        }
        let Some(start) = normalized.find(&old) else {
            return Err(format!("edits[{index}].old_text was not found"));
        };
        // Search after the first character, not after the whole match, so
        // overlapping occurrences are ambiguous too ("aa" in "aaa").
        let next = start
            + normalized[start..]
                .chars()
                .next()
                .expect("match is nonempty")
                .len_utf8();
        if normalized[next..].contains(&old) {
            return Err(format!("edits[{index}].old_text is ambiguous"));
        }
        matched.push(Matched {
            start,
            end: start + old.len(),
            replacement: &edit.new_text,
            index,
        });
    }
    matched.sort_unstable_by_key(|item| item.start);
    for pair in matched.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(format!(
                "edits[{}] and edits[{}] overlap; merge them into one replacement",
                pair[0].index, pair[1].index
            ));
        }
    }
    let mut result = String::with_capacity(source.len());
    let mut raw = 0;
    let mut normalized_offset = 0;
    let mut copied_through = 0;
    for item in matched {
        advance_raw_offset(
            body.as_bytes(),
            &mut raw,
            &mut normalized_offset,
            item.start,
        );
        let raw_start = bom_bytes + raw;
        advance_raw_offset(body.as_bytes(), &mut raw, &mut normalized_offset, item.end);
        let raw_end = bom_bytes + raw;
        result.push_str(&source[copied_through..raw_start]);
        let style = if source[raw_start..raw_end].contains(['\r', '\n']) {
            line_ending(&source[raw_start..raw_end])
        } else {
            line_ending(body)
        };
        result.push_str(&normalize_newlines(item.replacement).replace('\n', style));
        copied_through = raw_end;
    }
    result.push_str(&source[copied_through..]);
    if result == source {
        return Err("edit made no changes".into());
    }
    Ok(result)
}
fn read_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other("file exceeds 8 MiB"));
    }
    Ok(bytes)
}
fn ensure_writable(path: &Path) -> std::io::Result<()> {
    // Atomic rename would otherwise bypass the target file's write permission.
    // Opening without truncate checks the current host user's effective access.
    OpenOptions::new().write(true).open(path).map(|_| ())
}
fn replace_file(
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
) -> std::io::Result<()> {
    // Reading an existing symlink follows it. Keep the same target when an
    // edit or replacement is committed instead of replacing the link itself.
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    let temporary = parent.join(format!(
        ".ion-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
struct Captured {
    bytes: Vec<u8>,
    complete: bool,
    omitted_bytes: Option<u64>,
    full_path: Option<PathBuf>,
    full_error: Option<String>,
}
struct CaptureState {
    bytes: VecDeque<u8>,
    total: u64,
    complete: bool,
    full_path: Option<PathBuf>,
    full_error: Option<String>,
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        // A dropped client must not leave an unclaimed completed artifact.
        if let Some(path) = self.full_path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

struct OutputSpool {
    path: Option<PathBuf>,
    file: tokio::fs::File,
}

impl OutputSpool {
    fn open() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("ion-output-{}.log", uuid::Uuid::now_v7()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            path: Some(path),
            file: tokio::fs::File::from_std(file),
        })
    }

    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.file.write_all(bytes).await
    }

    async fn finish(mut self, complete: bool) -> std::io::Result<Option<PathBuf>> {
        // Tokio writes can return before the blocking filesystem operation
        // settles. Flush even an incomplete capture before deleting its file.
        self.file.flush().await?;
        Ok(if complete { self.path.take() } else { None })
    }
}

impl Drop for OutputSpool {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

struct OutputCapture {
    task: tokio::task::JoinHandle<CaptureState>,
    exited: CancellationToken,
    stop_acquisition: CancellationToken,
}
impl Drop for OutputCapture {
    fn drop(&mut self) {
        // Stop acquisition without aborting filesystem work in flight.
        self.stop_acquisition.cancel();
    }
}
impl OutputCapture {
    fn start(mut pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> Self {
        let exited = CancellationToken::new();
        let capture_exited = exited.clone();
        let stop_acquisition = CancellationToken::new();
        let capture_stop = stop_acquisition.clone();
        let task = tokio::spawn(async move {
            let mut state = CaptureState {
                bytes: VecDeque::with_capacity(MAX_OUTPUT_BYTES),
                total: 0,
                complete: false,
                full_path: None,
                full_error: None,
            };
            let mut chunk = [0u8; 8192];
            let mut spool: Option<OutputSpool> = None;
            let mut spool_failed = false;
            let mut post_exit = false;
            loop {
                // Idle means a pending pipe read, never a spool write/flush.
                // Stop takes priority even for endless ready output; otherwise
                // ready bytes or EOF win over a simultaneously expired idle wait.
                let read = tokio::select! {
                    biased;
                    () = capture_stop.cancelled() => break,
                    result = pipe.read(&mut chunk) => result,
                    () = capture_exited.cancelled(), if !post_exit => {
                        post_exit = true;
                        continue;
                    }
                    () = tokio::time::sleep(POST_EXIT_OUTPUT_IDLE), if post_exit => break,
                };
                match read {
                    Ok(0) => {
                        state.complete = true;
                        break;
                    }
                    Ok(n) => {
                        let prefix = (!spool_failed
                            && spool.is_none()
                            && state.total.saturating_add(n as u64) > MAX_OUTPUT_BYTES as u64)
                            .then(|| state.bytes.iter().copied().collect::<Vec<_>>());
                        state.total = state.total.saturating_add(n as u64);
                        let overflow = state
                            .bytes
                            .len()
                            .saturating_add(n)
                            .saturating_sub(MAX_OUTPUT_BYTES);
                        let remove_existing = overflow.min(state.bytes.len());
                        state.bytes.drain(..remove_existing);
                        state.bytes.extend(&chunk[overflow - remove_existing..n]);
                        if let Some(prefix) = prefix {
                            match async {
                                let mut file = OutputSpool::open()?;
                                file.write(&prefix).await?;
                                file.write(&chunk[..n]).await?;
                                Ok::<_, std::io::Error>(file)
                            }
                            .await
                            {
                                Ok(file) => spool = Some(file),
                                Err(error) => {
                                    spool_failed = true;
                                    state.full_error = Some(error.to_string());
                                }
                            }
                        } else if let Some(file) = &mut spool
                            && let Err(error) = file.write(&chunk[..n]).await
                        {
                            spool_failed = true;
                            spool = None;
                            state.full_error = Some(error.to_string());
                        }
                    }
                    Err(_) => break,
                }
            }
            // Acquisition has stopped, but every started filesystem operation
            // must settle. EOF remains true even if the artifact cannot finish.
            if let Some(spool) = spool {
                match spool.finish(state.complete).await {
                    Ok(path) => state.full_path = path,
                    Err(error) => state.full_error = Some(error.to_string()),
                }
            }
            state
        });
        Self {
            task,
            exited,
            stop_acquisition,
        }
    }

    async fn finish(mut self, stop: &CancellationToken, already_stopped: bool) -> (Captured, bool) {
        self.exited.cancel();
        // Only cancellation/timeout imposes a bounded drain regardless of
        // activity. Normal direct exit has no total post-exit cutoff.
        let stop_reading = async {
            if already_stopped {
                tokio::time::sleep(POST_EXIT_OUTPUT_IDLE).await;
            } else {
                stop.cancelled().await;
            }
        };
        let mut cancelled = false;
        let result = tokio::select! {
            biased;
            result = &mut self.task => result,
            () = stop_reading => {
                cancelled = !already_stopped;
                self.stop_acquisition.cancel();
                // Do not abort: this task may be writing or flushing, even
                // after EOF. It will stop at its next acquisition boundary.
                (&mut self.task).await
            }
        };
        let mut state = result.unwrap_or_else(|error| CaptureState {
            bytes: VecDeque::new(),
            total: 0,
            complete: false,
            full_path: None,
            full_error: Some(format!("output capture task failed: {error}")),
        });
        let bytes: Vec<u8> = std::mem::take(&mut state.bytes).into();
        let complete = state.complete;
        let omitted_bytes = complete.then(|| state.total.saturating_sub(bytes.len() as u64));
        (
            Captured {
                bytes,
                complete,
                omitted_bytes,
                full_path: state.full_path.take(),
                full_error: state.full_error.take(),
            },
            cancelled,
        )
    }
}
async fn stop_child(
    child: &mut tokio::process::Child,
    pid: Option<Pid>,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = pid {
        let _ = kill_process_group(pid, Signal::TERM);
    }
    match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            if let Some(pid) = pid {
                let _ = kill_process_group(pid, Signal::KILL);
            }
            child.kill().await?;
            child.wait().await
        }
    }
}
fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec { name: "read".into(), description: "Read UTF-8 text or a supported image (JPEG, PNG, GIF, WebP) from the live working directory. Images are attached to the result. Paths may be relative or absolute. Large text files can be read in byte ranges; use returned next_offset to continue at a UTF-8 boundary. A complete text-file digest is provided when available.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}}}) },
        ToolSpec { name: "edit".into(), description: "Apply one or more disjoint exact text replacements to a UTF-8 file in one write. Each old_text must occur exactly once in the original file; overlapping edits are rejected. Optionally reject changes since base_digest. Operates with the host user's permissions.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","edits"],"properties":{"path":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["old_text","new_text"],"properties":{"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}}}},"base_digest":{"type":"string"}}}) },
        ToolSpec { name: "write".into(), description: "Create or replace a UTF-8 file in the live working directory. Missing parent directories are created.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}) },
        ToolSpec { name: "exec".into(), description: "Run a Bash command (or POSIX sh when Bash is unavailable) in the live working directory with the host user's permissions; this is not sandboxed. Timeout is optional. Returns direct command exit and the final 64 KiB of each output stream, with omitted byte counts when truncated. For complete truncated captures, stdout_full_path and stderr_full_path name private temporary files containing the full observed streams; inspect them instead of rerunning a command. Cancellation is best effort.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1}}}) },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_preserves_existing_symlink() {
        let root = std::env::temp_dir().join(format!("ion-tools-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target.txt");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = tools.write(&json!({"path":"link.txt","content":"new"}));
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert!(
            fs::symlink_metadata(root.join("link.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_and_write_do_not_replace_read_only_files() {
        let root = std::env::temp_dir().join(format!("ion-readonly-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("file.txt");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let edit =
            tools.edit(&json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}]}));
        assert!(edit.is_error, "{}", edit.value);
        let write = tools.write(&json!({"path":"file.txt","content":"new"}));
        assert!(write.is_error, "{}", write.value);
        assert_eq!(fs::read_to_string(&path).unwrap(), "old");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_matches_lf_text_in_crlf_file_and_preserves_bom() {
        let root = std::env::temp_dir().join(format!("ion-edit-crlf-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("file.txt");
        let original = "\u{feff}one\r\ntwo\r\nthree\r\n";
        fs::write(&path, original).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = tools.edit(&json!({
            "path":"file.txt",
            "edits":[{"old_text":"two\nthree","new_text":"TWO\nthree"}],
            "base_digest": hex_digest(original.as_bytes()),
        }));
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "\u{feff}one\r\nTWO\r\nthree\r\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_digest_covers_the_whole_file_and_can_guard_edit() {
        let root = std::env::temp_dir().join(format!("ion-read-digest-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("file.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let read = tools.read(&json!({"path":"file.txt"}));
        assert!(!read.is_error, "{}", read.value);
        let digest = read.value["base_digest"].as_str().unwrap();
        assert_eq!(digest, hex_digest(&fs::read(&path).unwrap()));
        let edit = tools.edit(&json!({
            "path":"file.txt",
            "edits":[{"old_text":"beta","new_text":"BETA"}],
            "base_digest":digest,
        }));
        assert!(!edit.is_error, "{}", edit.value);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_preserves_unmatched_mixed_line_endings_and_rejects_ambiguity() {
        let result = replace_text_preserving_format(
            "first\r\nsecond\nthird\r\n",
            &[TextEdit {
                old_text: "second\nthird".into(),
                new_text: "SECOND\nthird".into(),
            }],
        )
        .unwrap();
        assert_eq!(result, "first\r\nSECOND\nthird\r\n");
        assert!(
            replace_text_preserving_format(
                "same\r\nsame\n",
                &[TextEdit {
                    old_text: "same".into(),
                    new_text: "new".into()
                }]
            )
            .is_err()
        );
    }

    #[test]
    fn multi_edit_matches_one_original_snapshot_and_rejects_partial_changes() {
        let root = std::env::temp_dir().join(format!("ion-multi-edit-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("file.txt");
        let tools = LocalTools::new(&root).unwrap();
        let original = "\u{feff}α\r\nfirst\nsecond\r\n";
        fs::write(&path, original).unwrap();
        let output = tools.edit(&json!({
            "path":"file.txt",
            "edits":[
                {"old_text":"second","new_text":"SECOND"},
                {"old_text":"α","new_text":"β"}
            ],
            "base_digest":hex_digest(original.as_bytes()),
        }));
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["replacements"], 2);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\u{feff}β\r\nfirst\nSECOND\r\n"
        );

        for (source, edits) in [
            (
                "abcdef",
                json!([{"old_text":"bcd","new_text":"B"},{"old_text":"cde","new_text":"C"}]),
            ),
            ("aaa", json!([{"old_text":"aa","new_text":"A"}])),
            (
                "abcdef",
                json!([{"old_text":"abc","new_text":"ABC"},{"old_text":"missing","new_text":"M"}]),
            ),
        ] {
            fs::write(&path, source).unwrap();
            let output = tools.edit(&json!({"path":"file.txt","edits":edits}));
            assert!(output.is_error, "{}", output.value);
            assert_eq!(fs::read_to_string(&path).unwrap(), source);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_continuation_stays_on_utf8_boundaries() {
        let root = std::env::temp_dir().join(format!("ion-read-utf8-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file.txt"), "aéZ").unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let first = tools.read(&json!({"path":"file.txt","offset":null,"limit":2}));
        assert!(!first.is_error, "{}", first.value);
        assert_eq!(first.value["content"], "a");
        assert_eq!(first.value["next_offset"], 1);
        assert_eq!(first.value["has_more"], true);
        let second = tools.read(&json!({"path":"file.txt","offset":1,"limit":2}));
        assert_eq!(second.value["content"], "é");
        assert_eq!(second.value["next_offset"], 3);
        let third = tools.read(&json!({"path":"file.txt","offset":3,"limit":2}));
        assert_eq!(third.value["content"], "Z");
        assert_eq!(third.value["has_more"], false);
        let beyond = tools.read(&json!({"path":"file.txt","offset":5}));
        assert!(beyond.is_error);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_image_returns_normalized_typed_content() {
        let root = std::env::temp_dir().join(format!("ion-read-image-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let bytes = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        fs::write(root.join("picture.png"), bytes).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = tools.read(&json!({"path":"picture.png"}));
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].mime_type().as_str(), "image/png");
        assert_eq!(output.value["path"], "picture.png");
        assert!(
            tools
                .read(&json!({"path":"picture.png","offset":1}))
                .is_error
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn turn_cancellation_waits_for_native_tool_settlement_with_or_without_warming() {
        use ion_ai::{
            Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role,
            Script, ScriptedModelService, Usage,
        };
        use ion_core::{
            AgentLimits, CodingAgent, CodingAgentError, CodingAgentEvent, CodingSession,
            PromptCacheWarmingPolicy, SessionEntry, TurnEndReason,
        };

        for warming in [false, true] {
            let root =
                std::env::temp_dir().join(format!("ion-turn-cancel-{}", uuid::Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            let path = root.join("session.sqlite");
            let session = Arc::new(CodingSession::create(&path, &root).unwrap());
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "running".into(),
                        name: "exec".into(),
                        arguments: json!({"command":"printf 'OBSERVED_BEFORE_CANCEL\\n'; printf ready > ready; exec sleep 30", "timeout_ms": 120000}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                usage: Usage {
                    input_tokens: Some(10_000),
                    cache_write_input_tokens: Some(10_000),
                    ..Usage::unknown()
                },
                termination: ResponseTermination::Completed,
                returned_model: None,
            };
            let model = Arc::new(ScriptedModelService::new([Script::Stream(vec![
                ModelStreamEvent::Completed(response),
            ])]));
            let agent = CodingAgent::new(model.clone(), Arc::new(LocalTools::new(&root).unwrap()))
                .with_limits(AgentLimits {
                    prompt_cache_warming: warming.then_some(PromptCacheWarmingPolicy {
                        lifetime_seconds: 300,
                        cache_write_microusd_per_million: 5_000_000,
                        cache_read_microusd_per_million: 100_000,
                        output_microusd_per_million: 1_000_000,
                        minimum_savings_microusd: 1,
                    }),
                    ..AgentLimits::default()
                });
            let stop = CancellationToken::new();
            let trigger = stop.clone();
            let running_session = session.clone();
            let task = tokio::spawn(async move {
                let mut events = Vec::new();
                let result = agent
                    .submit(
                        &running_session,
                        ModelRef {
                            provider: "test".into(),
                            model: "test".into(),
                        },
                        "run".into(),
                        String::new(),
                        stop,
                        |event| events.push(event),
                    )
                    .await;
                (result, events)
            });
            let ready = tokio::time::timeout(Duration::from_secs(4), async {
                while !root.join("ready").exists() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await;
            trigger.cancel(); // Also stop the task if startup failed.
            let (result, events) = tokio::time::timeout(Duration::from_secs(4), task)
                .await
                .expect("Turn cancellation did not settle")
                .unwrap();
            ready.expect("native command did not start");
            assert!(
                matches!(result, Err(CodingAgentError::Cancelled)),
                "{result:?}"
            );
            let view = session.view().unwrap();
            let output = view
                .entries
                .iter()
                .find_map(|entry| match entry {
                    SessionEntry::ToolResult { result, .. } => Some(result),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                output.result["cancelled"], true,
                "must record observed cancellation, not a synthetic unknown effect: {output:?}"
            );
            assert_eq!(output.result["stdout"], "OBSERVED_BEFORE_CANCEL\n");
            assert!(output.result["exit_code"].is_number() || output.result["signal"].is_number());
            assert!(events.iter().any(|event| matches!(event, CodingAgentEvent::ToolFinished { output, .. } if output.value["cancelled"] == true)));
            assert!(matches!(
                view.entries.last(),
                Some(SessionEntry::TurnEnded {
                    reason: TurnEndReason::Cancelled,
                    ..
                })
            ));
            assert_eq!(
                model.requests().len(),
                1,
                "no dependent request or warming after cancellation"
            );
            drop(session);
            let reopened = CodingSession::open(&path).unwrap();
            assert_eq!(reopened.view().unwrap().entries, view.entries);
            drop(reopened);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn spool_finalization_reports_delayed_write_failure_and_removes_artifact() {
        let path = std::env::temp_dir().join(format!("ion-spool-fault-{}", uuid::Uuid::now_v7()));
        fs::write(&path, b"original").unwrap();
        let mut spool = OutputSpool {
            path: Some(path.clone()),
            file: tokio::fs::File::from_std(File::open(&path).unwrap()),
        };
        // Tokio accepts this into its buffer; the actual read-only-file fault
        // is only observed when the blocking write settles during flush.
        spool.write(b"cannot write").await.unwrap();
        assert!(spool.finish(true).await.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn capture_settles_spool_io_without_treating_it_as_pipe_idle() {
        use std::{
            pin::Pin,
            sync::mpsc,
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, ReadBuf};

        struct GatedRead {
            cursor: std::io::Cursor<Vec<u8>>,
            stall_at: u64,
            release: Option<mpsc::Receiver<()>>,
            entered: Option<tokio::sync::oneshot::Sender<()>>,
            eof: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl AsyncRead for GatedRead {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if self.cursor.position() == self.stall_at
                    && let Some(release) = self.release.take()
                {
                    let (ready_tx, ready_rx) = mpsc::channel();
                    tokio::task::spawn_blocking(move || {
                        let _ = ready_tx.send(());
                        let _ = release.recv();
                    });
                    // The sole blocking thread must be occupied before the
                    // next chunk can start its real Tokio file write.
                    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    let _ = self.entered.take().unwrap().send(());
                }
                let before = buf.filled().len();
                let result = Pin::new(&mut self.cursor).poll_read(cx, buf);
                if matches!(result, Poll::Ready(Ok(())))
                    && buf.filled().len() == before
                    && let Some(eof) = self.eof.take()
                {
                    let _ = eof.send(());
                }
                result
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            for (stall_at, cancel) in [
                (65_536, false),
                (73_728, false),
                (65_536, true),
                (73_728, true),
            ] {
                let payload = [b"x\n".repeat(36_864), b"END_MARKER\n".to_vec()].concat();
                // Dropping the sender also releases the gate on any assertion
                // failure, before runtime Drop waits for blocking work.
                let (release, gate) = mpsc::channel::<()>();
                let (entered_tx, entered) = tokio::sync::oneshot::channel();
                let (eof_tx, eof) = tokio::sync::oneshot::channel();
                let capture = OutputCapture::start(GatedRead {
                    cursor: std::io::Cursor::new(payload.clone()),
                    stall_at,
                    release: Some(gate),
                    entered: Some(entered_tx),
                    eof: Some(eof_tx),
                });
                let stop = CancellationToken::new();
                let trigger = stop.clone();
                let finishing = tokio::spawn(async move { capture.finish(&stop, false).await });
                tokio::time::timeout(Duration::from_secs(2), entered)
                    .await
                    .unwrap()
                    .unwrap();
                let at_eof = stall_at == 73_728;
                if at_eof {
                    // The final write is queued behind the blocker; EOF then
                    // arrives while the real Tokio File flush is still pending.
                    tokio::time::timeout(Duration::from_secs(2), eof)
                        .await
                        .unwrap()
                        .unwrap();
                }
                if cancel {
                    trigger.cancel();
                }
                tokio::time::sleep(POST_EXIT_OUTPUT_IDLE * 3).await;
                let finished_before_io = finishing.is_finished();
                drop(release);
                let (captured, cancelled) = tokio::time::timeout(Duration::from_secs(2), finishing)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    !finished_before_io,
                    "capture returned before spool I/O settled: {stall_at}, {cancel}"
                );
                assert_eq!(cancelled, cancel);
                let complete = !cancel || at_eof;
                assert_eq!(captured.complete, complete);
                assert_eq!(captured.full_error, None);
                if complete {
                    assert_eq!(captured.bytes, payload[payload.len() - MAX_OUTPUT_BYTES..]);
                    assert_eq!(
                        captured.omitted_bytes,
                        Some((payload.len() - MAX_OUTPUT_BYTES) as u64)
                    );
                    let path = captured.full_path.unwrap();
                    assert_eq!(fs::read(&path).unwrap(), payload);
                    assert_eq!(
                        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    fs::remove_file(path).unwrap();
                } else {
                    assert_eq!(captured.omitted_bytes, None);
                    assert_eq!(captured.full_path, None);
                    assert_eq!(captured.bytes, payload[8_192..73_728]);
                }
            }
        });
    }

    #[tokio::test]
    async fn long_command_output_retains_the_failure_summary_at_the_end() {
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        let output = tools
            .execute(
                &ToolCall {
                    id: "tail".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"yes x | head -c 70000; printf 'END_MARKER\\n'; yes e | head -c 70000 >&2; printf 'ERROR_MARKER\\n' >&2"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(
            output.value["stdout_omitted_bytes"],
            70_011 - MAX_OUTPUT_BYTES
        );
        assert_eq!(
            output.value["stderr_omitted_bytes"],
            70_013 - MAX_OUTPUT_BYTES
        );
        assert_eq!(output.value["stderr_truncated"], true);
        let expected_stdout = [b"x\n".repeat(35_000), b"END_MARKER\n".to_vec()].concat();
        let expected_stderr = [b"e\n".repeat(35_000), b"ERROR_MARKER\n".to_vec()].concat();
        assert_eq!(
            output.value["stdout"].as_str().unwrap().as_bytes(),
            &expected_stdout[expected_stdout.len() - MAX_OUTPUT_BYTES..]
        );
        assert_eq!(
            output.value["stderr"].as_str().unwrap().as_bytes(),
            &expected_stderr[expected_stderr.len() - MAX_OUTPUT_BYTES..]
        );
        let stdout_path = PathBuf::from(output.value["stdout_full_path"].as_str().unwrap());
        let stderr_path = PathBuf::from(output.value["stderr_full_path"].as_str().unwrap());
        let stdout = fs::read(&stdout_path).unwrap();
        let stderr = fs::read(&stderr_path).unwrap();
        assert_eq!(stdout, expected_stdout);
        assert_eq!(stderr, expected_stderr);
        assert_eq!(
            fs::metadata(&stdout_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&stderr_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(stdout_path).unwrap();
        fs::remove_file(stderr_path).unwrap();
    }

    #[tokio::test]
    async fn direct_exit_keeps_output_when_descendant_holds_pipe_open() {
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        let output = tools
            .execute(
                &ToolCall {
                    id: "inherited-pipe".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"sleep 3 & printf 'DIRECT_EXIT_MARKER\\n'"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["exit_code"], 0);
        assert_eq!(output.value["stdout"], "DIRECT_EXIT_MARKER\n");
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
    }

    #[tokio::test]
    async fn incomplete_long_capture_does_not_claim_a_full_output_file() {
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        let output = tools
            .execute(
                &ToolCall {
                    id: "incomplete-spool".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"sleep 2 & yes x | head -c 70000"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
        assert_eq!(output.value["stdout_full_path"], Value::Null);
    }

    #[tokio::test]
    async fn post_exit_output_activity_keeps_the_pipe_open_until_idle() {
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        let output = tools
            .execute(
                &ToolCall {
                    id: "active-pipe".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"(n=1; while [ \"$n\" -le 24 ]; do sleep 0.05; printf 'CHUNK_%s\\n' \"$n\"; n=$((n+1)); done) &"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        let expected = (1..=24).map(|n| format!("CHUNK_{n}\n")).collect::<String>();
        assert_eq!(output.value["stdout"], expected);
        assert_eq!(output.value["stdout_truncated"], false);
    }

    #[tokio::test]
    async fn cancellation_stops_post_exit_capture() {
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        let stop = CancellationToken::new();
        let trigger = stop.clone();
        let task = tokio::spawn(async move {
            tools
                .execute(
                    &ToolCall {
                        id: "cancel-active-pipe".into(),
                        name: "exec".into(),
                        arguments: json!({"command":"(while :; do printf x; sleep 0.05; done) &"}),
                        raw_arguments: None,
                    },
                    stop,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(250)).await;
        trigger.cancel();
        let output = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("post-exit capture ignored cancellation")
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.value["cancelled"], true);
        assert_eq!(output.value["stdout_truncated"], true);
        assert!(!output.value["stdout"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_capture_does_not_follow_endless_output() {
        use tokio::io::AsyncWriteExt;

        let (reader, mut writer) = tokio::io::duplex(64);
        let writer_task = tokio::spawn(async move {
            loop {
                if writer.write_all(b"x").await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let stop = CancellationToken::new();
        stop.cancel();
        let output = tokio::time::timeout(
            Duration::from_millis(500),
            OutputCapture::start(reader).finish(&stop, true),
        )
        .await;
        writer_task.abort();
        let _ = writer_task.await;
        let (captured, _) = output.expect("cancelled capture followed endless output");
        assert!(!captured.complete);
        assert!(!captured.bytes.is_empty());
    }

    #[tokio::test]
    async fn stopped_capture_bounds_continuously_ready_output() {
        let (captured, cancelled) = tokio::time::timeout(
            Duration::from_secs(2),
            OutputCapture::start(tokio::io::repeat(b'x')).finish(&CancellationToken::new(), true),
        )
        .await
        .expect("stopped capture followed continuously ready output");
        assert!(
            !cancelled,
            "timeout-style drain must not invent cancellation"
        );
        assert!(!captured.complete);
        assert_eq!(captured.bytes, vec![b'x'; MAX_OUTPUT_BYTES]);
        assert_eq!(captured.omitted_bytes, None);
        assert_eq!(captured.full_path, None);
    }

    #[tokio::test]
    async fn shell_timeout_bounds_output_from_a_surviving_descendant() {
        let root = std::env::temp_dir().join(format!("ion-timeout-drain-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(3),
            tools.exec(
                &json!({
                    "command": "(trap '' TERM; printf ready > ready; while :; do printf x || exit; sleep 0.01; done) & exec sleep 30",
                    "timeout_ms": 500,
                }),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("timed-out command followed descendant output");
        assert!(root.join("ready").exists());
        assert!(output.is_error);
        assert_eq!(output.value["timed_out"], true);
        assert_eq!(output.value["cancelled"], false);
        assert!(!output.value["stdout"].as_str().unwrap().is_empty());
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shell_timeout_is_opt_in_and_accepts_long_explicit_limits() {
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        for args in [
            json!({"command":"printf ok"}),
            json!({"command":"printf ok","timeout_ms":600000}),
        ] {
            let output = tools
                .execute(
                    &ToolCall {
                        id: "timeout".into(),
                        name: "exec".into(),
                        arguments: args,
                        raw_arguments: None,
                    },
                    CancellationToken::new(),
                )
                .await;
            assert!(!output.is_error, "{}", output.value);
            assert_eq!(output.value["stdout"], "ok");
        }
        assert!(
            specs()
                .iter()
                .find(|spec| spec.name == "exec")
                .unwrap()
                .input_schema["properties"]["timeout_ms"]
                .get("maximum")
                .is_none()
        );
    }

    #[tokio::test]
    async fn shell_uses_bash_when_available() {
        if !Path::new("/bin/bash").is_file() {
            return;
        }
        let tools = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
        ])
        .snapshot();
        let output = tools
            .execute(
                &ToolCall {
                    id: "bash".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"set -o pipefail; false | true"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert_eq!(output.value["exit_code"], 1);
        assert!(output.is_error);
    }
}

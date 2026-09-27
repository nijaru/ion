//! Direct coding tools with the caller's host permissions.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use rustix::process::{Pid, Signal, kill_process_group};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

use crate::agent::{ToolHost, ToolOutput};

const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

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
}

impl ToolHost for LocalTools {
    fn specs(&self) -> Vec<ToolSpec> {
        specs()
    }
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if stop.is_cancelled() {
                return error("cancelled before tool start");
            }
            match call.name.as_str() {
                "read" => self.read(&call.arguments),
                "edit" => self.edit(&call.arguments),
                "write" => self.write(&call.arguments),
                "exec" => self.exec(&call.arguments, stop).await,
                _ => error(format!("unknown tool: {}", call.name)),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default)]
    offset: u64,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    old_text: String,
    new_text: String,
    base_digest: Option<String>,
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
        let digest = if size <= MAX_FILE_BYTES as u64 {
            let mut all = Vec::new();
            if let Err(e) = file.read_to_end(&mut all) {
                return error(e.to_string());
            }
            Some(hex_digest(&all))
        } else {
            None
        };
        if let Err(e) = file.seek(SeekFrom::Start(input.offset)) {
            return error(e.to_string());
        }
        let mut bytes = Vec::new();
        if let Err(e) = file.take(limit as u64).read_to_end(&mut bytes) {
            return error(e.to_string());
        }
        match String::from_utf8(bytes) {
            Ok(content) => success(
                json!({"path": input.path, "offset": input.offset, "content": content, "file_bytes": size, "has_more": input.offset.saturating_add(content.len() as u64) < size, "base_digest": digest}),
            ),
            Err(_) => error("read range is not UTF-8; use exec for binary data"),
        }
    }

    fn edit(&self, args: &Value) -> ToolOutput {
        let input: EditInput = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(e) => return error(format!("invalid edit arguments: {e}")),
        };
        if input.old_text.is_empty() {
            return error("old_text must not be empty");
        }
        let path = match self.path(&input.path) {
            Ok(path) => path,
            Err(e) => return error(e),
        };
        let bytes = match read_file(&path) {
            Ok(bytes) => bytes,
            Err(e) => return error(format!("edit {}: {e}", input.path)),
        };
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
        if text.matches(&input.old_text).count() != 1 {
            return error("old_text must occur exactly once");
        }
        let replacement = text.replacen(&input.old_text, &input.new_text, 1);
        if replacement.len() > MAX_FILE_BYTES {
            return error("edited file exceeds 8 MiB");
        }
        let permissions = match fs::metadata(&path) {
            Ok(meta) => meta.permissions(),
            Err(e) => return error(e.to_string()),
        };
        match replace_file(&path, replacement.as_bytes(), Some(permissions)) {
            Ok(()) => success(
                json!({"path": input.path, "replacements": 1, "base_digest": actual, "new_digest": hex_digest(replacement.as_bytes())}),
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
        if input.command.is_empty() || input.command.contains('\0') || input.command.len() > 8192 {
            return error("invalid command");
        }
        let timeout = input.timeout_ms.unwrap_or(30_000);
        if !(1..=120_000).contains(&timeout) {
            return error("timeout_ms must be between 1 and 120000");
        }
        if stop.is_cancelled() {
            return error("cancelled before command start");
        }
        let mut command = Command::new("/bin/sh");
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
        let out = tokio::spawn(capture(child.stdout.take().expect("piped")));
        let err = tokio::spawn(capture(child.stderr.take().expect("piped")));
        let mut cancelled = false;
        let mut timed_out = false;
        let status = tokio::select! {
            status = child.wait() => status,
            () = stop.cancelled() => { cancelled = true; stop_child(&mut child, pid).await },
            () = tokio::time::sleep(Duration::from_millis(timeout)) => { timed_out = true; stop_child(&mut child, pid).await },
        };
        // A detached descendant can retain stdout/stderr after the direct
        // command exits. Do not let that pipe keep this Turn open forever.
        let stdout = finish_capture(out).await;
        let stderr = finish_capture(err).await;
        match status {
            Ok(status) => {
                let result = json!({"exit_code": status.code(), "signal": status.signal(), "stdout": String::from_utf8_lossy(&stdout.bytes), "stderr": String::from_utf8_lossy(&stderr.bytes), "stdout_truncated": !stdout.complete, "stderr_truncated": !stderr.complete, "cancelled": cancelled, "timed_out": timed_out});
                ToolOutput {
                    value: result,
                    is_error: !status.success() || cancelled || timed_out,
                }
            }
            Err(e) => error(format!(
                "command started, but direct-child exit is unknown: {e}"
            )),
        }
    }
}

fn success(value: Value) -> ToolOutput {
    ToolOutput {
        value,
        is_error: false,
    }
}
fn error(message: impl Into<String>) -> ToolOutput {
    ToolOutput {
        value: json!({"error": message.into()}),
        is_error: true,
    }
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
}
impl Captured {
    fn lost() -> Self {
        Self {
            bytes: Vec::new(),
            complete: false,
        }
    }
}
async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Captured {
    let mut bytes = Vec::new();
    let mut complete = true;
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let keep = n.min(MAX_OUTPUT_BYTES.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
                if keep < n {
                    complete = false;
                }
            }
            Err(_) => {
                complete = false;
                break;
            }
        }
    }
    Captured { bytes, complete }
}
async fn finish_capture(mut task: tokio::task::JoinHandle<Captured>) -> Captured {
    match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
        Ok(Ok(captured)) => captured,
        _ => {
            task.abort();
            Captured::lost()
        }
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
        ToolSpec { name: "read".into(), description: "Read UTF-8 file content from the live working directory. Paths may be relative or absolute. Large files can be read in ranges; a complete file digest is provided when available.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}}}) },
        ToolSpec { name: "edit".into(), description: "Replace one exact occurrence of old_text in a UTF-8 file; optionally reject changes since base_digest. Operates with the host user's permissions.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","old_text","new_text"],"properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"base_digest":{"type":"string"}}}) },
        ToolSpec { name: "write".into(), description: "Create or replace a UTF-8 file in the live working directory. Missing parent directories are created.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}) },
        ToolSpec { name: "exec".into(), description: "Run a native shell command in the live working directory with the host user's permissions; this is not sandboxed. Returns direct command exit, output and truncation. Cancellation is best effort.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1,"maximum":120000}}}) },
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

    #[tokio::test]
    async fn cancelled_command_reports_observed_exit_without_waiting_for_timeout() {
        let tools = LocalTools::new(std::env::temp_dir()).unwrap();
        let stop = CancellationToken::new();
        let trigger = stop.clone();
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(async move {
            tools
                .execute(
                    &ToolCall {
                        id: "test".into(),
                        name: "exec".into(),
                        arguments: json!({"command":"sleep 10","timeout_ms":120000}),
                    },
                    stop,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
        let output = tokio::time::timeout(Duration::from_secs(4), task)
            .await
            .expect("command cancellation hung")
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.value["cancelled"], true);
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}

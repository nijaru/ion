//! Direct coding tools with the caller's host permissions.
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
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
    offset: Option<u64>,
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
        if offset > size {
            return error(format!(
                "offset {offset} is beyond end of file ({size} bytes)"
            ));
        }
        let digest = if size <= MAX_FILE_BYTES as u64 {
            let mut all = Vec::new();
            if let Err(e) = file.read_to_end(&mut all) {
                return error(e.to_string());
            }
            Some(hex_digest(&all))
        } else {
            None
        };
        if let Err(e) = file.seek(SeekFrom::Start(offset)) {
            return error(e.to_string());
        }
        let mut bytes = Vec::new();
        if let Err(e) = file.take(limit as u64).read_to_end(&mut bytes) {
            return error(e.to_string());
        }
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
            json!({"path": input.path, "offset": offset, "next_offset": next_offset, "content": content, "file_bytes": size, "has_more": next_offset < size, "base_digest": digest}),
        )
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
        let replacement =
            match replace_text_preserving_format(&text, &input.old_text, &input.new_text) {
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
        let out = tokio::spawn(capture(child.stdout.take().expect("piped")));
        let err = tokio::spawn(capture(child.stderr.take().expect("piped")));
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
        // A detached descendant can retain stdout/stderr after the direct
        // command exits. Do not let that pipe keep this Turn open forever.
        let stdout = finish_capture(out).await;
        let stderr = finish_capture(err).await;
        match status {
            Ok(status) => {
                let result = json!({"exit_code": status.code(), "signal": status.signal(), "stdout": String::from_utf8_lossy(&stdout.bytes), "stderr": String::from_utf8_lossy(&stderr.bytes), "stdout_truncated": !stdout.complete || stdout.omitted_bytes != Some(0), "stderr_truncated": !stderr.complete || stderr.omitted_bytes != Some(0), "stdout_omitted_bytes": stdout.omitted_bytes, "stderr_omitted_bytes": stderr.omitted_bytes, "cancelled": cancelled, "timed_out": timed_out});
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

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn raw_offset_for_normalized(text: &str, normalized_offset: usize) -> usize {
    let bytes = text.as_bytes();
    let mut raw = 0;
    let mut normalized = 0;
    while normalized < normalized_offset {
        if bytes[raw] == b'\r' && bytes.get(raw + 1) == Some(&b'\n') {
            raw += 2;
        } else {
            raw += 1;
        }
        normalized += 1;
    }
    raw
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

fn replace_text_preserving_format(
    source: &str,
    old_text: &str,
    new_text: &str,
) -> Result<String, &'static str> {
    let bom_bytes = if source.starts_with('\u{feff}') { 3 } else { 0 };
    let body = &source[bom_bytes..];
    let normalized = normalize_newlines(body);
    let old = normalize_newlines(old_text);
    let mut matches = normalized.match_indices(&old);
    let Some((start, _)) = matches.next() else {
        return Err("old_text must occur exactly once");
    };
    if matches.next().is_some() {
        return Err("old_text must occur exactly once");
    }
    let raw_start = bom_bytes + raw_offset_for_normalized(body, start);
    let raw_end = bom_bytes + raw_offset_for_normalized(body, start + old.len());
    let style = if source[raw_start..raw_end].contains(['\r', '\n']) {
        line_ending(&source[raw_start..raw_end])
    } else {
        line_ending(body)
    };
    let replacement = normalize_newlines(new_text).replace('\n', style);
    let mut result =
        String::with_capacity(source.len() - (raw_end - raw_start) + replacement.len());
    result.push_str(&source[..raw_start]);
    result.push_str(&replacement);
    result.push_str(&source[raw_end..]);
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
}
impl Captured {
    fn lost() -> Self {
        Self {
            bytes: Vec::new(),
            complete: false,
            omitted_bytes: None,
        }
    }
}
async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Captured {
    let mut bytes = VecDeque::with_capacity(MAX_OUTPUT_BYTES);
    let mut total = 0u64;
    let mut complete = true;
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                total = total.saturating_add(n as u64);
                let overflow = bytes
                    .len()
                    .saturating_add(n)
                    .saturating_sub(MAX_OUTPUT_BYTES);
                let remove_existing = overflow.min(bytes.len());
                bytes.drain(..remove_existing);
                bytes.extend(&chunk[overflow - remove_existing..n]);
            }
            Err(_) => {
                complete = false;
                break;
            }
        }
    }
    let bytes: Vec<u8> = bytes.into();
    Captured {
        omitted_bytes: complete.then(|| total.saturating_sub(bytes.len() as u64)),
        bytes,
        complete,
    }
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
        ToolSpec { name: "read".into(), description: "Read UTF-8 file content from the live working directory. Paths may be relative or absolute. Large files can be read in byte ranges; use returned next_offset to continue at a UTF-8 boundary. A complete file digest is provided when available.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}}}) },
        ToolSpec { name: "edit".into(), description: "Replace one exact occurrence of old_text in a UTF-8 file; optionally reject changes since base_digest. Operates with the host user's permissions.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","old_text","new_text"],"properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"base_digest":{"type":"string"}}}) },
        ToolSpec { name: "write".into(), description: "Create or replace a UTF-8 file in the live working directory. Missing parent directories are created.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}) },
        ToolSpec { name: "exec".into(), description: "Run a Bash command (or POSIX sh when Bash is unavailable) in the live working directory with the host user's permissions; this is not sandboxed. Timeout is optional. Returns direct command exit and the final 64 KiB of each output stream, with omitted byte counts when truncated. Cancellation is best effort.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1}}}) },
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
        let edit = tools.edit(&json!({"path":"file.txt","old_text":"old","new_text":"new"}));
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
            "old_text":"two\nthree",
            "new_text":"TWO\nthree",
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
    fn edit_preserves_unmatched_mixed_line_endings_and_rejects_ambiguity() {
        let result = replace_text_preserving_format(
            "first\r\nsecond\nthird\r\n",
            "second\nthird",
            "SECOND\nthird",
        )
        .unwrap();
        assert_eq!(result, "first\r\nSECOND\nthird\r\n");
        assert!(replace_text_preserving_format("same\r\nsame\n", "same", "new").is_err());
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
                        raw_arguments: None,
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

    #[tokio::test]
    async fn long_command_output_retains_the_failure_summary_at_the_end() {
        let tools = LocalTools::new(std::env::temp_dir()).unwrap();
        let output = tools
            .execute(
                &ToolCall {
                    id: "tail".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"yes x | head -c 70000; printf 'END_MARKER\\n'"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["stdout_truncated"], true);
        assert!(output.value["stdout_omitted_bytes"].as_u64().unwrap() > 0);
        assert!(
            output.value["stdout"]
                .as_str()
                .unwrap()
                .ends_with("END_MARKER\n")
        );
    }

    #[tokio::test]
    async fn shell_timeout_is_opt_in_and_accepts_long_explicit_limits() {
        let tools = LocalTools::new(std::env::temp_dir()).unwrap();
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
        let tools = LocalTools::new(std::env::temp_dir()).unwrap();
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

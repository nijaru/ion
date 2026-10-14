//! Direct coding tools with the caller's host permissions.
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use ion_ai::{BoxFuture, ImageMime, MAX_SOURCE_BYTES, ToolCall, ToolSpec, normalize_image};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use ion_core::{
    CodingToolOutput as ToolOutput, CodingToolSource as ToolSource, ToolActivityKind,
    ToolDefinition, ToolExecutor, ToolExposure, ToolPresentation, ToolRegistration,
};

mod command;
mod text_read;

const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

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

    /// Run a user's direct command through the native shell execution path.
    pub async fn run_user_shell(&self, command: &str, stop: CancellationToken) -> ToolOutput {
        command::run(&self.cwd, &json!({"command": command}), stop).await
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

#[derive(Clone, Copy)]
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
            if matches!(self.operation, LocalOperation::Exec) {
                return command::run(&self.tools.cwd, &call.arguments, stop).await;
            }
            let operation = self.operation;
            let tools = self.tools.clone();
            let arguments = call.arguments.clone();
            // Filesystem calls, hashing and image normalization must not block
            // the coding operation's async poll. Retain the worker through
            // settlement; cancellation cannot interrupt an in-flight syscall.
            tokio::task::spawn_blocking(move || {
                if stop.is_cancelled() {
                    error("cancelled before tool start")
                } else {
                    match operation {
                        LocalOperation::Read => tools.read(&arguments, &stop),
                        LocalOperation::Edit => tools.edit(&arguments, &stop),
                        LocalOperation::Write => tools.write(&arguments, &stop),
                        LocalOperation::Exec => unreachable!("shell uses async execution"),
                    }
                }
            })
            .await
            .unwrap_or_else(|failure| {
                // A panic is not an observed tool failure: prior effects may be
                // unknown. Preserve the operation's existing panic/recovery path.
                if failure.is_panic() {
                    std::panic::resume_unwind(failure.into_panic());
                }
                panic!("native file worker stopped without an observation: {failure}");
            })
        })
    }
}

// Omission selects defaults; a present field must have its declared type.
// Option's usual null-as-None decoding would silently discard invalid inputs.
fn present_input<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default, deserialize_with = "present_input")]
    offset: Option<u64>,
    #[serde(default, deserialize_with = "present_input")]
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    edits: Vec<TextEdit>,
    #[serde(default, deserialize_with = "present_input")]
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
    #[serde(default, deserialize_with = "present_input")]
    base_digest: Option<String>,
}

impl LocalTools {
    fn read(&self, args: &Value, stop: &CancellationToken) -> ToolOutput {
        let input: ReadInput = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(e) => return error(format!("invalid read arguments: {e}")),
        };
        let limit = input.limit.unwrap_or(text_read::MAX_LINES);
        let offset = input.offset.unwrap_or(1);
        if offset == 0 {
            return error("read offset must be a one-based line number");
        }
        if !(1..=text_read::MAX_LINES).contains(&limit) {
            return error("read limit must be between 1 and 2000 lines");
        }
        let path = match self.path(&input.path) {
            Ok(path) => path,
            Err(e) => return error(e),
        };
        let mut file = match crate::file_io::open_regular(&path) {
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
        match text_read::read(file, size, &input.path, offset, limit, stop) {
            Ok(value) => success(value),
            Err(message) => error(message),
        }
    }

    fn edit(&self, args: &Value, stop: &CancellationToken) -> ToolOutput {
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
        let mutation = match crate::file_io::FileMutation::open(&path, false, stop) {
            Ok(mutation) => mutation,
            Err(e) => return error(format!("edit {}: {e}", input.path)),
        };
        let bytes = match crate::file_io::read_bounded(mutation.path(), MAX_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(e) => return error(format!("edit {}: {e}", input.path)),
        };
        if let Err(e) = mutation.ensure_writable() {
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
        let permissions = match fs::metadata(mutation.path()) {
            Ok(meta) => meta.permissions(),
            Err(e) => return error(e.to_string()),
        };
        let diff = crate::edit_diff::capture(&input.path, &text, &replacement);
        match mutation.replace(replacement.as_bytes(), Some(permissions)) {
            Ok(()) => success(
                json!({"path": input.path, "replacements": input.edits.len(), "base_digest": actual, "new_digest": hex_digest(replacement.as_bytes()), "diff": diff}),
            ),
            Err(e) => error(format!("edit write failed: {e}")),
        }
    }

    fn write(&self, args: &Value, stop: &CancellationToken) -> ToolOutput {
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
        let mutation =
            match crate::file_io::FileMutation::open(&path, input.base_digest.is_none(), stop) {
                Ok(mutation) => mutation,
                Err(e) => return error(format!("write {}: {e}", input.path)),
            };
        let previous = match fs::metadata(mutation.path()) {
            Ok(meta) if meta.is_file() => Some(meta.permissions()),
            Ok(_) => return error("write target is not a regular file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return error(e.to_string()),
        };
        if let Some(expected) = &input.base_digest {
            if previous.is_none() {
                return error("guarded write requires an existing file");
            }
            let bytes = match crate::file_io::read_bounded(mutation.path(), MAX_FILE_BYTES) {
                Ok(bytes) => bytes,
                Err(e) => return error(format!("read guarded write target: {e}")),
            };
            if expected != &hex_digest(&bytes) {
                return error("file changed since supplied base_digest");
            }
        }
        if previous.is_some()
            && let Err(e) = mutation.ensure_writable()
        {
            return error(format!("write {}: {e}", input.path));
        }
        let created = previous.is_none();
        match mutation.replace(input.content.as_bytes(), previous) {
            Ok(()) => success(
                json!({"path": input.path, "created": created, "bytes": input.content.len(), "new_digest": hex_digest(input.content.as_bytes())}),
            ),
            Err(e) => error(format!("write failed: {e}")),
        }
    }
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
fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec { name: "read".into(), description: "Read UTF-8 text or a supported image (JPEG, PNG, GIF, WebP) from the live working directory. Images are attached to the result. Paths may be relative or absolute. Text reads use one-based line ranges (default 2000 lines), bounded to 65536 bytes of complete lines. Check has_more and pass next_line as offset to continue. An oversized first line requires an explicitly bounded exec read. base_digest covers the complete file when available, not just returned content. Do not recreate an existing file from an incomplete read; prefer exact edits or read all pages before full replacement.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":1,"description":"One-based starting line (default 1). Pass the prior result's next_line to continue. Omit for images."},"limit":{"type":"integer","minimum":1,"maximum":2000,"description":"Maximum complete lines to return (default and maximum 2000); the 65536-byte output bound may stop earlier. Omit for images."}}}) },
        ToolSpec { name: "edit".into(), description: "Apply one or more disjoint exact text replacements to a UTF-8 file in one write. Each old_text must occur exactly once in the original file; overlapping edits are rejected. Optionally reject changes since base_digest. Operates with the host user's permissions.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","edits"],"properties":{"path":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["old_text","new_text"],"properties":{"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}}}},"base_digest":{"type":"string"}}}) },
        ToolSpec { name: "write".into(), description: "Create or replace a UTF-8 file in the live working directory. Omitted base_digest permits unconditional replacement and creation of missing parents. A supplied string must match the entire existing regular file under the native mutation lock; stale, missing or unreadable targets reject without replacement or parent creation. Use exact edit or complete-source guarded replacement, not an incomplete read window. Success returns path, created, bytes and new_digest (SHA256 of published content), not a lease against later changes.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"},"base_digest":{"type":"string"}}}) },
        ToolSpec { name: "exec".into(), description: "Run a Bash command (or POSIX sh when Bash is unavailable) in the live working directory with the host user's permissions; this is not sandboxed. Timeout is optional. Returns direct command exit and the final 64 KiB of each output stream, with omitted byte counts when truncated. For complete truncated captures, stdout_full_path and stderr_full_path name private temporary files containing the full observed streams; inspect them instead of rerunning a command. Cancellation is best effort.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1}}}) },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, os::unix::fs::PermissionsExt, time::Duration};

    #[tokio::test]
    async fn optional_native_inputs_reject_null_before_effects() {
        let root =
            std::env::temp_dir().join(format!("ion-input-boundary-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file.txt"), "old").unwrap();
        let catalog = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>
        ])
        .snapshot();
        let mut failures = Vec::new();
        for (name, field, mut arguments) in [
            ("read", "offset", json!({"path":"file.txt"})),
            ("read", "limit", json!({"path":"file.txt"})),
            (
                "edit",
                "base_digest",
                json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}]}),
            ),
            (
                "exec",
                "timeout_ms",
                json!({"command":"printf effect > marker"}),
            ),
        ] {
            arguments[field] = Value::Null;
            let output = catalog
                .execute(
                    &ToolCall {
                        id: "boundary".into(),
                        name: name.into(),
                        arguments,
                        raw_arguments: None,
                    },
                    CancellationToken::new(),
                )
                .await;
            let unchanged = fs::read(root.join("file.txt")).unwrap() == b"old";
            let no_marker = !root.join("marker").exists();
            if !output.is_error || !unchanged || !no_marker {
                failures.push(format!(
                    "{name}.{field}: {} unchanged={unchanged} no_marker={no_marker}",
                    output.value
                ));
            }
            // Isolate every probe even on the original, effectful defect.
            fs::write(root.join("file.txt"), "old").unwrap();
            if !no_marker {
                fs::remove_file(root.join("marker")).unwrap();
            }
        }
        fs::remove_dir_all(root).unwrap();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn native_schema_keeps_exec_timeout_optional_and_uncapped() {
        // The frozen provider schema is where the optional-and-uncapped
        // timeout contract is declared. Runtime long-timeout behavior belongs
        // to the command owner; assert only the declaration here.
        let catalog =
            ion_core::ToolSet::new([
                Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>
            ])
            .snapshot();
        let specs = catalog.specs();
        let exec = specs
            .iter()
            .find(|spec| spec.name == "exec")
            .expect("exec is registered");
        assert_eq!(exec.input_schema["required"], json!(["command"]));
        assert_eq!(exec.input_schema["additionalProperties"], false);
        assert_eq!(
            exec.input_schema["properties"]["timeout_ms"]["type"],
            "integer"
        );
        assert_eq!(exec.input_schema["properties"]["timeout_ms"]["minimum"], 1);
        // No upper bound: the declaration accepts arbitrarily long positive
        // limits rather than inventing a maximum.
        assert_eq!(
            exec.input_schema["properties"]["timeout_ms"].get("maximum"),
            None
        );
    }

    #[test]
    fn guarded_write_checks_whole_file_without_creating_missing_targets() {
        let root = std::env::temp_dir().join(format!("ion-write-guard-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let stop = CancellationToken::new();
        let source = "αβ🦀\n".repeat(4000);
        fs::write(root.join("file"), &source).unwrap();
        let read = tools.read(&json!({"path":"file","limit":1}), &CancellationToken::new());
        assert_eq!(read.value["has_more"], true);
        assert_eq!(read.value["base_digest"], hex_digest(source.as_bytes()));
        let written = tools.write(
            &json!({"path":"file","content":"first","base_digest":read.value["base_digest"]}),
            &stop,
        );
        assert!(!written.is_error, "{}", written.value);
        assert_eq!(written.value["created"], false);
        assert_eq!(written.value["new_digest"], hex_digest(b"first"));
        let stale = tools.write(
            &json!({"path":"file","content":"lost","base_digest":read.value["base_digest"]}),
            &stop,
        );
        assert!(stale.is_error);
        assert_eq!(
            stale.value["error"],
            "file changed since supplied base_digest"
        );
        assert_eq!(fs::read(root.join("file")).unwrap(), b"first");
        for guard in [Value::Null, json!(42), json!(hex_digest(b""))] {
            let rejected = tools.write(
                &json!({"path":"missing/parents/file","content":"lost","base_digest":guard}),
                &stop,
            );
            assert!(rejected.is_error, "{}", rejected.value);
            assert!(!root.join("missing").exists());
        }
        let missing = tools.write(
            &json!({"path":"absent","content":"lost","base_digest":hex_digest(b"")}),
            &stop,
        );
        assert!(missing.is_error && !root.join("absent").exists());
        let oversized = vec![b'x'; MAX_FILE_BYTES + 1];
        fs::write(root.join("large"), &oversized).unwrap();
        assert!(
            tools
                .write(
                    &json!({"path":"large","content":"lost","base_digest":hex_digest(&oversized)}),
                    &stop,
                )
                .is_error
        );
        assert_eq!(fs::read(root.join("large")).unwrap(), oversized);
        let unconditional = tools.write(&json!({"path":"large","content":"second"}), &stop);
        assert!(!unconditional.is_error);
        assert_eq!(unconditional.value["new_digest"], hex_digest(b"second"));
        let created = tools.write(
            &json!({"path":"new/parents/file","content":"created"}),
            &stop,
        );
        assert!(!created.is_error && created.value["created"] == true);
        assert_eq!(created.value["new_digest"], hex_digest(b"created"));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_guarded_writes_admit_one_current_digest() {
        let root = std::env::temp_dir().join(format!("ion-write-race-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file"), "seed").unwrap();
        std::os::unix::fs::symlink("file", root.join("alias")).unwrap();
        let catalogs = [0, 1].map(|_| {
            ion_core::ToolSet::new([
                Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>
            ])
            .snapshot()
        });
        let calls = [("file", "first"), ("alias", "second")].map(|(path, content)| ToolCall {
            id: content.into(),
            name: "write".into(),
            raw_arguments: None,
            arguments: json!({"path":path,"content":content,"base_digest":hex_digest(b"seed")}),
        });
        let (first, second) = tokio::join!(
            catalogs[0].execute(&calls[0], CancellationToken::new()),
            catalogs[1].execute(&calls[1], CancellationToken::new()),
        );
        assert_ne!(first.is_error, second.is_error, "{first:?} {second:?}");
        let (winner, loser) = if first.is_error {
            (&second, &first)
        } else {
            (&first, &second)
        };
        assert_eq!(
            loser.value["error"],
            "file changed since supplied base_digest"
        );
        let bytes = fs::read(root.join("file")).unwrap();
        assert_eq!(winner.value["new_digest"], hex_digest(&bytes));
        assert_eq!(
            bytes,
            if first.is_error {
                b"second".as_slice()
            } else {
                b"first".as_slice()
            }
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_native_edits_preserve_guards_and_disjoint_changes() {
        let root = std::env::temp_dir().join(format!("ion-edit-race-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("aliases")).unwrap();
        std::os::unix::fs::symlink("../file", root.join("aliases/link")).unwrap();
        let source = format!("LEFT:\n{}RIGHT:\n", "payload0123456789\n".repeat(200_000));
        let catalogs = [0, 1].map(|_| {
            ion_core::ToolSet::new([
                Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>
            ])
            .snapshot()
        });
        let mut failures = Vec::new();
        for guarded in [true, false] {
            fs::write(root.join("file"), &source).unwrap();
            let calls = [("file", "LEFT", "1"), ("aliases/link", "RIGHT", "2")].map(|(path, anchor, value)| {
                let mut arguments = json!({"path":path,"edits":[{"old_text":format!("{anchor}:"),"new_text":format!("{anchor}:{value}")}]});
                if guarded { arguments["base_digest"] = json!(hex_digest(source.as_bytes())); }
                ToolCall { id: anchor.into(), name: "edit".into(), arguments, raw_arguments: None }
            });
            let outputs = tokio::join!(
                catalogs[0].execute(&calls[0], CancellationToken::new()),
                catalogs[1].execute(&calls[1], CancellationToken::new()),
            );
            let final_bytes = fs::read(root.join("file")).unwrap();
            let final_text = std::str::from_utf8(&final_bytes).unwrap();
            let left = final_text.starts_with("LEFT:1\n");
            let right = final_text.ends_with("RIGHT:2\n");
            let successful = [&outputs.0, &outputs.1]
                .iter()
                .filter(|output| !output.is_error)
                .count();
            let correct = if guarded {
                successful == 1
                    && (left ^ right)
                    && [&outputs.0, &outputs.1].iter().all(|output| {
                        if output.is_error {
                            output.value["error"] == "file changed since supplied base_digest"
                        } else {
                            output.value["new_digest"] == hex_digest(&final_bytes)
                        }
                    })
            } else {
                successful == 2 && left && right
            };
            if !correct {
                failures.push(format!(
                    "guarded={guarded} successful={successful} left={left} right={right}"
                ));
            }
        }
        fs::remove_dir_all(root).unwrap();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[tokio::test]
    async fn stopped_native_mutations_leave_an_externally_locked_target_unchanged() {
        let root = std::env::temp_dir().join(format!("ion-mutation-stop-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file"), "old").unwrap();
        let holder = File::open(&root).unwrap();
        holder.lock().unwrap();
        let catalog = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>
        ])
        .snapshot();
        for (name, arguments) in [
            (
                "edit",
                json!({"path":"file","edits":[{"old_text":"old","new_text":"edited"}]}),
            ),
            ("write", json!({"path":"file","content":"written"})),
        ] {
            let call = ToolCall {
                id: name.into(),
                name: name.into(),
                arguments,
                raw_arguments: None,
            };
            let stop = CancellationToken::new();
            let pending = catalog.execute(&call, stop.clone());
            tokio::pin!(pending);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut pending)
                    .await
                    .is_err()
            );
            stop.cancel();
            let output = tokio::time::timeout(Duration::from_secs(2), pending)
                .await
                .unwrap();
            assert!(
                output.is_error
                    && output.value["error"]
                        .as_str()
                        .unwrap()
                        .contains("cancelled"),
                "{}",
                output.value
            );
            assert_eq!(fs::read(root.join("file")).unwrap(), b"old");
        }
        drop(holder);
        let output = LocalTools::new(&root).unwrap().write(
            &json!({"path":"file","content":"released"}),
            &CancellationToken::new(),
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(fs::read(root.join("file")).unwrap(), b"released");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queued_file_work_keeps_async_polling_live_and_cancels_before_effects() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let root =
                std::env::temp_dir().join(format!("ion-file-dispatch-{}", uuid::Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("file.txt"), "old").unwrap();
            let catalog = ion_core::ToolSet::new([
                Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>
            ])
            .snapshot();
            for (name, arguments) in [
                ("read", json!({"path":"file.txt"})),
                (
                    "edit",
                    json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}]}),
                ),
                ("write", json!({"path":"file.txt","content":"new"})),
            ] {
                // Hold the real blocking-pool resource, not a fake native tool.
                // Cancellation must remain serviceable while dispatch is queued.
                let (entered, started) = tokio::sync::oneshot::channel();
                let (release, held) = tokio::sync::oneshot::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    entered.send(()).unwrap();
                    held.blocking_recv().unwrap();
                });
                started.await.unwrap();
                let call = ToolCall {
                    id: name.into(),
                    name: name.into(),
                    arguments,
                    raw_arguments: None,
                };
                let stop = CancellationToken::new();
                let mut work = Box::pin(catalog.execute(&call, stop.clone()));
                let observed = tokio::time::timeout(Duration::from_millis(10), &mut work).await;
                let pending = observed.is_err();
                stop.cancel();
                release.send(()).unwrap();
                let output = match observed {
                    Ok(output) => output,
                    Err(_) => work.await,
                };
                blocker.await.unwrap();
                assert!(pending, "{name} bypassed bounded blocking dispatch");
                assert!(output.is_error, "{name}: {}", output.value);
                assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(), "old");
            }
            fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn write_preserves_existing_symlink() {
        let root = std::env::temp_dir().join(format!("ion-tools-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target.txt");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = tools.write(
            &json!({"path":"link.txt","content":"new"}),
            &CancellationToken::new(),
        );
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
        let edit = tools.edit(
            &json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}]}),
            &CancellationToken::new(),
        );
        assert!(edit.is_error, "{}", edit.value);
        let write = tools.write(
            &json!({"path":"file.txt","content":"new"}),
            &CancellationToken::new(),
        );
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
        let output = tools.edit(
            &json!({
                "path":"file.txt",
                "edits":[{"old_text":"two\nthree","new_text":"TWO\nthree"}],
                "base_digest": hex_digest(original.as_bytes()),
            }),
            &CancellationToken::new(),
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "\u{feff}one\r\nTWO\r\nthree\r\n"
        );
        let patch = output.value["diff"]["text"].as_str().unwrap();
        assert!(patch.contains("-two\r\n+TWO\r\n"), "{patch:?}");
        assert_eq!(output.value["diff"]["truncated"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_digest_covers_the_whole_file_and_can_guard_edit() {
        let root = std::env::temp_dir().join(format!("ion-read-digest-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("file.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let read = tools.read(
            &json!({"path":"file.txt","offset":2,"limit":1}),
            &CancellationToken::new(),
        );
        assert_eq!(read.value["content"], "beta\n");
        assert!(!read.is_error, "{}", read.value);
        let digest = read.value["base_digest"].as_str().unwrap();
        assert_eq!(digest, hex_digest(&fs::read(&path).unwrap()));
        let edit = tools.edit(
            &json!({
                "path":"file.txt",
                "edits":[{"old_text":"beta","new_text":"BETA"}],
                "base_digest":digest,
            }),
            &CancellationToken::new(),
        );
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
        let output = tools.edit(
            &json!({
                "path":"file.txt",
                "edits":[
                    {"old_text":"second","new_text":"SECOND"},
                    {"old_text":"α","new_text":"β"}
                ],
                "base_digest":hex_digest(original.as_bytes()),
            }),
            &CancellationToken::new(),
        );
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
            let output = tools.edit(
                &json!({"path":"file.txt","edits":edits}),
                &CancellationToken::new(),
            );
            assert!(output.is_error, "{}", output.value);
            assert_eq!(fs::read_to_string(&path).unwrap(), source);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_text_reads_skip_bounded_prefixes_and_honor_cancellation() {
        let root = std::env::temp_dir().join(format!("ion-read-large-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let mut text = vec![b'x'; MAX_FILE_BYTES + 1];
        text.extend_from_slice(b"\nTAIL\n");
        fs::write(root.join("file.txt"), &text).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = tools.read(
            &json!({"path":"file.txt","offset":2}),
            &CancellationToken::new(),
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["content"], "TAIL\n");
        assert_eq!(output.value["next_line"], 3);
        assert_eq!(output.value["base_digest"], Value::Null);
        assert_eq!(output.value["has_more"], false);
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(
            tools
                .read(&json!({"path":"file.txt","offset":2}), &stop)
                .is_error
        );
        for args in [
            json!({"path":"file.txt","offset":0}),
            json!({"path":"file.txt","limit":0}),
            json!({"path":"file.txt","limit":2001}),
        ] {
            assert!(tools.read(&args, &CancellationToken::new()).is_error);
        }
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
        let output = tools.read(&json!({"path":"picture.png"}), &CancellationToken::new());
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].mime_type().as_str(), "image/png");
        assert_eq!(output.value["path"], "picture.png");
        assert!(
            tools
                .read(
                    &json!({"path":"picture.png","offset":1}),
                    &CancellationToken::new()
                )
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
            let agent = CodingAgent::new(
                model.clone(),
                Arc::new(LocalTools::new(&root).unwrap()),
                ModelRef {
                    provider: "test".into(),
                    model: "test".into(),
                },
            )
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
                let events = Arc::new(std::sync::Mutex::new(Vec::new()));
                let observed = events.clone();
                let result = agent
                    .submit(
                        running_session.clone(),
                        "run".into(),
                        String::new(),
                        stop,
                        move |event| observed.lock().unwrap().push(event),
                    )
                    .await;
                let events = std::mem::take(&mut *events.lock().unwrap());
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
            let ion_core::ToolOutcome::Observed { output, .. } = &output.outcome else {
                panic!("missing observed cancellation")
            };
            assert_eq!(
                output.value["cancelled"], true,
                "must record observed cancellation, not a synthetic unknown effect: {output:?}"
            );
            assert_eq!(output.value["stdout"], "OBSERVED_BEFORE_CANCEL\n");
            assert!(output.value["exit_code"].is_number() || output.value["signal"].is_number());
            assert!(events.iter().any(|event| matches!(event, CodingAgentEvent::ToolFinished { outcome: ion_core::ToolOutcome::Observed { output, .. }, .. } if output.value["cancelled"] == true)));
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
            let reopened = Arc::new(CodingSession::open(&path).unwrap());
            assert_eq!(reopened.view().unwrap().entries, view.entries);
            drop(reopened);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

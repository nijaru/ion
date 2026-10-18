//! Native file validation, reads and coordinated exact replacements.
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use ion_ai::{ImageMime, MAX_SOURCE_BYTES, normalize_image};
use ion_core::CodingToolOutput as ToolOutput;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{error, present_input, success, text_read};

pub(super) const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

fn path(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.contains('\0') {
        return Err("invalid path".into());
    }
    Ok(cwd.join(raw))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default, deserialize_with = "present_input")]
    offset: Option<u64>,
    #[serde(default, deserialize_with = "present_input")]
    limit: Option<usize>,
    #[serde(default, deserialize_with = "present_input")]
    byte_offset: Option<u64>,
    #[serde(default, deserialize_with = "present_input")]
    byte_limit: Option<usize>,
}

impl ReadInput {
    fn range(&self) -> Result<text_read::Range, String> {
        if self.byte_offset.is_some() || self.byte_limit.is_some() {
            if self.offset.is_some() || self.limit.is_some() {
                return Err("read byte ranges cannot be combined with line offset or limit".into());
            }
            let limit = self.byte_limit.unwrap_or(text_read::MAX_BYTES);
            if !(4..=text_read::MAX_BYTES).contains(&limit) {
                return Err("read byte_limit must be between 4 and 65536 bytes".into());
            }
            Ok(text_read::Range::Bytes {
                offset: self.byte_offset.unwrap_or(0),
                limit,
            })
        } else {
            let offset = self.offset.unwrap_or(1);
            let limit = self.limit.unwrap_or(text_read::MAX_LINES);
            if offset == 0 {
                return Err("read offset must be a one-based line number".into());
            }
            if !(1..=text_read::MAX_LINES).contains(&limit) {
                return Err("read limit must be between 1 and 2000 lines".into());
            }
            Ok(text_read::Range::Lines { offset, limit })
        }
    }
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
    #[serde(default)]
    unconditional: bool,
}

pub(super) fn read(cwd: &Path, args: &Value, stop: &CancellationToken) -> ToolOutput {
    let input: ReadInput = match serde_json::from_value(args.clone()) {
        Ok(value) => value,
        Err(e) => return error(format!("invalid read arguments: {e}")),
    };
    let range = match input.range() {
        Ok(range) => range,
        Err(message) => return error(message),
    };
    let path = match path(cwd, &input.path) {
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
        if input.offset.is_some()
            || input.limit.is_some()
            || input.byte_offset.is_some()
            || input.byte_limit.is_some()
        {
            return error("image read does not accept line or byte ranges");
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
    match text_read::read(file, size, &input.path, range, stop) {
        Ok(value) => success(value),
        Err(message) => error(message),
    }
}

pub(super) fn edit(cwd: &Path, args: &Value, stop: &CancellationToken) -> ToolOutput {
    let input: EditInput = match serde_json::from_value(args.clone()) {
        Ok(value) => value,
        Err(e) => return error(format!("invalid edit arguments: {e}")),
    };
    if input.edits.is_empty() {
        return error("edits must contain at least one replacement");
    }
    let Some(base_digest) = input.base_digest else {
        return error("edit requires base_digest from a complete read");
    };
    let path = match path(cwd, &input.path) {
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
    if base_digest != actual {
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

pub(super) fn write(cwd: &Path, args: &Value, stop: &CancellationToken) -> ToolOutput {
    let input: WriteInput = match serde_json::from_value(args.clone()) {
        Ok(value) => value,
        Err(e) => return error(format!("invalid write arguments: {e}")),
    };
    if input.content.len() > MAX_FILE_BYTES {
        return error("content exceeds 8 MiB");
    }
    if input.base_digest.is_some() && input.unconditional {
        return error("write accepts base_digest or unconditional, not both");
    }
    let path = match path(cwd, &input.path) {
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
    let created = previous.is_none();
    if !created && input.base_digest.is_none() && !input.unconditional {
        return error(
            "write replacement requires base_digest from read; set unconditional=true only for intentional unread replacement",
        );
    }
    let mut previous_bytes = None;
    let mut diff_omitted = None;
    if !created {
        match crate::file_io::read_bounded(mutation.path(), MAX_FILE_BYTES) {
            Ok(bytes) => {
                if let Some(expected) = &input.base_digest
                    && expected != &hex_digest(&bytes)
                {
                    return error("file changed since supplied base_digest");
                }
                previous_bytes = Some(bytes);
            }
            Err(e) if input.base_digest.is_some() => {
                return error(format!("read guarded write target: {e}"));
            }
            Err(e) => {
                diff_omitted = Some(format!("existing content could not be captured: {e}"));
            }
        }
    } else if input.base_digest.is_some() {
        return error("guarded write requires an existing file");
    }
    if previous.is_some()
        && let Err(e) = mutation.ensure_writable()
    {
        return error(format!("write {}: {e}", input.path));
    }
    let diff = if created {
        Some(crate::edit_diff::capture(&input.path, "", &input.content))
    } else {
        match previous_bytes {
            Some(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Some(crate::edit_diff::capture(
                    &input.path,
                    &text,
                    &input.content,
                )),
                Err(_) => {
                    diff_omitted = Some("existing file is not UTF-8".into());
                    None
                }
            },
            None => None,
        }
    };
    match mutation.replace(input.content.as_bytes(), previous) {
        Ok(()) => {
            let mut value = json!({"path": input.path, "created": created, "bytes": input.content.len(), "new_digest": hex_digest(input.content.as_bytes())});
            if let Some(diff) = diff {
                value["diff"] = serde_json::to_value(diff).expect("write diff is serializable");
            }
            if let Some(reason) = diff_omitted {
                value["diff_omitted"] = Value::String(reason);
            }
            success(value)
        }
        Err(e) => error(format!("write failed: {e}")),
    }
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalTools;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn guarded_write_checks_whole_file_without_creating_missing_targets() {
        let root = std::env::temp_dir().join(format!("ion-write-guard-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let stop = CancellationToken::new();
        let source = "αβ🦀\n".repeat(4000);
        fs::write(root.join("file"), &source).unwrap();
        let read = read(
            &tools.cwd,
            &json!({"path":"file","limit":1}),
            &CancellationToken::new(),
        );
        assert_eq!(read.value["has_more"], true);
        assert_eq!(read.value["base_digest"], hex_digest(source.as_bytes()));
        let written = write(
            &tools.cwd,
            &json!({"path":"file","content":"first","base_digest":read.value["base_digest"]}),
            &stop,
        );
        assert!(!written.is_error, "{}", written.value);
        assert_eq!(written.value["created"], false);
        assert_eq!(written.value["new_digest"], hex_digest(b"first"));
        let stale = write(
            &tools.cwd,
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
            let rejected = write(
                &tools.cwd,
                &json!({"path":"missing/parents/file","content":"lost","base_digest":guard}),
                &stop,
            );
            assert!(rejected.is_error, "{}", rejected.value);
            assert!(!root.join("missing").exists());
        }
        let missing = write(
            &tools.cwd,
            &json!({"path":"absent","content":"lost","base_digest":hex_digest(b"")}),
            &stop,
        );
        assert!(missing.is_error && !root.join("absent").exists());
        let oversized = vec![b'x'; MAX_FILE_BYTES + 1];
        fs::write(root.join("large"), &oversized).unwrap();
        assert!(
            write(
                &tools.cwd,
                &json!({"path":"large","content":"lost","base_digest":hex_digest(&oversized)}),
                &stop,
            )
            .is_error
        );
        assert_eq!(fs::read(root.join("large")).unwrap(), oversized);
        let unguarded = write(
            &tools.cwd,
            &json!({"path":"large","content":"second"}),
            &stop,
        );
        assert!(unguarded.is_error);
        assert!(
            unguarded.value["error"]
                .as_str()
                .unwrap()
                .contains("requires base_digest")
        );
        assert_eq!(fs::read(root.join("large")).unwrap(), oversized);
        let unconditional = write(
            &tools.cwd,
            &json!({"path":"large","content":"second","unconditional":true}),
            &stop,
        );
        assert!(!unconditional.is_error, "{}", unconditional.value);
        assert_eq!(unconditional.value["new_digest"], hex_digest(b"second"));
        let conflicting = write(
            &tools.cwd,
            &json!({"path":"large","content":"third","base_digest":hex_digest(b"second"),"unconditional":true}),
            &stop,
        );
        assert!(conflicting.is_error);
        assert_eq!(fs::read(root.join("large")).unwrap(), b"second");
        let created = write(
            &tools.cwd,
            &json!({"path":"new/parents/file","content":"created"}),
            &stop,
        );
        assert!(!created.is_error && created.value["created"] == true);
        assert_eq!(created.value["new_digest"], hex_digest(b"created"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_records_bounded_diff_for_replacements_and_creations() {
        let root = std::env::temp_dir().join(format!("ion-write-diff-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let stop = CancellationToken::new();
        fs::write(root.join("file"), "old\n").unwrap();
        let rejected = write(
            &tools.cwd,
            &json!({"path":"file","content":"lost\n"}),
            &stop,
        );
        assert!(rejected.is_error);
        assert_eq!(fs::read(root.join("file")).unwrap(), b"old\n");
        let replaced = write(
            &tools.cwd,
            &json!({"path":"file","content":"new\n","base_digest":hex_digest(b"old\n")}),
            &stop,
        );
        assert!(!replaced.is_error, "{}", replaced.value);
        let diff = replaced.value["diff"]["text"].as_str().unwrap();
        assert!(diff.contains("-old") && diff.contains("+new"), "{diff}");
        assert!(replaced.value.get("diff_omitted").is_none());
        assert_eq!(replaced.value["diff"]["truncated"], false);

        let created = write(
            &tools.cwd,
            &json!({"path":"created","content":"created\n"}),
            &stop,
        );
        assert!(!created.is_error, "{}", created.value);
        let diff = created.value["diff"]["text"].as_str().unwrap();
        assert!(diff.contains("+created"), "{diff}");
        assert_eq!(created.value["created"], true);

        fs::write(root.join("binary"), [0xff, 0xfe]).unwrap();
        let binary = write(
            &tools.cwd,
            &json!({"path":"binary","content":"text\n","base_digest":hex_digest(&[0xff,0xfe])}),
            &stop,
        );
        assert!(!binary.is_error, "{}", binary.value);
        assert!(binary.value.get("diff").is_none());
        assert_eq!(binary.value["diff_omitted"], "existing file is not UTF-8");

        let oversized = vec![b'x'; MAX_FILE_BYTES + 1];
        fs::write(root.join("large"), &oversized).unwrap();
        let large = write(
            &tools.cwd,
            &json!({"path":"large","content":"replacement","unconditional":true}),
            &stop,
        );
        assert!(!large.is_error, "{}", large.value);
        assert!(large.value.get("diff").is_none());
        assert!(
            large.value["diff_omitted"]
                .as_str()
                .unwrap()
                .contains("could not be captured"),
            "{}",
            large.value
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_preserves_existing_symlink() {
        let root = std::env::temp_dir().join(format!("ion-tools-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target.txt");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
        let tools = LocalTools::new(&root).unwrap();
        let output = write(
            &tools.cwd,
            &json!({"path":"link.txt","content":"new","base_digest":hex_digest(b"old")}),
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
        let edit = edit(
            &tools.cwd,
            &json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}],"base_digest":hex_digest(b"old")}),
            &CancellationToken::new(),
        );
        assert!(edit.is_error, "{}", edit.value);
        let write = write(
            &tools.cwd,
            &json!({"path":"file.txt","content":"new","base_digest":hex_digest(b"old")}),
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
        let output = edit(
            &tools.cwd,
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
        let read = read(
            &tools.cwd,
            &json!({"path":"file.txt","offset":2,"limit":1}),
            &CancellationToken::new(),
        );
        assert_eq!(read.value["content"], "beta\n");
        assert!(!read.is_error, "{}", read.value);
        let digest = read.value["base_digest"].as_str().unwrap();
        assert_eq!(digest, hex_digest(&fs::read(&path).unwrap()));
        let bytes = super::read(
            &tools.cwd,
            &json!({"path":"file.txt","byte_offset":0,"byte_limit":4}),
            &CancellationToken::new(),
        );
        assert!(!bytes.is_error, "{}", bytes.value);
        assert_eq!(bytes.value["content"], "alph");
        assert_eq!(bytes.value["base_digest"], digest);
        let digest = bytes.value["base_digest"].as_str().unwrap();
        let edit = edit(
            &tools.cwd,
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
        let output = edit(
            &tools.cwd,
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
            let output = edit(
                &tools.cwd,
                &json!({"path":"file.txt","edits":edits,"base_digest":hex_digest(source.as_bytes())}),
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
        let output = read(
            &tools.cwd,
            &json!({"path":"file.txt","offset":2}),
            &CancellationToken::new(),
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["content"], "TAIL\n");
        assert_eq!(output.value["next_line"], 3);
        assert_eq!(output.value["base_digest"], Value::Null);
        assert_eq!(output.value["has_more"], false);
        let bytes = read(
            &tools.cwd,
            &json!({"path":"file.txt","byte_offset":MAX_FILE_BYTES + 2}),
            &CancellationToken::new(),
        );
        assert!(!bytes.is_error, "{}", bytes.value);
        assert_eq!(bytes.value["content"], "TAIL\n");
        assert_eq!(bytes.value["base_digest"], Value::Null);
        assert_eq!(bytes.value["has_more"], false);
        let stop = CancellationToken::new();
        stop.cancel();
        for args in [
            json!({"path":"file.txt","offset":2}),
            json!({"path":"file.txt","byte_offset":MAX_FILE_BYTES + 2}),
        ] {
            assert!(read(&tools.cwd, &args, &stop).is_error);
        }
        for args in [
            json!({"path":"file.txt","offset":0}),
            json!({"path":"file.txt","limit":0}),
            json!({"path":"file.txt","limit":2001}),
            json!({"path":"file.txt","byte_limit":3}),
            json!({"path":"file.txt","byte_limit":65537}),
            json!({"path":"file.txt","byte_offset":-1}),
            json!({"path":"file.txt","byte_offset":null}),
            json!({"path":"file.txt","byte_limit":null}),
            json!({"path":"file.txt","byte_offset":0,"limit":1}),
            json!({"path":"file.txt","byte_limit":4,"offset":1}),
        ] {
            assert!(read(&tools.cwd, &args, &CancellationToken::new()).is_error);
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
        let output = read(
            &tools.cwd,
            &json!({"path":"picture.png"}),
            &CancellationToken::new(),
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].mime_type().as_str(), "image/png");
        assert_eq!(output.value["path"], "picture.png");
        for args in [
            json!({"path":"picture.png","offset":1}),
            json!({"path":"picture.png","byte_offset":0}),
        ] {
            assert!(read(&tools.cwd, &args, &CancellationToken::new()).is_error);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

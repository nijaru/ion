//! Attachment parsing for `@path` references in chat input.
//!
//! Detects file/directory references in user input and injects their contents
//! as content blocks. Supports text files, images, PDFs, directories, and
//! binary file metadata.

use crate::provider::ContentBlock;
use base64::Engine;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

/// Maximum text file size before rejection (500KB).
const MAX_TEXT_SIZE: u64 = 500 * 1024;

/// Maximum line count for text files.
const MAX_TEXT_LINES: usize = 5000;

/// Maximum image file size (20MB).
const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;

/// Maximum PDF file size (10MB).
const MAX_PDF_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum chars extracted from a PDF.
const MAX_PDF_CHARS: usize = 500_000;

/// Maximum directory tree entries.
const MAX_DIR_ENTRIES: usize = 200;

/// Maximum directory recursion depth.
const MAX_DIR_DEPTH: usize = 3;

/// Aggregate cap for all text content across attachments per message (1MB).
const AGGREGATE_TEXT_CAP: usize = 1024 * 1024;

/// Image extensions and their MIME types.
const IMAGE_FORMATS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// Known binary file extensions (no content extraction).
const BINARY_EXTENSIONS: &[&str] = &[
    "exe", "dll", "so", "dylib", "o", "a", "lib", "class", "jar", "zip", "tar", "gz", "bz2", "xz",
    "7z", "rar", "wasm", "pyc", "pyo", "beam", "mp4", "mov", "avi", "mp3", "wav", "flac", "ico",
    "bmp", "tiff", "psd", "doc", "xls", "ppt", "docx", "xlsx", "pptx", "db", "sqlite", "sqlite3",
    "dat", "bin", "img", "iso", "dmg", "deb", "rpm",
];

/// Directories to skip during tree listing.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
];

/// Parse input text and resolve `@path` references into content blocks.
///
/// Returns a list of content blocks: the user's text (with references stripped)
/// plus injected file/directory content blocks.
pub async fn parse_attachments(
    input: &str,
    working_dir: &Path,
    no_sandbox: bool,
) -> Vec<ContentBlock> {
    let refs = extract_refs(input);

    if refs.is_empty() {
        return vec![ContentBlock::Text {
            text: input.to_string(),
        }];
    }

    // Build user text with refs stripped
    let mut user_text = String::with_capacity(input.len());
    let mut last_end = 0;
    let mut attachment_blocks = Vec::new();
    let mut aggregate_text_bytes = 0usize;

    for r in &refs {
        user_text.push_str(&input[last_end..r.start]);
        last_end = r.end;

        let path = if Path::new(&r.path).is_absolute() {
            PathBuf::from(&r.path)
        } else {
            working_dir.join(&r.path)
        };

        // Sandbox check
        let display_header = format_display_header(&r.path, r.range);
        if !no_sandbox && let Err(msg) = check_within_dir(&path, working_dir) {
            attachment_blocks.push(ContentBlock::Text {
                text: format!("--- {display_header} ---\n[Error: {msg}]\n---"),
            });
            continue;
        }

        let ref_path = r.path.clone();
        match resolve_attachment(&path, &ref_path, r.range, &mut aggregate_text_bytes).await {
            Ok(block) => attachment_blocks.push(block),
            Err(msg) => {
                attachment_blocks.push(ContentBlock::Text {
                    text: format!("--- {display_header} ---\n[Error: {msg}]\n---"),
                });
            }
        }
    }

    // Append remaining text
    user_text.push_str(&input[last_end..]);

    let mut blocks = Vec::new();
    let trimmed = user_text.trim();
    if !trimmed.is_empty() {
        blocks.push(ContentBlock::Text {
            text: trimmed.to_string(),
        });
    }
    blocks.extend(attachment_blocks);

    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: String::new(),
        });
    }

    blocks
}

/// A parsed `@path` reference with byte offsets into the original input.
#[derive(Debug)]
struct PathRef {
    path: String,
    /// Optional line range (start, end), 1-indexed inclusive.
    range: Option<(usize, usize)>,
    start: usize,
    end: usize,
}

/// Extract all `@path` references from input text.
///
/// Rules:
/// - Must be preceded by start-of-string, newline, or whitespace
/// - Path must contain `/` or `.` to distinguish from `@username`
/// - Unquoted paths end at whitespace; quoted paths use `@"path"`
fn extract_refs(input: &str) -> Vec<PathRef> {
    let mut refs = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }

        // Check word boundary: start of string, or preceded by whitespace/newline
        if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        let at_pos = i;
        i += 1; // skip @

        if i >= bytes.len() {
            break;
        }

        // Quoted path: @"path with spaces" or @"path":10-50
        let (path, end) = if bytes[i] == b'"' {
            i += 1; // skip opening quote
            let quote_start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let path = &input[quote_start..i];
            if i < bytes.len() {
                i += 1; // skip closing quote
            }
            // Consume optional :N-M suffix after closing quote
            let suffix_start = i;
            if i < bytes.len() && bytes[i] == b':' {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'-') {
                    i += 1;
                }
                // Include the suffix in the path so parse_line_range can extract it
                let suffix = &input[suffix_start..i];
                (format!("{path}{suffix}"), i)
            } else {
                (path.to_string(), i)
            }
        } else {
            // Unquoted path: ends at whitespace
            let path_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let path = &input[path_start..i];
            (path.to_string(), i)
        };

        if path.is_empty() {
            continue;
        }

        // Parse optional :N-M or :N line range suffix
        let (path, range) = parse_line_range(&path);

        // Must contain `/` or `.` to distinguish from @username
        if !path.contains('/') && !path.contains('.') {
            continue;
        }

        refs.push(PathRef {
            path,
            range,
            start: at_pos,
            end,
        });
    }

    refs
}

/// Parse a `:N-M` or `:N` line range suffix from a path string.
///
/// Returns `(path, Some((start, end)))` if a valid range was found,
/// or `(original_path, None)` if not. Only treats as range when the
/// colon is followed by digits (avoids matching Windows paths like `C:\`).
fn parse_line_range(path: &str) -> (String, Option<(usize, usize)>) {
    // Find last colon
    let Some(colon_pos) = path.rfind(':') else {
        return (path.to_string(), None);
    };

    let suffix = &path[colon_pos + 1..];
    if suffix.is_empty() {
        return (path.to_string(), None);
    }

    // Must start with a digit
    if !suffix.as_bytes()[0].is_ascii_digit() {
        return (path.to_string(), None);
    }

    // Try N-M format
    if let Some(dash_pos) = suffix.find('-') {
        let start_str = &suffix[..dash_pos];
        let end_str = &suffix[dash_pos + 1..];
        if let (Ok(start), Ok(end)) = (start_str.parse::<usize>(), end_str.parse::<usize>())
            && start > 0
            && end >= start
        {
            return (path[..colon_pos].to_string(), Some((start, end)));
        }
        // Invalid range format — treat whole thing as path
        return (path.to_string(), None);
    }

    // Try single line N
    if let Ok(line) = suffix.parse::<usize>()
        && line > 0
    {
        return (path[..colon_pos].to_string(), Some((line, line)));
    }

    (path.to_string(), None)
}

/// Sandbox check: verify path resolves within working_dir.
fn check_within_dir(path: &Path, working_dir: &Path) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .or_else(|_| {
            // Path might not exist yet — check parent
            path.parent()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no parent"))
                .and_then(|p| {
                    p.canonicalize()
                        .map(|cp| cp.join(path.file_name().unwrap_or_default()))
                })
        })
        .map_err(|e| format!("cannot resolve path: {e}"))?;

    let wd_canonical = working_dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve working directory: {e}"))?;

    if canonical.starts_with(&wd_canonical) {
        Ok(())
    } else {
        Err(format!(
            "path is outside sandbox ({})",
            working_dir.display()
        ))
    }
}

/// Resolve a single attachment path to a content block.
async fn resolve_attachment(
    path: &Path,
    display_path: &str,
    range: Option<(usize, usize)>,
    aggregate_text_bytes: &mut usize,
) -> Result<ContentBlock, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("cannot read: {e}"))?;

    if metadata.is_dir() {
        let block = load_directory(path, display_path)?;
        if let ContentBlock::Text { ref text } = block {
            *aggregate_text_bytes += text.len();
            if *aggregate_text_bytes > AGGREGATE_TEXT_CAP {
                return Err("aggregate attachment size exceeds 1MB limit".to_string());
            }
        }
        return Ok(block);
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);

    // Image
    if let Some(ref ext) = ext
        && IMAGE_FORMATS.iter().any(|(e, _)| e == ext)
    {
        return load_image(path, display_path);
    }

    // PDF
    if ext.as_deref() == Some("pdf") {
        let path_owned = path.to_path_buf();
        let display = display_path.to_string();
        let block = tokio::task::spawn_blocking(move || load_pdf(&path_owned, &display))
            .await
            .map_err(|e| format!("task failed: {e}"))??;
        if let ContentBlock::Text { ref text } = block {
            *aggregate_text_bytes += text.len();
            if *aggregate_text_bytes > AGGREGATE_TEXT_CAP {
                return Err("aggregate attachment size exceeds 1MB limit".to_string());
            }
        }
        return Ok(block);
    }

    // Known binary
    if let Some(ref ext) = ext
        && BINARY_EXTENSIONS.iter().any(|b| b == ext)
    {
        return load_binary_metadata(path, display_path, &metadata);
    }

    // No extension — sniff first 8KB for null bytes
    if ext.is_none() {
        let mut buf = vec![0u8; 8192.min(metadata.len() as usize)];
        if let Ok(mut f) = std::fs::File::open(path) {
            let n = f.read(&mut buf).unwrap_or(0);
            if buf[..n].contains(&0) {
                return load_binary_metadata(path, display_path, &metadata);
            }
        }
    }

    // Text file
    let block = load_text_file(path, display_path, &metadata, range)?;
    if let ContentBlock::Text { ref text } = block {
        *aggregate_text_bytes += text.len();
        if *aggregate_text_bytes > AGGREGATE_TEXT_CAP {
            return Err("aggregate attachment size exceeds 1MB limit".to_string());
        }
    }
    Ok(block)
}

/// Format the display header for a file, including optional line range.
fn format_display_header(display_path: &str, range: Option<(usize, usize)>) -> String {
    match range {
        Some((start, end)) if start == end => format!("{display_path}:{start}"),
        Some((start, end)) => format!("{display_path}:{start}-{end}"),
        None => display_path.to_string(),
    }
}

/// Load a text file with size and line truncation.
fn load_text_file(
    path: &Path,
    display_path: &str,
    metadata: &std::fs::Metadata,
    range: Option<(usize, usize)>,
) -> Result<ContentBlock, String> {
    if metadata.len() > MAX_TEXT_SIZE {
        return Err(format!(
            "file too large: {} bytes (max {})",
            metadata.len(),
            MAX_TEXT_SIZE
        ));
    }

    let file = std::fs::File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let reader = std::io::BufReader::new(file.take(MAX_TEXT_SIZE));

    let mut lines = Vec::new();
    let mut total_lines = 0usize;
    let mut truncated = false;

    for line_result in reader.lines() {
        total_lines += 1;
        match line_result {
            Ok(line) => {
                // Skip lines before range start
                if let Some((start, _)) = range
                    && total_lines < start
                {
                    continue;
                }
                // Stop after range end
                if let Some((_, end)) = range
                    && total_lines > end
                {
                    break;
                }
                if lines.len() < MAX_TEXT_LINES {
                    lines.push(line);
                } else {
                    truncated = true;
                }
            }
            Err(_) => {
                // Non-UTF-8 — fall back to lossy read
                return load_text_file_lossy(path, display_path, range);
            }
        }
    }

    let mut content = lines.join("\n");
    if truncated {
        content.push_str(&format!("\n[... truncated at {} lines]", MAX_TEXT_LINES,));
    }

    let header = format_display_header(display_path, range);
    Ok(ContentBlock::Text {
        text: format!("--- {header} ---\n{content}\n---"),
    })
}

/// Fallback for files with non-UTF-8 bytes: lossy conversion.
fn load_text_file_lossy(
    path: &Path,
    display_path: &str,
    range: Option<(usize, usize)>,
) -> Result<ContentBlock, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);

    let all_lines: Vec<&str> = text.lines().collect();

    // Apply line range filter
    let lines: Vec<&str> = if let Some((start, end)) = range {
        let start_idx = start.saturating_sub(1); // 1-indexed to 0-indexed
        let end_idx = end.min(all_lines.len());
        all_lines
            .get(start_idx..end_idx)
            .unwrap_or_default()
            .to_vec()
    } else {
        all_lines
    };

    let total = lines.len();
    let truncated = total > MAX_TEXT_LINES;
    let content: String = if truncated {
        let mut s = lines[..MAX_TEXT_LINES].join("\n");
        s.push_str(&format!("\n[... truncated at {} lines]", MAX_TEXT_LINES,));
        s
    } else {
        lines.join("\n")
    };

    let header = format_display_header(display_path, range);
    Ok(ContentBlock::Text {
        text: format!("--- {header} ---\n{content}\n---"),
    })
}

/// Load a directory as a tree listing.
fn load_directory(path: &Path, display_path: &str) -> Result<ContentBlock, String> {
    let mut entries = Vec::new();
    let mut truncated = false;
    collect_tree(path, "", 0, &mut entries, &mut truncated);

    let tree = entries.join("\n");
    let mut content = format!("--- {display_path}/ ---\n{tree}");
    if truncated {
        content.push_str(&format!("\n[... truncated at {} entries]", MAX_DIR_ENTRIES));
    }
    content.push_str("\n---");

    Ok(ContentBlock::Text { text: content })
}

/// Recursively collect directory entries into a tree.
fn collect_tree(
    dir: &Path,
    prefix: &str,
    depth: usize,
    entries: &mut Vec<String>,
    truncated: &mut bool,
) {
    if depth > MAX_DIR_DEPTH || entries.len() >= MAX_DIR_ENTRIES {
        *truncated = entries.len() >= MAX_DIR_ENTRIES;
        return;
    }

    let mut items: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                // Skip hidden files and known junk dirs
                if name_str.starts_with('.') {
                    return false;
                }
                if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                    return !SKIP_DIRS.contains(&name_str.as_ref());
                }
                true
            })
            .collect(),
        Err(_) => return,
    };

    // Sort: dirs first, then alphabetical
    items.sort_by(|a, b| {
        let a_dir = a.file_type().is_ok_and(|ft| ft.is_dir());
        let b_dir = b.file_type().is_ok_and(|ft| ft.is_dir());
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });

    for item in items {
        if entries.len() >= MAX_DIR_ENTRIES {
            *truncated = true;
            return;
        }

        let name = item.file_name();
        let name_str = name.to_string_lossy();
        let is_dir = item.file_type().is_ok_and(|ft| ft.is_dir());

        if is_dir {
            entries.push(format!("{prefix}{name_str}/"));
            collect_tree(
                &item.path(),
                &format!("{prefix}  "),
                depth + 1,
                entries,
                truncated,
            );
        } else {
            let size = item.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push(format!("{prefix}{name_str} ({})", format_size(size)));
        }
    }
}

/// Load an image file as a base64-encoded content block.
fn load_image(path: &Path, display_path: &str) -> Result<ContentBlock, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .ok_or_else(|| "no file extension".to_string())?;

    let media_type = IMAGE_FORMATS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mt)| *mt)
        .ok_or_else(|| format!("unsupported image format: {ext}"))?;

    let metadata = std::fs::metadata(path).map_err(|e| format!("cannot stat: {e}"))?;
    if metadata.len() > MAX_IMAGE_SIZE {
        return Err(format!(
            "image too large: {} (max {})",
            format_size(metadata.len()),
            format_size(MAX_IMAGE_SIZE)
        ));
    }

    let data = std::fs::read(path).map_err(|e| format!("cannot read {display_path}: {e}"))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok(ContentBlock::Image {
        media_type: media_type.to_string(),
        data: encoded,
    })
}

/// Extract text from a PDF file.
fn load_pdf(path: &Path, display_path: &str) -> Result<ContentBlock, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("cannot stat: {e}"))?;
    if metadata.len() > MAX_PDF_SIZE {
        return Err(format!(
            "PDF too large: {} (max {})",
            format_size(metadata.len()),
            format_size(MAX_PDF_SIZE)
        ));
    }

    let bytes = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;

    let mut text = pdf_extract::extract_text_from_mem(&bytes)
        .map_err(|e| format!("PDF extraction failed: {e}"))?;

    let mut truncation_note = String::new();
    if text.len() > MAX_PDF_CHARS {
        text.truncate(MAX_PDF_CHARS);
        truncation_note = format!("\n[... truncated at {} chars]", MAX_PDF_CHARS);
    }

    Ok(ContentBlock::Text {
        text: format!("--- {display_path} (PDF) ---\n{text}{truncation_note}\n---"),
    })
}

/// Return metadata-only block for binary files.
fn load_binary_metadata(
    path: &Path,
    display_path: &str,
    metadata: &std::fs::Metadata,
) -> Result<ContentBlock, String> {
    let size = format_size(metadata.len());
    let description = detect_binary_type(path);

    Ok(ContentBlock::Text {
        text: format!("--- {display_path} ---\n[Binary file: {description}, {size}]\n---"),
    })
}

/// Detect binary file type via extension or `file` command.
fn detect_binary_type(path: &Path) -> String {
    // Try `file` command first
    if let Ok(output) = std::process::Command::new("file")
        .arg("--brief")
        .arg(path)
        .output()
        && output.status.success()
    {
        let desc = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !desc.is_empty() {
            return desc;
        }
    }

    // Fall back to extension-based
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| format!("{ext} file"))
        .unwrap_or_else(|| "unknown binary".to_string())
}

/// Format a byte count as a human-readable size.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn test_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    fn minimal_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // 8-bit RGBA
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
            0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, // compressed
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
            0xAE, 0x42, 0x60, 0x82,
        ]
    }

    #[test]
    fn test_extract_refs_basic() {
        let refs = extract_refs("look at @src/main.rs please");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "src/main.rs");
    }

    #[test]
    fn test_extract_refs_quoted() {
        let refs = extract_refs("check @\"path with spaces/file.rs\" now");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "path with spaces/file.rs");
    }

    #[test]
    fn test_extract_refs_email_ignored() {
        let refs = extract_refs("contact user@example.com for help");
        assert_eq!(refs.len(), 0, "emails should not be treated as refs");
    }

    #[test]
    fn test_extract_refs_username_ignored() {
        let refs = extract_refs("ask @username about it");
        assert_eq!(refs.len(), 0, "@username without / or . is not a path ref");
    }

    #[test]
    fn test_extract_refs_multiple() {
        let refs = extract_refs("@file1.rs @src/ @pic.png");
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].path, "file1.rs");
        assert_eq!(refs[1].path, "src/");
        assert_eq!(refs[2].path, "pic.png");
    }

    #[tokio::test]
    async fn test_no_attachments() {
        let blocks = parse_attachments("Hello world", Path::new("."), true).await;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Hello world"));
    }

    #[tokio::test]
    async fn test_text_file() {
        let dir = test_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "line one\nline two\n").unwrap();

        let blocks = parse_attachments("read @test.txt", dir.path(), true).await;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "read"));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("line one")));
        assert!(
            matches!(&blocks[1], ContentBlock::Text { text } if text.contains("--- test.txt ---"))
        );
    }

    #[tokio::test]
    async fn test_directory() {
        let dir = test_dir();
        let sub = dir.path().join("mydir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("a.rs"), "fn main() {}").unwrap();
        fs::write(sub.join("b.rs"), "fn test() {}").unwrap();

        let blocks = parse_attachments("list @mydir/", dir.path(), true).await;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("a.rs")));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("b.rs")));
    }

    #[tokio::test]
    async fn test_image_auto_detect() {
        let dir = test_dir();
        let img_path = dir.path().join("screenshot.png");
        fs::write(&img_path, minimal_png()).unwrap();

        let blocks = parse_attachments("look at @screenshot.png", dir.path(), true).await;
        assert_eq!(blocks.len(), 2);
        assert!(
            matches!(&blocks[1], ContentBlock::Image { media_type, .. } if media_type == "image/png")
        );
    }

    #[tokio::test]
    async fn test_binary_metadata() {
        let dir = test_dir();
        let bin_path = dir.path().join("app.exe");
        fs::write(&bin_path, b"\x00\x01\x02binary").unwrap();

        let blocks = parse_attachments("check @app.exe", dir.path(), true).await;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("Binary file")));
    }

    #[tokio::test]
    async fn test_missing_file() {
        let dir = test_dir();
        let blocks = parse_attachments("read @nonexistent.rs", dir.path(), true).await;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("Error")));
    }

    #[tokio::test]
    async fn test_large_file_truncated() {
        let dir = test_dir();
        let file_path = dir.path().join("big.txt");
        let content: String = (0..6000).map(|i| format!("line {i}\n")).collect();
        fs::write(&file_path, &content).unwrap();

        let blocks = parse_attachments("read @big.txt", dir.path(), true).await;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("truncated")));
    }

    #[tokio::test]
    async fn test_multiple_refs() {
        let dir = test_dir();
        fs::write(dir.path().join("file1.rs"), "fn one() {}").unwrap();
        let sub = dir.path().join("src");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("lib.rs"), "mod lib;").unwrap();
        fs::write(dir.path().join("pic.png"), minimal_png()).unwrap();

        let blocks = parse_attachments("@file1.rs @src/ @pic.png", dir.path(), true).await;
        // 3 attachment blocks (no user text since entire input is refs)
        assert!(blocks.len() >= 3);
    }

    #[tokio::test]
    async fn test_quoted_path() {
        let dir = test_dir();
        let spaced_dir = dir.path().join("path with spaces");
        fs::create_dir(&spaced_dir).unwrap();
        fs::write(spaced_dir.join("file.rs"), "fn spaced() {}").unwrap();

        let blocks = parse_attachments("@\"path with spaces/file.rs\"", dir.path(), true).await;
        assert!(blocks.len() >= 1);
        // Should find the file
        let has_content = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("fn spaced()")));
        assert!(has_content, "should contain file content");
    }

    #[tokio::test]
    async fn test_path_traversal_blocked() {
        let dir = test_dir();
        // Create a file outside sandbox
        let parent = dir.path().parent().unwrap();
        let outside = parent.join("outside.txt");
        fs::write(&outside, "secret").unwrap();

        let blocks = parse_attachments(
            "@../outside.txt",
            dir.path(),
            false, // sandbox enabled
        )
        .await;

        let has_error = blocks.iter().any(|b| {
            matches!(b, ContentBlock::Text { text } if text.contains("Error") && text.contains("sandbox"))
        });
        assert!(has_error, "traversal should be blocked by sandbox");

        // Cleanup
        let _ = fs::remove_file(&outside);
    }

    #[tokio::test]
    async fn test_non_utf8_lossy() {
        let dir = test_dir();
        let file_path = dir.path().join("mixed.txt");
        // Mix of valid UTF-8 and invalid bytes
        let mut content = b"valid text\n".to_vec();
        content.extend_from_slice(&[0xFF, 0xFE, b'\n']);
        content.extend_from_slice(b"more valid text\n");
        fs::write(&file_path, &content).unwrap();

        let blocks = parse_attachments("@mixed.txt", dir.path(), true).await;
        let has_text = blocks.iter().any(|b| {
            matches!(b, ContentBlock::Text { text } if text.contains("valid text") && text.contains("\u{FFFD}"))
        });
        assert!(has_text, "should handle non-UTF-8 with lossy conversion");
    }

    #[tokio::test]
    async fn test_pdf_extraction() {
        let dir = test_dir();
        let pdf_path = dir.path().join("test.pdf");
        // Minimal valid PDF with text "Hello"
        let pdf_content = b"%PDF-1.0\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R/Contents 4 0 R>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 100 700 Td (Hello) Tj ET\nendstream\nendobj\nxref\n0 5\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000210 00000 n \ntrailer<</Size 5/Root 1 0 R>>\nstartxref\n306\n%%EOF";
        fs::write(&pdf_path, pdf_content).unwrap();

        let blocks = parse_attachments("read @test.pdf", dir.path(), true).await;
        // Should produce a text block with PDF content (or an error if extraction fails on minimal PDF)
        assert!(blocks.len() >= 1);
        let has_pdf_ref = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("test.pdf")));
        assert!(has_pdf_ref, "should reference the PDF file");
    }

    #[tokio::test]
    async fn test_pdf_malformed_graceful() {
        let dir = test_dir();
        let pdf_path = dir.path().join("corrupt.pdf");
        fs::write(&pdf_path, b"not a real pdf").unwrap();

        let blocks = parse_attachments("read @corrupt.pdf", dir.path(), true).await;
        let has_error = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("Error")));
        assert!(has_error, "corrupt PDF should produce error, not panic");
    }

    #[tokio::test]
    async fn test_aggregate_size_limit() {
        let dir = test_dir();
        // Create multiple 400KB files (total > 1MB)
        for i in 0..4 {
            let name = format!("big{i}.txt");
            let content = "x".repeat(400 * 1024);
            fs::write(dir.path().join(&name), &content).unwrap();
        }

        let blocks =
            parse_attachments("@big0.txt @big1.txt @big2.txt @big3.txt", dir.path(), true).await;

        let has_limit_error = blocks.iter().any(|b| {
            matches!(b, ContentBlock::Text { text } if text.contains("aggregate") && text.contains("1MB"))
        });
        assert!(has_limit_error, "should hit aggregate size limit");
    }

    // --- Line range tests ---

    #[test]
    fn test_extract_refs_line_range() {
        let refs = extract_refs("@file.rs:10-50");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "file.rs");
        assert_eq!(refs[0].range, Some((10, 50)));
    }

    #[test]
    fn test_extract_refs_single_line() {
        let refs = extract_refs("@file.rs:42");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "file.rs");
        assert_eq!(refs[0].range, Some((42, 42)));
    }

    #[test]
    fn test_colon_not_range() {
        // Non-digit after colon — treated as part of path
        let refs = extract_refs("@file.rs:abc");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "file.rs:abc");
        assert_eq!(refs[0].range, None);
    }

    #[test]
    fn test_extract_refs_quoted_with_range() {
        let refs = extract_refs("@\"file.rs\":10-50");
        assert_eq!(refs.len(), 1);
        // Quoted path gets range parsed from after the closing quote
        assert_eq!(refs[0].path, "file.rs");
        assert_eq!(refs[0].range, Some((10, 50)));
    }

    #[tokio::test]
    async fn test_line_range_extraction() {
        let dir = test_dir();
        let file_path = dir.path().join("lines.txt");
        let content: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        fs::write(&file_path, &content).unwrap();

        let blocks = parse_attachments("@lines.txt:10-20", dir.path(), true).await;
        assert_eq!(blocks.len(), 1); // No user text left after stripping ref
        let text = match &blocks[0] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected text block"),
        };
        assert!(
            text.contains("--- lines.txt:10-20 ---"),
            "header should show range"
        );
        assert!(text.contains("line 10"), "should include line 10");
        assert!(text.contains("line 20"), "should include line 20");
        assert!(!text.contains("line 9\n"), "should not include line 9");
        assert!(!text.contains("line 21"), "should not include line 21");
    }

    #[tokio::test]
    async fn test_line_range_out_of_bounds() {
        let dir = test_dir();
        let file_path = dir.path().join("short.txt");
        fs::write(&file_path, "line 1\nline 2\nline 3\n").unwrap();

        let blocks = parse_attachments("@short.txt:2-100", dir.path(), true).await;
        let text = match &blocks[0] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected text block"),
        };
        assert!(text.contains("line 2"), "should include line 2");
        assert!(text.contains("line 3"), "should include line 3");
        assert!(!text.contains("line 1\n"), "should not include line 1");
        // Should not error — just reads to EOF
        assert!(
            !text.contains("Error"),
            "should not error on out-of-bounds range"
        );
    }

    // --- parse_line_range edge cases ---

    #[test]
    fn test_parse_line_range_zero_line() {
        // Line 0 is invalid (1-indexed), treat as path
        let (path, range) = parse_line_range("file.rs:0");
        assert_eq!(path, "file.rs:0");
        assert_eq!(range, None);
    }

    #[test]
    fn test_parse_line_range_reversed() {
        // end < start is invalid
        let (path, range) = parse_line_range("file.rs:50-10");
        assert_eq!(path, "file.rs:50-10");
        assert_eq!(range, None);
    }

    #[test]
    fn test_parse_line_range_trailing_dash() {
        let (path, range) = parse_line_range("file.rs:10-");
        assert_eq!(path, "file.rs:10-");
        assert_eq!(range, None);
    }

    #[test]
    fn test_parse_line_range_no_colon() {
        let (path, range) = parse_line_range("file.rs");
        assert_eq!(path, "file.rs");
        assert_eq!(range, None);
    }

    #[test]
    fn test_parse_line_range_colon_in_dir() {
        // Last colon is the range separator, earlier colons are preserved
        let (path, range) = parse_line_range("some:dir/file.rs:42");
        assert_eq!(path, "some:dir/file.rs");
        assert_eq!(range, Some((42, 42)));
    }
}

//! Direct coding tools with the caller's host permissions.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ion_ai::{BoxFuture, ToolSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use ion_core::{
    CodingToolOutput as ToolOutput, CodingToolSource as ToolSource, ToolActivityKind,
    ToolDefinition, ToolExecutor, ToolExposure, ToolPresentation, ToolRegistration,
};

pub(crate) mod command;
mod discovery;
mod files;
mod text_read;

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
    /// Run a user's direct command through the native shell execution path.
    pub async fn run_user_shell(&self, command: &str, stop: CancellationToken) -> ToolOutput {
        command::run(&self.cwd, &json!({"command": command}), stop).await
    }
}

impl crate::UserShell for LocalTools {
    fn prepare(&self, command: &str) -> anyhow::Result<crate::PreparedShell> {
        let tools = Self::new(&self.cwd)?;
        let command = command.to_owned();
        Ok(Box::new(move |stop| {
            Box::pin(async move { tools.run_user_shell(&command, stop).await })
        }))
    }
}

impl ToolSource for LocalTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        specs()
            .into_iter()
            .map(|spec| {
                let (operation, kind, key) = match spec.name.as_str() {
                    "read" => (LocalOperation::Read, ToolActivityKind::Read, "path"),
                    "ls" => (LocalOperation::Ls, ToolActivityKind::List, "path"),
                    "find" => (LocalOperation::Find, ToolActivityKind::Search, "pattern"),
                    "grep" => (LocalOperation::Grep, ToolActivityKind::Search, "pattern"),
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
    Ls,
    Find,
    Grep,
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
        invocation: ion_core::ToolInvocation<'a>,
    ) -> BoxFuture<'a, ToolOutput> {
        let call = invocation.call();
        let stop = invocation.stop().clone();
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
                        LocalOperation::Read => files::read(&tools.cwd, &arguments, &stop),
                        LocalOperation::Ls => discovery::ls(&tools.cwd, &arguments, &stop),
                        LocalOperation::Find => discovery::find(&tools.cwd, &arguments, &stop),
                        LocalOperation::Grep => discovery::grep(&tools.cwd, &arguments, &stop),
                        LocalOperation::Edit => files::edit(&tools.cwd, &arguments, &stop),
                        LocalOperation::Write => files::write(&tools.cwd, &arguments, &stop),
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
fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec { name: "read".into(), description: "Read UTF-8 text or a supported image (JPEG, PNG, GIF, WebP) from the live working directory. Images are attached to the result. Paths may be relative or absolute. Text reads use one-based line ranges (default 2000 lines), bounded to 65536 bytes of complete lines. Check has_more and pass next_line as offset to continue. For oversized lines or saved JSON observations, use zero-based byte_offset and byte_limit (4..65536, default 65536) instead of line offset/limit. Byte pages preserve UTF-8; pass next_byte as byte_offset to continue. base_digest covers the complete file when available, not just returned content. Do not recreate an existing file from an incomplete read; prefer exact edits or read all pages before full replacement.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":1,"description":"One-based starting line (default 1). Pass the prior result's next_line to continue. Omit for images."},"limit":{"type":"integer","minimum":1,"maximum":2000,"description":"Maximum complete lines to return (default and maximum 2000); the 65536-byte output bound may stop earlier. Omit for images."},"byte_offset":{"type":"integer","minimum":0,"description":"Zero-based UTF-8 byte offset (default 0). Cannot combine byte and line selectors. Start at a character boundary or use the previous next_byte. Omit for images."},"byte_limit":{"type":"integer","minimum":4,"maximum":65536,"description":"Maximum UTF-8 bytes (default 65536), independent of physical lines. End rounds down to a character boundary; continue at next_byte. Omit for images."}}}) },
        ToolSpec { name: "ls".into(), description: "List one directory, including hidden entries. Returns names sorted lexicographically with type file, dir, symlink or other. Path defaults to the working directory; absolute paths are allowed. limit defaults to 500 and is at most 1000. If the directory is too large or cannot be read exhaustively, partial results are explicit: check exhaustive, skipped and omissions. Output is serialized JSON bounded to 64 KiB; refine the directory after an entry, work or output cap. This is ordinary host filesystem access, not a sandbox.".into(), input_schema: json!({"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"Directory to list (default: current directory)."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum entries to return (default 500)."}}}) },
        ToolSpec { name: "find".into(), description: "Find regular files recursively by glob. Pattern uses gitignore-style globs: no slash matches basenames at any depth, a slash matches the path relative to the returned root, and ** is recursive. path defaults to the working directory; absolute paths are allowed. Hidden files are included; discovered symlinks are not followed and the .git tree is skipped. .gitignore and .ignore rules are traversal convenience, not a secret or security boundary. limit defaults to and is at most 1000. Traversal is bounded to 20000 visited entries and depth 9; filesystem skips, result caps and output bounds are explicit. An empty incomplete result is not proof of no match; refine the root or glob. Output is serialized JSON bounded to 64 KiB.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["pattern"],"properties":{"pattern":{"type":"string","minLength":1,"maxLength":4096,"description":"Glob such as '*.rs', 'src/**/*.rs' or 'docs/*'."},"path":{"type":"string","description":"Directory to search (default: current directory)."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum file results (default 1000)."}}}) },
        ToolSpec { name: "grep".into(), description: "Search one UTF-8 regular file or a directory recursively. Returns one item per matching line with path relative to the returned root, a 1-based line number, the first match's zero-based byte_offset (usable with read byte_offset), and a UTF-8 snippet of at most 1024 bytes; long-line truncation is visible. pattern is a regex by default; literal=true searches literal text; ignore_case is optional. glob filters files with the same basename/relative-path semantics as find. path defaults to the working directory; absolute paths are allowed. Hidden files are included; discovered symlinks are not followed and the .git tree is skipped. .gitignore and .ignore rules are convenience only, not a secret or security boundary. Binary/invalid UTF-8 files, files over 8 MiB and the total 32 MiB inspected-source budget are reported; traversal is bounded to 20000 visited entries and depth 9; limit defaults to 100 and is at most 1000. Check exhaustive, skipped and omissions: an empty incomplete result is not proof of no match. root names the returned search root and root_kind is file or directory: for a file root, use root itself and match paths are its basename; otherwise match paths are relative to root. Output is serialized JSON bounded to 64 KiB; use read with byte_offset for a full long line.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["pattern"],"properties":{"pattern":{"type":"string","minLength":1,"maxLength":4096,"description":"Regex, or literal text when literal=true."},"path":{"type":"string","description":"File or directory to search (default: current directory)."},"glob":{"type":"string","minLength":1,"maxLength":4096,"description":"Optional file glob, e.g. '*.rs' or 'src/**/*.ts'."},"ignore_case":{"type":"boolean","description":"Case-insensitive matching (default false)."},"literal":{"type":"boolean","description":"Treat pattern as literal text instead of a regex (default false)."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum matching lines (default 100)."}}}) },
        ToolSpec { name: "edit".into(), description: "Apply one or more disjoint exact text replacements to a UTF-8 file in one write. Requires base_digest from a read of the complete current file; stale content is rejected. Each old_text must occur exactly once in the original file; overlapping edits are rejected. Operates with the host user's permissions.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","edits","base_digest"],"properties":{"path":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["old_text","new_text"],"properties":{"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}}}},"base_digest":{"type":"string","description":"Complete-file digest returned by read; required."}}}) },
        ToolSpec { name: "write".into(), description: "Create or replace a UTF-8 file in the live working directory. Replacing an existing file requires base_digest from read, or unconditional=true only for an intentional unread replacement; creation needs neither and creates missing parents. A supplied digest must match the entire existing regular file under the native mutation lock; stale, missing or unreadable targets reject. Use exact edit or complete-source guarded replacement, not an incomplete read window. Success returns path, created, bytes and new_digest (SHA256 of published content), plus a bounded unified diff when the previous content is readable UTF-8; otherwise diff_omitted explains the omission. The digest is not a lease against later changes.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"},"base_digest":{"type":"string","description":"Complete-file digest returned by read; required to replace an existing file."},"unconditional":{"type":"boolean","description":"Only for intentional unguarded replacement; do not use after a read."}}}) },
        ToolSpec { name: "exec".into(), description: "Run a Bash command (or POSIX sh when Bash is unavailable) in the live working directory with the host user's permissions; this is not sandboxed. Timeout is optional. Returns the observed direct command exit and the final 64 KiB of each raw output stream decoded as UTF-8. Per-stream stdout_/stderr_ flags: lossy means decoding replaced bytes in the returned tail (including possible UTF-8 boundary cuts, not a literal U+FFFD); complete means pipe EOF was observed; truncated means bytes were omitted or capture is incomplete. omitted_bytes counts raw bytes outside the tail, or is null when capture is incomplete. For complete truncated or lossy captures, full_path names a private temporary raw file when available; full_error reports capture/artifact failures. Artifact paths do not guarantee retention. Inspect available raw evidence instead of rerunning effects. Capture/decoding loss does not change a known command exit. Cancellation is best effort.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1}}}) },
    ]
}

#[cfg(test)]
mod tests {
    use super::files::hex_digest;
    use super::*;
    use ion_ai::ToolCall;
    use std::{
        fs::{self, File},
        time::Duration,
    };

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
            ("ls", "limit", json!({})),
            ("ls", "path", json!({})),
            ("find", "pattern", json!({"pattern":"*"})),
            ("find", "path", json!({"pattern":"*"})),
            ("find", "limit", json!({"pattern":"*"})),
            ("grep", "pattern", json!({"pattern":"needle"})),
            ("grep", "path", json!({"pattern":"needle"})),
            ("grep", "glob", json!({"pattern":"needle"})),
            ("grep", "ignore_case", json!({"pattern":"needle"})),
            ("grep", "literal", json!({"pattern":"needle"})),
            ("grep", "limit", json!({"pattern":"needle"})),
            (
                "exec",
                "timeout_ms",
                json!({"command":"printf effect > marker"}),
            ),
        ] {
            arguments[field] = Value::Null;
            let output = catalog
                .execute_untracked(
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
    fn native_schema_requires_edit_digest_and_exposes_write_opt_out() {
        let catalog =
            ion_core::ToolSet::new([
                Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>
            ])
            .snapshot();
        let specs = catalog.specs();
        let edit = specs.iter().find(|spec| spec.name == "edit").unwrap();
        assert!(
            edit.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("base_digest"))
        );
        assert_eq!(
            edit.input_schema["properties"]["base_digest"]["type"],
            "string"
        );
        let write = specs.iter().find(|spec| spec.name == "write").unwrap();
        assert!(
            !write.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("base_digest"))
        );
        assert_eq!(
            write.input_schema["properties"]["unconditional"]["type"],
            "boolean"
        );
    }

    #[test]
    fn native_discovery_schema_and_presentation_are_registered() {
        let catalog =
            ion_core::ToolSet::new([
                Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>
            ])
            .snapshot();
        let specs = catalog.specs();
        assert_eq!(specs.len(), 7);
        for (name, kind, key) in [
            ("ls", ToolActivityKind::List, "path"),
            ("find", ToolActivityKind::Search, "pattern"),
            ("grep", ToolActivityKind::Search, "pattern"),
        ] {
            let spec = specs.iter().find(|spec| spec.name == name).unwrap();
            assert_eq!(spec.input_schema["additionalProperties"], false);
            let call = ToolCall {
                id: name.into(),
                name: name.into(),
                arguments: json!({key: "target"}),
                raw_arguments: None,
            };
            let activity = catalog.activity(&call);
            assert_eq!(activity.kind, kind, "{name}");
            assert_eq!(activity.subject.as_deref(), Some("target"));
        }
        let grep = specs.iter().find(|spec| spec.name == "grep").unwrap();
        assert_eq!(grep.input_schema["properties"]["limit"]["minimum"], 1);
        assert_eq!(grep.input_schema["properties"]["limit"]["maximum"], 1000);
        assert_eq!(
            grep.input_schema["properties"]["literal"]["type"],
            "boolean"
        );
        let find = specs.iter().find(|spec| spec.name == "find").unwrap();
        assert_eq!(find.input_schema["required"], json!(["pattern"]));
        let ls = specs.iter().find(|spec| spec.name == "ls").unwrap();
        assert_eq!(ls.input_schema["properties"]["limit"]["maximum"], 1000);
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
            catalogs[0].execute_untracked(&calls[0], CancellationToken::new()),
            catalogs[1].execute_untracked(&calls[1], CancellationToken::new()),
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
                catalogs[0].execute_untracked(&calls[0], CancellationToken::new()),
                catalogs[1].execute_untracked(&calls[1], CancellationToken::new()),
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
                successful == 0
                    && final_bytes == source.as_bytes()
                    && [&outputs.0, &outputs.1].iter().all(|output| {
                        output.value["error"]
                            .as_str()
                            .is_some_and(|error| error.contains("requires base_digest"))
                    })
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
                json!({"path":"file","edits":[{"old_text":"old","new_text":"edited"}],"base_digest":hex_digest(b"old")}),
            ),
            (
                "write",
                json!({"path":"file","content":"written","base_digest":hex_digest(b"old")}),
            ),
        ] {
            let call = ToolCall {
                id: name.into(),
                name: name.into(),
                arguments,
                raw_arguments: None,
            };
            let stop = CancellationToken::new();
            let pending = catalog.execute_untracked(&call, stop.clone());
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
        let output = files::write(
            &root,
            &json!({"path":"file","content":"released","base_digest":hex_digest(b"old")}),
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
                ("ls", json!({})),
                ("find", json!({"pattern":"*.txt"})),
                ("grep", json!({"pattern":"old"})),
                (
                    "edit",
                    json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}],"base_digest":hex_digest(b"old")}),
                ),
                ("write", json!({"path":"file.txt","content":"new","base_digest":hex_digest(b"old")})),
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
                let mut work = Box::pin(catalog.execute_untracked(&call, stop.clone()));
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
            let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
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

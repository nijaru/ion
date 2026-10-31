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
    ToolActivityKind, ToolDefinition, ToolExecutor, ToolExposure, ToolOutput, ToolPresentation,
    ToolRegistration, ToolSource,
};

pub(crate) mod command;
mod discovery;
mod files;
mod text_read;

// A rule edit and a dependent native mutation must not both prepare against the
// same old rules in concurrent Code Mode/child Sessions. This is process-local
// coordination, not a lock against external editors, exec or other processes.
static INSTRUCTION_MUTATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct LocalTools {
    cwd: PathBuf,
    project_instructions: bool,
    environment: Arc<crate::CommandEnvironment>,
}
impl LocalTools {
    pub fn new(cwd: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::with_environment(cwd, Arc::new(crate::CommandEnvironment::inherited()))
    }

    pub fn with_environment(
        cwd: impl AsRef<Path>,
        environment: Arc<crate::CommandEnvironment>,
    ) -> std::io::Result<Self> {
        let cwd = cwd.as_ref().canonicalize()?;
        if !cwd.is_dir() {
            return Err(std::io::Error::other(
                "working directory is not a directory",
            ));
        }
        Ok(Self {
            cwd,
            project_instructions: false,
            environment,
        })
    }

    /// Opt in to coding instruction discovery from explicit native file targets.
    /// Plain LocalTools and application-owned Resources inherit no ambient rules.
    pub fn with_project_instructions(mut self) -> Self {
        self.project_instructions = true;
        self
    }

    /// Run a user's direct command through the native shell execution path.
    pub async fn run_user_shell(&self, command: &str, stop: CancellationToken) -> ToolOutput {
        command::run(
            &self.environment,
            &self.cwd,
            &json!({"command": command}),
            stop,
        )
        .await
    }
}

impl crate::UserShell for LocalTools {
    fn prepare(&self, command: &str) -> anyhow::Result<crate::PreparedShell> {
        let tools = Self::with_environment(&self.cwd, self.environment.clone())?;
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
                        result_schema: None,
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

impl LocalOperation {
    fn instruction_target(
        self,
        cwd: &Path,
        arguments: &Value,
    ) -> Option<(PathBuf, crate::project_instructions::TargetKind)> {
        use crate::project_instructions::TargetKind;
        let kind = match self {
            Self::Read | Self::Edit | Self::Write => TargetKind::File,
            Self::Ls | Self::Find => TargetKind::Directory,
            Self::Grep => TargetKind::SearchRoot,
            Self::Exec => return None,
        };
        let raw = match arguments.get("path") {
            Some(Value::String(raw)) if !raw.is_empty() && !raw.contains('\0') => raw.as_str(),
            None if matches!(self, Self::Ls | Self::Find | Self::Grep) => ".",
            _ => return None,
        };
        Some((cwd.join(raw), kind))
    }
}

struct LocalExecutor {
    tools: Arc<LocalTools>,
    operation: LocalOperation,
}

/// Retain every filesystem worker through settlement, including instruction
/// discovery. A panic cannot become a plausible observed tool error.
async fn file_work<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|failure| {
            if failure.is_panic() {
                std::panic::resume_unwind(failure.into_panic());
            }
            panic!("native file worker stopped without an observation: {failure}");
        })
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
                return command::run(
                    &self.tools.environment,
                    &self.tools.cwd,
                    &call.arguments,
                    stop,
                )
                .await;
            }
            let mutating = matches!(self.operation, LocalOperation::Edit | LocalOperation::Write);
            let instruction_mutation = if self.tools.project_instructions && mutating {
                Some(tokio::select! {
                    biased;
                    _ = stop.cancelled() => return error("cancelled before instruction preparation"),
                    guard = INSTRUCTION_MUTATIONS.lock() => guard,
                })
            } else {
                None
            };
            let scope: Option<crate::project_instructions::ScopedInstructions> = if self
                .tools
                .project_instructions
            {
                if let Some((target, kind)) = self
                    .operation
                    .instruction_target(&self.tools.cwd, &call.arguments)
                {
                    let tools = self.tools.clone();
                    let stop = stop.clone();
                    match file_work(move || {
                        crate::project_instructions::load_scoped(&tools.cwd, &target, kind, &stop)
                    })
                    .await
                    {
                        Ok(scope) => Some(scope),
                        Err(failure) => {
                            return error(format!("instruction discovery failed: {failure:#}"));
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let requires_review = scope
                .as_ref()
                .is_some_and(|scope| scope.requires_review(invocation.model_tool_results()));
            if mutating
                && requires_review
                && let Some(scope) = scope
            {
                let mut output = error(
                    "New or changed scoped project instructions require review. No file mutation or parent creation was performed. Reconsider this change in a later model request; do not blindly retry it inside the same batch or Code Mode program. Return the complete project_instructions observation to the model from Code Mode.",
                );
                output.value["project_instructions"] = scope.observation;
                return output;
            }
            let (mutation_context, observation) = if mutating {
                (scope, None)
            } else {
                (
                    None,
                    scope
                        .filter(|_| requires_review)
                        .map(|scope| scope.observation),
                )
            };
            let operation = self.operation;
            let tools = self.tools.clone();
            let arguments = call.arguments.clone();
            // Filesystem calls, hashing and image normalization must not block
            // the coding operation's async poll. Retain the worker through
            // settlement; cancellation cannot interrupt an in-flight syscall.
            let mut output = file_work(move || {
                // The actual worker owns the fence even if a low-level caller
                // drops its future. Core still retains and joins this work.
                let _instruction_mutation = instruction_mutation;
                if stop.is_cancelled() {
                    error("cancelled before tool start")
                } else {
                    match operation {
                        LocalOperation::Read => files::read(&tools.cwd, &arguments, &stop),
                        LocalOperation::Ls => discovery::ls(&tools.cwd, &arguments, &stop),
                        LocalOperation::Find => discovery::find(&tools.cwd, &arguments, &stop),
                        LocalOperation::Grep => discovery::grep(&tools.cwd, &arguments, &stop),
                        LocalOperation::Edit => {
                            files::edit(&tools.cwd, &arguments, &stop, mutation_context.as_ref())
                        }
                        LocalOperation::Write => {
                            files::write(&tools.cwd, &arguments, &stop, mutation_context.as_ref())
                        }
                        LocalOperation::Exec => unreachable!("shell uses async execution"),
                    }
                }
            })
            .await;
            if let Some(observation) = observation {
                output.value["project_instructions"] = observation;
            }
            output
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
mod tests;

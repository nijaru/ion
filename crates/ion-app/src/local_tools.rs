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
    pub async fn run_user_shell(
        &self,
        command: &str,
        stop: CancellationToken,
        artifacts: Option<ion_core::ArtifactSink>,
        progress: Option<crate::ShellOutputObserver>,
    ) -> ToolOutput {
        command::run(
            &self.environment,
            &self.cwd,
            &json!({"command": command}),
            stop,
            artifacts,
            progress,
        )
        .await
    }
}

impl crate::UserShell for LocalTools {
    fn prepare(&self, command: &str) -> anyhow::Result<crate::PreparedShell> {
        let tools = Self::with_environment(&self.cwd, self.environment.clone())?;
        let command = command.to_owned();
        Ok(Box::new(move |stop, artifacts, progress| {
            Box::pin(async move {
                tools
                    .run_user_shell(&command, stop, Some(artifacts), progress)
                    .await
            })
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
                let progress = invocation.output_progress().map(|report| {
                    let observe: crate::ShellOutputObserver =
                        Arc::new(move |stream, bytes| report.append(stream, bytes));
                    observe
                });
                return command::run(
                    &self.tools.environment,
                    &self.tools.cwd,
                    &call.arguments,
                    stop,
                    invocation.artifact_sink().cloned(),
                    progress,
                )
                .await;
            }
            // Keep the admitted call unchanged. Both instruction review and I/O
            // consume this one prepared path from the captured Host environment.
            let mut arguments = call.arguments.clone();
            if let Some(raw) = arguments.get("path").and_then(Value::as_str) {
                match crate::expand_home_path(Path::new(raw), self.tools.environment.home()) {
                    Ok(std::borrow::Cow::Borrowed(_)) => {}
                    Ok(std::borrow::Cow::Owned(path)) => {
                        let Some(path) = path.to_str() else {
                            return error(
                                "expanded home-relative path is not representable as UTF-8",
                            );
                        };
                        arguments["path"] = Value::String(path.to_owned());
                    }
                    Err(failure) => return error(failure.to_string()),
                }
            }
            let mutating = matches!(self.operation, LocalOperation::Edit | LocalOperation::Write);
            // Physical-target preparation discards directory suffixes and parent
            // tails. Reject them before they can become a creatable regular file.
            if mutating
                && let Some(raw) = arguments.get("path").and_then(Value::as_str)
                && !raw.is_empty()
                && (raw.ends_with('/')
                    || raw.ends_with("/.")
                    || Path::new(raw).file_name().is_none())
            {
                return error("file mutation path names a directory");
            }
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
                    .instruction_target(&self.tools.cwd, &arguments)
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
        artifacts: Vec::new(),
        value,
        images: Vec::new(),
        is_error: false,
    }
}
fn error(message: impl Into<String>) -> ToolOutput {
    ToolOutput {
        artifacts: Vec::new(),
        value: json!({"error": message.into()}),
        images: Vec::new(),
        is_error: true,
    }
}
fn specs() -> Vec<ToolSpec> {
    let path_description = "Relative to the working directory, absolute, or ~ / ~/... using the Host's captured absolute HOME. Use ./~/... for a literal tilde directory. No other shell expansion.";
    vec![
        ToolSpec { name: "read".into(), description: "Read UTF-8 text or attach an image (JPEG, PNG, GIF, WebP). For text, follow next_line as offset to continue. For oversized lines, switch to byte ranges and follow next_byte as byte_offset. A base_digest covers the whole file, even on a partial read. Prefer exact edits or read all pages before replacing the file.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","description":path_description},"offset":{"type":"integer","minimum":1,"description":"One-based starting line (default 1). Pass the prior result's next_line to continue. Omit for images."},"limit":{"type":"integer","minimum":1,"maximum":2000,"description":"Maximum complete lines to return (default and maximum 2000); the 65536-byte output bound may stop earlier. Omit for images."},"byte_offset":{"type":"integer","minimum":0,"description":"Zero-based UTF-8 byte offset (default 0). Cannot combine byte and line selectors. Start at a character boundary or use the previous next_byte. Omit for images."},"byte_limit":{"type":"integer","minimum":4,"maximum":65536,"description":"Maximum UTF-8 bytes (default 65536), independent of physical lines. End rounds down to a character boundary; continue at next_byte. Omit for images."}}}) },
        ToolSpec { name: "ls".into(), description: "List one directory, including hidden entries. Returns sorted names and their types. If exhaustive is false, check omissions before concluding an entry is absent.".into(), input_schema: json!({"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":format!("Directory to list (default: current directory). {path_description}")},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum entries to return (default 500)."}}}) },
        ToolSpec { name: "find".into(), description: "Find regular files by glob. No slash matches basenames; a slash matches paths relative to the search root; ** is recursive. Includes hidden files, respects .gitignore and .ignore, and skips .git and discovered symlinks. If exhaustive is false, check omissions and narrow the search before concluding there are no matches.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["pattern"],"properties":{"pattern":{"type":"string","minLength":1,"maxLength":4096,"description":"Glob such as '*.rs', 'src/**/*.rs' or 'docs/*'."},"path":{"type":"string","description":format!("Directory to search (default: current directory). {path_description}")},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum file results (default 1000)."}}}) },
        ToolSpec { name: "grep".into(), description: "Search UTF-8 text in one file or recursively. Returns matching lines or snippets with line numbers and byte offsets for read. For root_kind=file, read root itself; otherwise resolve each match path relative to root. Uses find's glob and traversal rules. Non-text files are excluded from recursive search. If exhaustive is false, check omissions and narrow the search before concluding there are no matches.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["pattern"],"properties":{"pattern":{"type":"string","minLength":1,"maxLength":4096,"description":"Regex, or literal text when literal=true."},"path":{"type":"string","description":format!("File or directory to search (default: current directory). {path_description}")},"glob":{"type":"string","minLength":1,"maxLength":4096,"description":"Optional file glob, e.g. '*.rs' or 'src/**/*.ts'."},"ignore_case":{"type":"boolean","description":"Case-insensitive matching (default false)."},"literal":{"type":"boolean","description":"Treat pattern as literal text instead of a regex (default false)."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum matching lines (default 100)."}}}) },
        ToolSpec { name: "edit".into(), description: "Read the target first, then replace exact text in one UTF-8 file. Each old_text must match once and edits must not overlap. Use base_digest only when the edit depends on the whole-file snapshot. Returns a bounded unified diff.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","edits"],"properties":{"path":{"type":"string","description":path_description},"edits":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["old_text","new_text"],"properties":{"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}}}},"base_digest":{"type":"string","description":"Optional whole-file digest returned by read. Rejects any change since that read."}}}) },
        ToolSpec { name: "write".into(), description: "Create or replace a UTF-8 file, creating missing parents. For replacement, read the complete file and supply base_digest to reject stale writes; never reconstruct a file from a partial read. Use unconditional=true only for intentional unread replacement. Returns new_digest and a bounded diff when available.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["path","content"],"properties":{"path":{"type":"string","description":path_description},"content":{"type":"string"},"base_digest":{"type":"string","description":"Complete-file digest returned by read; required for replacement unless unconditional is true."},"unconditional":{"type":"boolean","description":"Only for intentional unguarded replacement; do not use after a read."}}}) },
        ToolSpec { name: "exec".into(), description: "Run Bash (or POSIX sh if unavailable) in the working directory with host permissions, not in a sandbox. Returns the direct command exit and a UTF-8 preview of stdout and stderr (up to 64 KiB of raw bytes per stream). Longer streams show the beginning and end with an omission marker. When present, stdout_file and stderr_file point to the retained bytes in temporary files. Capture flags distinguish missing bytes and lossy decoding from command failure. When artifacts are retained, direct replies put command fields under output beside entry and artifacts. Use history action artifact with that entry and the stdout_artifact or stderr_artifact index instead of rerunning the command.".into(), input_schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1,"description":"Optional timeout in milliseconds; no default timeout."}}}) },
    ]
}

#[cfg(test)]
mod tests;

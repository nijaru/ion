//! Tools, registry, executor, and core tool implementations.
//!
//! Tools are the runtime's "tool service" (see `ai/DESIGN.md`). A tool
//! exposes a [`ToolSpec`] (name, description, JSON-Schema input) and a
//! [`Tool::call`] that produces a [`ToolOutcome`]. The [`ToolRegistry`]
//! owns registered tools and validates arguments before dispatch.
//! Execution happens in a spawned tool task: the controller never awaits
//! tool I/O on its loop.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
#[cfg(unix)]
use nix::fcntl::{OFlag, open, openat};
#[cfg(unix)]
use nix::sys::stat::{Mode, mkdirat};
use regex::Regex;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ids::OperationId;
use crate::process::ProcessGuard;

/// Identifier for an in-flight tool call. Monotonic per provider.
pub type ToolCallId = u64;

/// Hard upper bound for the semantic text sent to the model for one tool
/// result. The complete byte stream, when available, is retained separately
/// as a bounded artifact.
const MODEL_RESULT_MAX_BYTES: usize = 16 * 1024;
const MODEL_RESULT_MAX_LINES: usize = MODEL_RESULT_MAX_BYTES;
const MODEL_SAMPLE_MAX_BYTES: usize = 12 * 1024;
const MODEL_SAMPLE_HEAD_BYTES: usize = MODEL_SAMPLE_MAX_BYTES / 2;
const MODEL_SAMPLE_TAIL_BYTES: usize = MODEL_SAMPLE_MAX_BYTES - MODEL_SAMPLE_HEAD_BYTES;
const ARTIFACT_MAX_BYTES: u64 = 16 * 1024 * 1024;
const OUTPUT_CHUNK_BYTES: usize = 8 * 1024;

/// A durable locator for raw output externalized from the model-visible
/// result. The URI is semantic; the backing path remains store-owned.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolArtifact {
    pub uri: String,
    pub stored_bytes: u64,
    pub total_bytes: u64,
    pub sha256: String,
    pub truncated: bool,
}

/// Static description of a tool: name, short doc, and JSON-Schema for its
/// input object. The schema's top-level `"required"` array drives argument
/// validation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A complete tool call requested by a provider, admitted through the
/// runtime's policy/effect path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub operation_id: OperationId,
    pub call_id: ToolCallId,
    pub name: String,
    pub arguments: Value,
}

/// Settlement of one tool effect, recorded as a semantic session entry
/// (DESIGN.md §16.4): exactly what the model will see.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ToolResult {
    Ok {
        call_id: ToolCallId,
        output: String,
        artifact: Option<ToolArtifact>,
    },
    Err {
        call_id: ToolCallId,
        error: String,
        artifact: Option<ToolArtifact>,
    },
}

impl ToolResult {
    /// Classify one tool outcome at the persistence boundary. Every
    /// runtime-settled result stores bounded model text; native shell
    /// output may additionally carry a lossless artifact reference.
    pub(crate) fn from_outcome(call_id: ToolCallId, outcome: ToolOutcome) -> Self {
        let output = bounded_stored_output(outcome.output, outcome.artifact.is_some());
        if outcome.is_error {
            Self::Err {
                call_id,
                error: output,
                artifact: outcome.artifact,
            }
        } else {
            Self::Ok {
                call_id,
                output,
                artifact: outcome.artifact,
            }
        }
    }

    /// The call id this result answers.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        match self {
            Self::Ok { call_id, .. } | Self::Err { call_id, .. } => *call_id,
        }
    }

    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Render the result to a single string (success output or error text).
    #[must_use]
    pub fn into_text(self) -> String {
        self.model_text()
    }

    /// The exact bounded text projected to the model, including the stable
    /// locator for any externalized raw output.
    #[must_use]
    pub fn model_text(&self) -> String {
        let (text, artifact) = match self {
            Self::Ok {
                output, artifact, ..
            } => (output.as_str(), artifact.as_ref()),
            Self::Err {
                error, artifact, ..
            } => (error.as_str(), artifact.as_ref()),
        };
        let suffix = artifact.map_or_else(String::new, |artifact| {
            format!(
                "\nfull result: {} ({} of {} bytes, sha256={}, {})",
                artifact.uri,
                artifact.stored_bytes,
                artifact.total_bytes,
                artifact.sha256,
                if artifact.truncated {
                    "artifact truncated at the configured limit"
                } else {
                    "complete artifact"
                }
            )
        });
        let combined = format!("{text}{suffix}");
        if combined.len() <= MODEL_RESULT_MAX_BYTES {
            combined
        } else {
            truncate_tail(&combined, MODEL_RESULT_MAX_BYTES, MODEL_RESULT_MAX_BYTES)
                .unwrap_or_default()
        }
    }

    /// Bounded display text for frontend rendering: the tail of the
    /// bounded model result, never the full body. The full raw shell
    /// stream, when available, is addressed by the artifact locator.
    #[must_use]
    pub fn display_preview(&self) -> Option<String> {
        let text = self.model_text();
        truncate_tail(&text, PREVIEW_MAX_LINES, PREVIEW_MAX_BYTES)
    }
}

fn bounded_stored_output(mut output: String, has_artifact: bool) -> String {
    if output.len() <= MODEL_RESULT_MAX_BYTES {
        return output;
    }
    let marker = if has_artifact {
        ""
    } else {
        "[tool output abbreviated; full result was not externalized]\n"
    };
    let budget = MODEL_RESULT_MAX_BYTES.saturating_sub(marker.len());
    let sampled = truncate_tail(&output, MODEL_RESULT_MAX_LINES, budget).unwrap_or_default();
    output.clear();
    output.push_str(marker);
    output.push_str(&sampled);
    output
}

/// Frontend preview bounds (pi-parity: tail-truncated so errors and
/// results stay visible).
const PREVIEW_MAX_LINES: usize = 20;
const PREVIEW_MAX_BYTES: usize = 2 * 1024;

fn truncate_tail(text: &str, max_lines: usize, max_bytes: usize) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = lines.iter().rev().take(max_lines).rev().copied().collect();
    let mut dropped = lines.len() - kept.len();
    // The byte bound is a hard total, truncation marker included: drop
    // whole lines from the head while over budget, then cut a lone
    // oversized line on a char boundary so one minified line cannot
    // bypass the limit.
    loop {
        let marker_len = if dropped > 0 {
            format!("… {dropped} earlier lines\n").len()
        } else {
            0
        };
        let body_len: usize = kept
            .iter()
            .map(|l| l.len() + 1)
            .sum::<usize>()
            .saturating_sub(1);
        if marker_len + body_len <= max_bytes {
            break;
        }
        if kept.len() > 1 {
            kept.remove(0);
            dropped += 1;
            continue;
        }
        let budget = max_bytes.saturating_sub(marker_len);
        let line = kept[0];
        let mut end = 0;
        for (index, ch) in line.char_indices() {
            if index + ch.len_utf8() > budget {
                break;
            }
            end = index + ch.len_utf8();
        }
        kept[0] = &line[..end];
        break;
    }
    if dropped > 0 {
        Some(format!("… {dropped} earlier lines\n{}", kept.join("\n")))
    } else {
        Some(kept.join("\n"))
    }
}

/// Outcome of tool execution, before it is classified into a [`ToolResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
    pub artifact: Option<ToolArtifact>,
}

impl ToolOutcome {
    #[must_use]
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            artifact: None,
        }
    }

    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            output: message.into(),
            is_error: true,
            artifact: None,
        }
    }

    #[must_use]
    fn with_artifact(mut self, artifact: Option<ToolArtifact>) -> Self {
        self.artifact = artifact;
        self
    }
}

/// One contract for native, MCP, and extension tools.
///
/// Object-safe so the registry can hold tools as `Arc<dyn Tool>`.
pub trait Tool: Send + Sync {
    /// Static description of the tool and its inputs.
    fn spec(&self) -> ToolSpec;

    /// Execute the tool. `arguments` has already been validated against
    /// `spec().input_schema`; the tool may still produce a runtime error.
    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
}

/// A callable tool wrapped as a trait object with its spec cached.
#[derive(Clone)]
struct ToolEntry {
    tool: Arc<dyn Tool>,
    spec: ToolSpec,
    recovery_class: RecoveryClass,
}

/// How an unresolved effect of this tool may be settled after process
/// loss (DESIGN.md §12.2). Recorded with the effect intent before
/// execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecoveryClass {
    /// Repeating the effect cannot duplicate an external mutation.
    ReplaySafe,
    /// Postconditions can be inspected before re-execution.
    Reconcile,
    /// Repeating may duplicate an external mutation; unresolved means
    /// indeterminate, never automatic replay.
    NeverReplay,
}

/// The effective target of one tool invocation, canonicalized before
/// the policy sees it (DESIGN.md §17.3): the policy approves exactly
/// what the executor will use, never a raw model string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalTarget {
    /// Absolute, lexically normalized path (cwd-relative arguments are
    /// resolved against the tool registry's working directory).
    Path { path: std::path::PathBuf },
    /// The exact shell command the executor will run.
    Command { command: String },
    /// A registered non-native tool (MCP/extension): the invocation
    /// goes through its owning transport, not local I/O (§19.2).
    Remote { tool: String },
}

/// Registry and executor for tools. Holds an `Arc<Path>` so a tool task
/// can clone the working directory cheaply before invoking a tool.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The file state used for reconciliation and execution-time precondition
/// checks. The identity supplements the content hash so replacing a file
/// with another file containing the same bytes is still detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileSnapshot {
    pub hash: [u8; 32],
    pub identity: Option<String>,
}

fn snapshot_json(snapshot: &FileSnapshot) -> Value {
    let mut value = json!({
        "exists": true,
        "hash": hex(&snapshot.hash),
    });
    if let Some(identity) = &snapshot.identity {
        value["identity"] = Value::String(identity.clone());
    }
    value
}

#[cfg(unix)]
fn metadata_identity(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn metadata_identity(_: &std::fs::Metadata) -> Option<String> {
    None
}

/// Reconciliation evidence for one write/edit invocation (DESIGN.md
/// §12.3), computed at admission before the effect intent is
/// committed: target path, preimage existence/hash, and the intended
/// postimage hash. Pure with respect to the file's future: only reads.
pub async fn reconciliation_evidence(
    cwd: &Path,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let path_arg = arguments
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string argument: path".to_owned())?;
    let full = resolve_under(cwd, path_arg)?;
    let preimage = match file_snapshot(cwd, Path::new(path_arg), false).await? {
        Some(snapshot) => snapshot_json(&snapshot),
        None => json!({ "exists": false }),
    };
    let postimage: Vec<u8> = match name {
        "write" => arguments
            .get("contents")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing string argument: contents".to_owned())?
            .as_bytes()
            .to_vec(),
        "edit" => {
            let old_str = arguments
                .get("old_str")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing string argument: old_str".to_owned())?;
            let new_str = arguments
                .get("new_str")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let original = read_secure_text(cwd, Path::new(path_arg), false).await?;
            if !original.contains(old_str) {
                return Err("old_str not found in file".to_owned());
            }
            original.replacen(old_str, new_str, 1).into_bytes()
        }
        other => return Err(format!("tool {other} takes no reconciliation evidence")),
    };
    use sha2::{Digest, Sha256};
    let postimage_hash = Sha256::digest(&postimage);
    Ok(serde_json::json!({
        "path": full,
        "preimage": preimage,
        "postimage_hash": hex(postimage_hash.as_slice()),
    }))
}

/// What recovery may do with a pending Reconcile effect, given the
/// recorded evidence and the file state found after process loss
/// (DESIGN.md §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileVerdict {
    /// The postimage is already on disk: the effect completed before
    /// the crash; settle without repeating.
    AlreadyApplied,
    /// The file still matches the recorded preimage: safe to execute
    /// the intended write exactly once.
    SafeToExecute,
    /// The file matches neither: conflict; never overwrite.
    Conflict,
    /// No usable evidence (effect predates evidence or is malformed).
    Unknown,
}

fn classify_reconciliation_parts(
    evidence: &Value,
    current_hash: Option<String>,
    current_identity: Option<&str>,
) -> ReconcileVerdict {
    if evidence.is_null() {
        return ReconcileVerdict::Unknown;
    }
    let postimage = evidence.get("postimage_hash").and_then(|v| v.as_str());
    if postimage == current_hash.as_deref() && postimage.is_some() {
        return ReconcileVerdict::AlreadyApplied;
    }
    let Some(preimage) = evidence.get("preimage") else {
        return ReconcileVerdict::Unknown;
    };
    let preimage_exists = preimage.get("exists").and_then(|v| v.as_bool()) == Some(true);
    if !preimage_exists {
        return if current_hash.is_none() {
            ReconcileVerdict::SafeToExecute
        } else {
            ReconcileVerdict::Conflict
        };
    }
    let preimage_matches = preimage.get("hash").and_then(|v| v.as_str()) == current_hash.as_deref();
    let identity_matches = match preimage.get("identity").and_then(|v| v.as_str()) {
        Some(expected) => current_identity == Some(expected),
        None => true,
    };
    if preimage_matches && identity_matches {
        ReconcileVerdict::SafeToExecute
    } else {
        ReconcileVerdict::Conflict
    }
}

/// Classify a pending Reconcile effect against a hash-only file state.
/// Older persisted evidence may not contain a file identity, so this
/// compatibility helper retains the original hash-only classification.
#[cfg(test)]
#[must_use]
pub fn classify_reconciliation(evidence: &Value, current: Option<[u8; 32]>) -> ReconcileVerdict {
    classify_reconciliation_parts(evidence, current.map(|hash| hex(&hash)), None)
}

/// Classify a pending Reconcile effect using the current file identity.
/// Identity-aware evidence refuses to replay over a same-content replacement.
#[must_use]
pub(crate) fn classify_reconciliation_snapshot(
    evidence: &Value,
    current: Option<&FileSnapshot>,
) -> ReconcileVerdict {
    classify_reconciliation_parts(
        evidence,
        current.map(|snapshot| hex(&snapshot.hash)),
        current.and_then(|snapshot| snapshot.identity.as_deref()),
    )
}

fn precondition_matches(evidence: &Value, current: Option<&FileSnapshot>) -> bool {
    let Some(preimage) = evidence.get("preimage") else {
        return false;
    };
    let exists = preimage.get("exists").and_then(|v| v.as_bool()) == Some(true);
    if !exists {
        return current.is_none();
    }
    let Some(current) = current else {
        return false;
    };
    let hash_matches =
        preimage.get("hash").and_then(|v| v.as_str()) == Some(hex(&current.hash).as_str());
    let identity_matches = match preimage.get("identity").and_then(|v| v.as_str()) {
        Some(expected) => current.identity.as_deref() == Some(expected),
        None => true,
    };
    hash_matches && identity_matches
}

/// Registry and executor for tools. Holds an `Arc<Path>` so a tool task
/// can clone the working directory cheaply before invoking a tool.
#[derive(Clone)]
pub struct ToolRegistry {
    cwd: Arc<Path>,
    entries: Arc<HashMap<String, ToolEntry>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_cwd(cwd)
    }
}

impl ToolRegistry {
    /// Build a registry rooted at `cwd` with the core tool set registered.
    #[must_use]
    pub fn with_cwd(cwd: impl AsRef<Path>) -> Self {
        let cwd: Arc<Path> = Arc::from(cwd.as_ref());
        let entries = core_tools(&cwd);
        Self {
            cwd,
            entries: Arc::new(entries),
        }
    }

    /// A read-only registry rooted at `cwd`: the research-child
    /// capability set (§20.4). Structural narrowing - write paths are
    /// absent, not denied at the gate.
    #[must_use]
    pub fn read_only(cwd: impl AsRef<Path>) -> Self {
        let cwd: Arc<Path> = Arc::from(cwd.as_ref());
        let mut all = core_tools(&cwd);
        all.retain(|name, _| matches!(name.as_str(), "read" | "search" | "find"));
        Self {
            cwd,
            entries: Arc::new(all),
        }
    }

    /// The directory tool paths are resolved against.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// All registered tool specs, ordered by name. The order is part of
    /// the capability snapshot, so it must be deterministic across
    /// processes for prompt-prefix stability (DESIGN.md P9, §14.4).
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self.entries.values().map(|e| e.spec.clone()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Look up a tool's spec by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.entries.get(name).map(|e| &e.spec)
    }

    /// The recovery class recorded for this tool's effects. Unknown
    /// tools are `NeverReplay` (DESIGN.md §12.2).
    #[must_use]
    pub fn recovery_class(&self, name: &str) -> RecoveryClass {
        self.entries
            .get(name)
            .map_or(RecoveryClass::NeverReplay, |e| e.recovery_class)
    }

    /// Validate `arguments` against a tool's schema: the value must be an
    /// object containing every name in the schema's `"required"` array.
    /// Canonicalize one invocation's effective target (§17.3). Pure:
    /// no filesystem access, so the decision input cannot change
    /// between policy and executor.
    pub fn canonicalize(&self, name: &str, arguments: &Value) -> Result<CanonicalTarget, String> {
        let resolve = |key: &str| -> Result<std::path::PathBuf, String> {
            let raw = arguments
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("missing string argument: {key}"))?;
            resolve_under(&self.cwd, raw)
        };
        match name {
            "read" | "write" | "edit" => Ok(CanonicalTarget::Path {
                path: resolve("path")?,
            }),
            "search" | "find" => {
                if arguments.get("path").is_some() {
                    Ok(CanonicalTarget::Path {
                        path: resolve("path")?,
                    })
                } else {
                    Ok(CanonicalTarget::Path {
                        path: lexically_normalize(&self.cwd),
                    })
                }
            }
            "bash" => {
                let command = arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing string argument: command".to_owned())?;
                Ok(CanonicalTarget::Command {
                    command: command.to_owned(),
                })
            }
            other => {
                // Registered non-native tools (MCP/extension scopes)
                // canonicalize to a remote target; truly unknown names
                // still deny model-visibly.
                if self.entries.contains_key(other) {
                    Ok(CanonicalTarget::Remote {
                        tool: other.to_owned(),
                    })
                } else {
                    Err(format!("unknown tool: {other}"))
                }
            }
        }
    }

    pub fn validate(&self, name: &str, arguments: &Value) -> Result<(), String> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let required = entry
            .spec
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .ok_or_else(|| "missing required args in tool spec".to_owned())?
            .iter()
            .map_while(|v| v.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        let object = arguments
            .as_object()
            .ok_or_else(|| "tool arguments must be a JSON object".to_owned())?;
        for key in required {
            if !object.contains_key(&key) {
                return Err(format!("missing required argument for `{name}`: {key}"));
            }
        }
        Ok(())
    }

    /// Resolve a tool by name, validate its arguments, and execute it.
    /// Returns a [`ToolOutcome`] the caller classifies into a result.
    pub async fn execute(
        &self,
        name: &str,
        arguments: &Value,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        let entry = match self.entries.get(name) {
            Some(e) => e,
            None => return ToolOutcome::error(format!("unknown tool: {name}")),
        };
        if let Err(msg) = self.validate(name, arguments) {
            return ToolOutcome::error(msg);
        }
        entry.tool.as_ref().call(arguments.clone(), cancel).await
    }

    /// Execute a native mutation with the reconciliation evidence that was
    /// committed before its effect intent. The evidence is passed through a
    /// private argument so the native owner can verify the opened file
    /// descriptor immediately before truncating it.
    pub(crate) async fn execute_with_reconciliation(
        &self,
        name: &str,
        arguments: &Value,
        reconciliation: Option<&Value>,
        artifact_root: Option<&Path>,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        if !matches!(name, "write" | "edit" | "bash")
            || (reconciliation.is_none() && artifact_root.is_none())
        {
            return self.execute(name, arguments, cancel).await;
        }
        let mut enriched = arguments.clone();
        if let Some(object) = enriched.as_object_mut() {
            if let Some(reconciliation) = reconciliation {
                object.insert("__ion_reconciliation".to_owned(), reconciliation.clone());
            }
            if let Some(artifact_root) = artifact_root {
                object.insert(
                    "__ion_artifact_root".to_owned(),
                    Value::String(artifact_root.to_string_lossy().into_owned()),
                );
            }
        }
        self.execute(name, &enriched, cancel).await
    }
}

/// Build the default core-tool entries under `cwd`.
/// The dynamic capability layer (DESIGN.md §18): core tools plus
/// scoped registrations from MCP servers and extensions. Everything
/// registered through a scope is owned by it; removing the scope
/// removes its tools from future snapshots. A snapshot is an ordinary
/// [`ToolRegistry`] - immutable once handed to a model step or a
/// dispatching effect task, so a disappearing scope cannot mutate a
/// started request (§18.2).
#[derive(Clone)]
pub struct ToolCatalog {
    core: ToolRegistry,
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
}

impl ToolCatalog {
    /// A catalog over `cwd` with only the core tool set.
    #[must_use]
    pub fn with_cwd(cwd: impl AsRef<Path>) -> Self {
        Self::from(ToolRegistry::with_cwd(cwd))
    }

    /// A read-only catalog over `cwd` (§20.4): the bounded research
    /// child capability set.
    #[must_use]
    pub fn read_only(cwd: impl AsRef<Path>) -> Self {
        Self::from(ToolRegistry::read_only(cwd))
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        self.core.cwd()
    }

    /// Register tools under `scope`, replacing that scope's previous
    /// registration. Publishing at a safe context boundary is the
    /// caller's contract (§19.2).
    pub fn register_scope(&self, scope: impl Into<String>, tools: Vec<Arc<dyn Tool>>) {
        let entries = tools
            .into_iter()
            .map(|tool| {
                let spec = tool.spec();
                let recovery_class = RecoveryClass::NeverReplay;
                ToolEntry {
                    tool,
                    spec,
                    recovery_class,
                }
            })
            .collect();
        self.dynamic
            .write()
            .expect("tool catalog poisoned")
            .insert(scope.into(), entries);
    }

    /// Remove a scope; future snapshots no longer include its tools.
    /// Returns false when the scope was not registered.
    pub fn remove_scope(&mut self, scope: &str) -> bool {
        self.dynamic
            .write()
            .expect("tool catalog poisoned")
            .remove(scope)
            .is_some()
    }

    /// The merged immutable snapshot: core plus every live scope. Name
    /// collisions resolve in favor of core tools.
    #[must_use]
    pub fn snapshot(&self) -> ToolRegistry {
        let mut entries: HashMap<String, ToolEntry> = self.core.entries.as_ref().clone();
        for scoped in self.dynamic.read().expect("tool catalog poisoned").values() {
            for entry in scoped {
                entries
                    .entry(entry.spec.name.clone())
                    .or_insert_with(|| entry.clone());
            }
        }
        ToolRegistry {
            cwd: Arc::from(self.core.cwd()),
            entries: Arc::new(entries),
        }
    }

    /// All registered tool specs in the current snapshot, ordered by
    /// name (deterministic capability snapshot, DESIGN.md P9).
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.snapshot().specs()
    }

    /// Look up a tool's spec in the current snapshot.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<ToolSpec> {
        self.snapshot().get(name).cloned()
    }

    /// The recovery class recorded for this tool's effects.
    #[must_use]
    pub fn recovery_class(&self, name: &str) -> RecoveryClass {
        self.snapshot().recovery_class(name)
    }

    /// Canonicalize one invocation's effective target (§17.3).
    pub fn canonicalize(&self, name: &str, arguments: &Value) -> Result<CanonicalTarget, String> {
        self.snapshot().canonicalize(name, arguments)
    }

    /// Validate `arguments` against a tool's schema in the current
    /// snapshot.
    pub fn validate(&self, name: &str, arguments: &Value) -> Result<(), String> {
        self.snapshot().validate(name, arguments)
    }

    /// Execute against the current snapshot: a scope removed after
    /// planning but before execution yields a visible unknown-tool
    /// failure (§18.2).
    pub async fn execute(
        &self,
        name: &str,
        arguments: &Value,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        self.snapshot().execute(name, arguments, cancel).await
    }

    pub(crate) async fn execute_with_reconciliation(
        &self,
        name: &str,
        arguments: &Value,
        reconciliation: Option<&Value>,
        artifact_root: Option<&Path>,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        self.snapshot()
            .execute_with_reconciliation(name, arguments, reconciliation, artifact_root, cancel)
            .await
    }
}

impl From<ToolRegistry> for ToolCatalog {
    fn from(core: ToolRegistry) -> Self {
        Self {
            core,
            dynamic: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }
}

impl Default for ToolCatalog {
    fn default() -> Self {
        Self::with_cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

/// A short display summary of a tool call's target, derived from the
/// raw arguments. Used where durable entries are rendered without a
/// registry (recovered transcripts); matches what live emission shows
/// because canonicalization preserves the file name and command text.
#[must_use]
pub fn target_from_arguments(name: &str, arguments: &Value) -> Option<String> {
    if name == "bash" {
        return arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
    }
    arguments.get("path").and_then(|v| v.as_str()).map(|path| {
        std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
    })
}

fn core_tools(cwd: &Path) -> HashMap<String, ToolEntry> {
    let cwd_path: Arc<Path> = Arc::from(cwd);
    // Recovery classes per DESIGN.md §12.2/§12.3: reads are
    // replay-safe; bash never replays automatically (§12.4); write/edit
    // reconcile because admission persists preimage/postimage evidence
    // with the intent, so recovery can classify the file state it
    // finds.
    let tools: Vec<(Arc<dyn Tool>, RecoveryClass)> = vec![
        (
            Arc::new(ReadTool {
                cwd: cwd_path.clone(),
            }),
            RecoveryClass::ReplaySafe,
        ),
        (
            Arc::new(WriteTool {
                cwd: cwd_path.clone(),
            }),
            RecoveryClass::Reconcile,
        ),
        (
            Arc::new(EditTool {
                cwd: cwd_path.clone(),
            }),
            RecoveryClass::Reconcile,
        ),
        (
            Arc::new(BashTool {
                cwd: cwd_path.clone(),
            }),
            RecoveryClass::NeverReplay,
        ),
        (
            Arc::new(SearchTool {
                cwd: cwd_path.clone(),
            }),
            RecoveryClass::ReplaySafe,
        ),
        (
            Arc::new(FindTool {
                cwd: cwd_path.clone(),
            }),
            RecoveryClass::ReplaySafe,
        ),
    ];
    let mut map = HashMap::new();
    for (tool, recovery_class) in tools {
        let spec = tool.spec();
        map.insert(
            spec.name.clone(),
            ToolEntry {
                tool,
                spec,
                recovery_class,
            },
        );
    }
    map
}

#[derive(Debug, Clone)]
struct SecurePath {
    display: PathBuf,
    relative: PathBuf,
}

#[derive(Debug)]
enum SecureOpenError {
    Missing,
    Message(String),
}

impl SecureOpenError {
    fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

fn secure_open_error_text(error: SecureOpenError) -> String {
    match error {
        SecureOpenError::Missing => "path not found".to_owned(),
        SecureOpenError::Message(message) => message,
    }
}

#[derive(Debug, Clone, Copy)]
enum SecureOpenMode {
    Read,
    WriteReplace,
    WriteExisting,
    WriteCreateExclusive,
}

/// Resolve a user-supplied relative path under `cwd`, lexically normalizing
/// `.` and `..`, rejecting escapes, and refusing protected `.git` paths.
/// Filesystem symlink checks happen at the descriptor-open boundary below.
fn resolve_under(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
    Ok(secure_path(cwd, Path::new(raw), false)?.display)
}

fn secure_path(cwd: &Path, raw: &Path, allow_absolute: bool) -> Result<SecurePath, String> {
    let root = lexically_normalize(cwd);
    let (display, relative) = if raw.is_absolute() {
        if !allow_absolute {
            return Err(format!(
                "refusing absolute path outside the project root: {}",
                raw.display()
            ));
        }
        let display = lexically_normalize(raw);
        let relative = display
            .strip_prefix(&root)
            .map_err(|_| format!("path escapes the project root: {}", raw.display()))?
            .to_path_buf();
        (display, relative)
    } else {
        let relative = lexically_normalize(raw);
        let display = lexically_normalize(&root.join(&relative));
        (display, relative)
    };
    if relative == Path::new("..") || relative.starts_with("..") {
        return Err(format!("path escapes the project root: {}", raw.display()));
    }
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(format!("path escapes the project root: {}", raw.display()));
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::Normal(name) if name == ".git"))
    {
        return Err(format!(
            "protected path is not available to native file tools: {}",
            raw.display()
        ));
    }
    Ok(SecurePath { display, relative })
}

/// Lexical normalization: collapse `.` and `..` without touching the
/// filesystem, so writes to not-yet-created files stay contained.
fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    let mut has_root = false;
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                out.push(p.as_os_str());
                has_root = true;
            }
            Component::RootDir => {
                out = PathBuf::new();
                out.push(comp.as_os_str());
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(name) => {
                out.push(name);
            }
        }
    }
    if has_root && out.as_os_str().is_empty() {
        out.push("/");
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(not(unix))]
fn check_no_symlink_components(path: &Path) -> Result<(), SecureOpenError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(SecureOpenError::message(format!(
                    "refusing symlink path component: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(SecureOpenError::message(format!(
                    "cannot inspect path component {}: {err}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn nix_open_error(err: nix::errno::Errno) -> SecureOpenError {
    if err == nix::errno::Errno::ENOENT {
        SecureOpenError::Missing
    } else if err == nix::errno::Errno::ELOOP {
        SecureOpenError::message("refusing symlink path component")
    } else {
        SecureOpenError::message(err.to_string())
    }
}

#[cfg(unix)]
fn open_secure_unix(
    cwd: &Path,
    path: &SecurePath,
    mode: SecureOpenMode,
) -> Result<std::os::fd::OwnedFd, SecureOpenError> {
    let root_flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    let mut directory = open(cwd, root_flags, Mode::empty()).map_err(nix_open_error)?;
    let components: Vec<_> = path.relative.components().collect();
    let Some(leaf) = components.last() else {
        return Err(SecureOpenError::message("path must name a file"));
    };
    let mut candidate = cwd.to_path_buf();
    for component in &components[..components.len() - 1] {
        let name = component.as_os_str();
        candidate.push(name);
        if let Ok(metadata) = std::fs::symlink_metadata(&candidate)
            && metadata.file_type().is_symlink()
        {
            return Err(SecureOpenError::message(format!(
                "refusing symlink path component: {}",
                candidate.display()
            )));
        }
        let flags = root_flags;
        directory = match openat(&directory, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(nix::errno::Errno::ENOENT) => {
                if !matches!(
                    mode,
                    SecureOpenMode::WriteReplace | SecureOpenMode::WriteCreateExclusive
                ) {
                    return Err(SecureOpenError::Missing);
                }
                match mkdirat(&directory, name, Mode::from_bits_truncate(0o755)) {
                    Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
                    Err(err) => return Err(nix_open_error(err)),
                }
                openat(&directory, name, flags, Mode::empty()).map_err(nix_open_error)?
            }
            Err(err) => return Err(nix_open_error(err)),
        };
    }
    let leaf_flags = OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    let (access, create, truncate, exclusive) = match mode {
        SecureOpenMode::Read => (OFlag::O_RDONLY, false, false, false),
        SecureOpenMode::WriteReplace => (OFlag::O_WRONLY, true, true, false),
        SecureOpenMode::WriteExisting => (OFlag::O_RDWR, false, false, false),
        SecureOpenMode::WriteCreateExclusive => (OFlag::O_WRONLY, true, false, true),
    };
    let mut flags = leaf_flags | access;
    if create {
        flags |= OFlag::O_CREAT;
    }
    if truncate {
        flags |= OFlag::O_TRUNC;
    }
    if exclusive {
        flags |= OFlag::O_EXCL;
    }
    openat(
        &directory,
        leaf.as_os_str(),
        flags,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(nix_open_error)
}

#[cfg(unix)]
fn validate_secure_directory_unix(cwd: &Path, path: &SecurePath) -> Result<(), SecureOpenError> {
    let root_flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    let mut directory = open(cwd, root_flags, Mode::empty()).map_err(nix_open_error)?;
    let mut candidate = cwd.to_path_buf();
    for component in path.relative.components() {
        candidate.push(component.as_os_str());
        if let Ok(metadata) = std::fs::symlink_metadata(&candidate)
            && metadata.file_type().is_symlink()
        {
            return Err(SecureOpenError::message(format!(
                "refusing symlink path component: {}",
                candidate.display()
            )));
        }
        directory = openat(&directory, component.as_os_str(), root_flags, Mode::empty())
            .map_err(nix_open_error)?;
    }
    Ok(())
}

fn validate_secure_directory(cwd: &Path, path: &SecurePath) -> Result<(), SecureOpenError> {
    #[cfg(unix)]
    {
        validate_secure_directory_unix(cwd, path)
    }
    #[cfg(not(unix))]
    {
        check_no_symlink_components(&path.display)
    }
}

async fn open_secure_file(
    cwd: &Path,
    path: &SecurePath,
    mode: SecureOpenMode,
) -> Result<fs::File, SecureOpenError> {
    #[cfg(unix)]
    {
        let fd = open_secure_unix(cwd, path, mode)?;
        Ok(fs::File::from_std(std::fs::File::from(fd)))
    }
    #[cfg(not(unix))]
    {
        check_no_symlink_components(&path.display)?;
        let mut options = fs::OpenOptions::new();
        match mode {
            SecureOpenMode::Read => {
                options.read(true);
            }
            SecureOpenMode::WriteReplace => {
                options.write(true).create(true).truncate(true);
            }
            SecureOpenMode::WriteExisting => {
                options.read(true).write(true);
            }
            SecureOpenMode::WriteCreateExclusive => {
                options.write(true).create_new(true);
            }
        }
        options.open(&path.display).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                SecureOpenError::Missing
            } else {
                SecureOpenError::message(err.to_string())
            }
        })
    }
}

async fn snapshot_from_file(file: &mut fs::File) -> Result<FileSnapshot, String> {
    let metadata = file
        .metadata()
        .await
        .map_err(|err| format!("cannot inspect file: {err}"))?;
    if !metadata.file_type().is_file() {
        return Err("path is not a regular file".to_owned());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|err| format!("read failed: {err}"))?;
    file.seek(SeekFrom::Start(0))
        .await
        .map_err(|err| format!("cannot rewind file: {err}"))?;
    use sha2::{Digest, Sha256};
    Ok(FileSnapshot {
        hash: Sha256::digest(bytes).into(),
        identity: metadata_identity(&metadata),
    })
}

pub(crate) async fn file_snapshot(
    cwd: &Path,
    path: &Path,
    allow_absolute: bool,
) -> Result<Option<FileSnapshot>, String> {
    let secure = secure_path(cwd, path, allow_absolute)?;
    let mut file = match open_secure_file(cwd, &secure, SecureOpenMode::Read).await {
        Ok(file) => file,
        Err(SecureOpenError::Missing) => return Ok(None),
        Err(SecureOpenError::Message(message)) => return Err(message),
    };
    snapshot_from_file(&mut file).await.map(Some)
}

async fn read_secure_bytes(
    cwd: &Path,
    path: &Path,
    allow_absolute: bool,
) -> Result<Vec<u8>, String> {
    let secure = secure_path(cwd, path, allow_absolute)?;
    let mut file = open_secure_file(cwd, &secure, SecureOpenMode::Read)
        .await
        .map_err(|error| match error {
            SecureOpenError::Missing => "file not found".to_owned(),
            SecureOpenError::Message(message) => message,
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|err| format!("read failed: {err}"))?;
    Ok(bytes)
}

async fn read_secure_text(cwd: &Path, path: &Path, allow_absolute: bool) -> Result<String, String> {
    let bytes = read_secure_bytes(cwd, path, allow_absolute).await?;
    String::from_utf8(bytes).map_err(|err| format!("file is not valid UTF-8: {err}"))
}

async fn write_secure_bytes(
    cwd: &Path,
    path: &str,
    contents: &[u8],
    reconciliation: Option<&Value>,
) -> Result<(), String> {
    let secure = secure_path(cwd, Path::new(path), false)?;
    let expected_exists = reconciliation.and_then(|evidence| {
        evidence
            .get("preimage")
            .and_then(|preimage| preimage.get("exists"))
            .and_then(Value::as_bool)
    });
    let mode = match expected_exists {
        Some(true) => SecureOpenMode::WriteExisting,
        Some(false) => SecureOpenMode::WriteCreateExclusive,
        None => SecureOpenMode::WriteReplace,
    };
    let mut file = open_secure_file(cwd, &secure, mode)
        .await
        .map_err(|error| match error {
            SecureOpenError::Missing => "file disappeared before write".to_owned(),
            SecureOpenError::Message(message) => format!("write failed: {message}"),
        })?;
    if let Some(evidence) = reconciliation.filter(|_| expected_exists == Some(true)) {
        let current = snapshot_from_file(&mut file)
            .await
            .map_err(|message| format!("write precondition could not be checked: {message}"))?;
        if !precondition_matches(evidence, Some(&current)) {
            return Err("write precondition changed; refusing to overwrite".to_owned());
        }
        file.set_len(0)
            .await
            .map_err(|err| format!("write truncate failed: {err}"))?;
        file.seek(SeekFrom::Start(0))
            .await
            .map_err(|err| format!("write seek failed: {err}"))?;
    }
    file.write_all(contents)
        .await
        .map_err(|err| format!("write failed: {err}"))?;
    file.flush()
        .await
        .map_err(|err| format!("write flush failed: {err}"))?;
    file.sync_all()
        .await
        .map_err(|err| format!("write sync failed: {err}"))
}

// ---- read ----

pub struct ReadTool {
    cwd: Arc<Path>,
}

impl ReadTool {
    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the project root." }
            },
            "required": ["path"]
        })
    }
}

impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".to_owned(),
            description: "Read a file's contents".to_owned(),
            input_schema: Self::input_schema(),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let path = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.to_owned(),
                None => return ToolOutcome::error("missing argument: path"),
            };
            match read_secure_text(&self.cwd, Path::new(&path), false).await {
                Ok(text) => ToolOutcome::text(text),
                Err(message) => ToolOutcome::error(format!("read failed: {message}")),
            }
        })
    }
}

// ---- write ----

pub struct WriteTool {
    cwd: Arc<Path>,
}

impl WriteTool {
    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "contents": { "type": "string" }
            },
            "required": ["path", "contents"]
        })
    }
}

impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".to_owned(),
            description: "Write contents to a file, replacing it".to_owned(),
            input_schema: Self::input_schema(),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let path = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.to_owned(),
                None => return ToolOutcome::error("missing argument: path"),
            };
            let contents = match arguments.get("contents").and_then(|v| v.as_str()) {
                Some(c) => c.to_owned(),
                None => return ToolOutcome::error("missing argument: contents"),
            };
            let reconciliation = arguments.get("__ion_reconciliation");
            match write_secure_bytes(&self.cwd, &path, contents.as_bytes(), reconciliation).await {
                Ok(()) => ToolOutcome::text("written"),
                Err(message) => ToolOutcome::error(message),
            }
        })
    }
}

// ---- edit ----

pub struct EditTool {
    cwd: Arc<Path>,
}

impl EditTool {
    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_str": { "type": "string" },
                "new_str": { "type": "string" }
            },
            "required": ["path", "old_str", "new_str"]
        })
    }
}

impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".to_owned(),
            description: "Replace the first occurrence of a string in a file".to_owned(),
            input_schema: Self::input_schema(),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let path = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.to_owned(),
                None => return ToolOutcome::error("missing argument: path"),
            };
            let old_str = match arguments.get("old_str").and_then(|v| v.as_str()) {
                Some(s) => s.to_owned(),
                None => return ToolOutcome::error("missing argument: old_str"),
            };
            let new_str = arguments
                .get("new_str")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let original = match read_secure_text(&self.cwd, Path::new(&path), false).await {
                Ok(text) => text,
                Err(message) => return ToolOutcome::error(format!("read failed: {message}")),
            };
            if !original.contains(&old_str) {
                return ToolOutcome::error("old_str not found in file");
            }
            let updated = original.replacen(&old_str, &new_str, 1);
            match write_secure_bytes(
                &self.cwd,
                &path,
                updated.as_bytes(),
                arguments.get("__ion_reconciliation"),
            )
            .await
            {
                Ok(()) => ToolOutcome::text("edited"),
                Err(message) => ToolOutcome::error(message),
            }
        })
    }
}

// ---- bash ----

pub struct BashTool {
    cwd: Arc<Path>,
}

impl BashTool {
    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run with sh -c." }
            },
            "required": ["command"]
        })
    }
}

impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_owned(),
            description: "Run a shell command and return its combined output".to_owned(),
            input_schema: Self::input_schema(),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let command = match arguments.get("command").and_then(|v| v.as_str()) {
                Some(c) => c.to_owned(),
                None => return ToolOutcome::error("missing argument: command"),
            };
            let artifact_root = arguments
                .get("__ion_artifact_root")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            run_shell(&self.cwd, &command, artifact_root.as_deref(), cancel).await
        })
    }
}

struct OutputPreview {
    inline: Vec<u8>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: u64,
    truncated: bool,
}

impl OutputPreview {
    fn new() -> Self {
        Self {
            inline: Vec::new(),
            head: Vec::new(),
            tail: VecDeque::with_capacity(MODEL_SAMPLE_TAIL_BYTES),
            total_bytes: 0,
            truncated: false,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        self.total_bytes = self
            .total_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if !self.truncated
            && self.inline.len().saturating_add(bytes.len()) <= MODEL_SAMPLE_MAX_BYTES
        {
            self.inline.extend_from_slice(bytes);
            return;
        }
        if !self.truncated {
            self.head = self.inline[..MODEL_SAMPLE_HEAD_BYTES.min(self.inline.len())].to_vec();
            self.tail
                .extend(self.inline[self.head.len()..].iter().copied());
            self.inline.clear();
            self.truncated = true;
        }
        for byte in bytes {
            if self.head.len() < MODEL_SAMPLE_HEAD_BYTES {
                self.head.push(*byte);
                continue;
            }
            if self.tail.len() == MODEL_SAMPLE_TAIL_BYTES {
                self.tail.pop_front();
            }
            self.tail.push_back(*byte);
        }
    }

    fn render(&mut self) -> String {
        if !self.truncated {
            return String::from_utf8_lossy(&self.inline).into_owned();
        }
        format!(
            "[tool output abbreviated; {} bytes total]\n{}\n… omitted middle …\n{}",
            self.total_bytes,
            String::from_utf8_lossy(&self.head),
            String::from_utf8_lossy(self.tail.make_contiguous()),
        )
    }
}

struct ArtifactWriter {
    id: String,
    temp_path: PathBuf,
    final_path: PathBuf,
    file: fs::File,
    hasher: sha2::Sha256,
    stored_bytes: u64,
    truncated: bool,
}

impl ArtifactWriter {
    async fn create(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root)
            .await
            .map_err(|err| format!("create artifact directory: {err}"))?;
        let id = Uuid::now_v7().to_string();
        let temp_path = root.join(format!(".{id}.part"));
        let final_path = root.join(&id);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(|err| format!("create artifact spool: {err}"))?;
        Ok(Self {
            id,
            temp_path,
            final_path,
            file,
            hasher: sha2::Sha256::default(),
            stored_bytes: 0,
            truncated: false,
        })
    }

    async fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        let remaining = ARTIFACT_MAX_BYTES.saturating_sub(self.stored_bytes);
        let take = remaining.min(u64::try_from(bytes.len()).unwrap_or(u64::MAX)) as usize;
        if take > 0 {
            self.file
                .write_all(&bytes[..take])
                .await
                .map_err(|err| format!("write artifact spool: {err}"))?;
            self.hasher.update(&bytes[..take]);
            self.stored_bytes = self
                .stored_bytes
                .saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
        }
        if take < bytes.len() {
            self.truncated = true;
        }
        Ok(())
    }

    async fn finish(mut self, total_bytes: u64) -> Result<ToolArtifact, String> {
        if let Err(err) = self.file.flush().await {
            let _ = fs::remove_file(&self.temp_path).await;
            return Err(format!("flush artifact spool: {err}"));
        }
        if let Err(err) = self.file.sync_all().await {
            let _ = fs::remove_file(&self.temp_path).await;
            return Err(format!("sync artifact spool: {err}"));
        }
        drop(self.file);
        if let Err(err) = fs::rename(&self.temp_path, &self.final_path).await {
            let _ = fs::remove_file(&self.temp_path).await;
            return Err(format!("publish artifact: {err}"));
        }
        let digest = hex(&self.hasher.finalize());
        Ok(ToolArtifact {
            uri: format!("artifact://{}", self.id),
            stored_bytes: self.stored_bytes,
            total_bytes,
            sha256: digest,
            truncated: self.truncated,
        })
    }
}

struct CollectedOutput {
    output: String,
    artifact: Option<ToolArtifact>,
    capture_error: Option<String>,
}

struct OutputSpool {
    preview: OutputPreview,
    artifact_root: Option<PathBuf>,
    artifact: Option<ArtifactWriter>,
    capture_error: Option<String>,
}

impl OutputSpool {
    fn new(artifact_root: Option<PathBuf>) -> Self {
        Self {
            preview: OutputPreview::new(),
            artifact_root,
            artifact: None,
            capture_error: None,
        }
    }

    async fn append(&mut self, bytes: &[u8]) {
        if self.artifact.is_none()
            && self.capture_error.is_none()
            && self.preview.total_bytes.saturating_add(bytes.len() as u64)
                > MODEL_SAMPLE_MAX_BYTES as u64
            && let Some(root) = self.artifact_root.as_deref()
        {
            match ArtifactWriter::create(root).await {
                Ok(mut artifact) => {
                    let prefix = std::mem::take(&mut self.preview.inline);
                    let result = artifact.append(&prefix).await;
                    let result = if result.is_ok() {
                        artifact.append(bytes).await
                    } else {
                        result
                    };
                    if let Err(err) = result {
                        let _ = fs::remove_file(&artifact.temp_path).await;
                        self.capture_error = Some(err);
                    } else {
                        self.artifact = Some(artifact);
                    }
                    self.preview.inline = prefix;
                }
                Err(err) => self.capture_error = Some(err),
            }
        } else if let Some(mut artifact) = self.artifact.take() {
            match artifact.append(bytes).await {
                Ok(()) => self.artifact = Some(artifact),
                Err(err) => {
                    let _ = fs::remove_file(&artifact.temp_path).await;
                    self.capture_error = Some(err);
                }
            }
        }
        self.preview.append(bytes);
    }

    async fn finish(mut self) -> CollectedOutput {
        let total_bytes = self.preview.total_bytes;
        let artifact = if let Some(artifact) = self.artifact.take() {
            match artifact.finish(total_bytes).await {
                Ok(artifact) => Some(artifact),
                Err(err) => {
                    self.capture_error = Some(err);
                    None
                }
            }
        } else {
            None
        };
        CollectedOutput {
            output: self.preview.render(),
            artifact,
            capture_error: self.capture_error,
        }
    }
}

async fn drain_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
    tx: mpsc::Sender<Vec<u8>>,
) -> Result<(), String> {
    let mut buffer = [0_u8; OUTPUT_CHUNK_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|err| format!("read process output: {err}"))?;
        if read == 0 {
            return Ok(());
        }
        tx.send(buffer[..read].to_vec())
            .await
            .map_err(|_| "output collector stopped".to_owned())?;
    }
}

async fn collect_process_output(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    artifact_root: Option<PathBuf>,
) -> CollectedOutput {
    let (tx, mut rx) = mpsc::channel(8);
    let mut readers = Vec::with_capacity(2);
    if let Some(stdout) = stdout {
        readers.push(tokio::spawn(drain_pipe(stdout, tx.clone())));
    }
    if let Some(stderr) = stderr {
        readers.push(tokio::spawn(drain_pipe(stderr, tx.clone())));
    }
    drop(tx);

    let mut spool = OutputSpool::new(artifact_root);
    while let Some(chunk) = rx.recv().await {
        spool.append(&chunk).await;
    }
    for reader in readers {
        match reader.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if spool.capture_error.is_none() {
                    spool.capture_error = Some(err);
                }
            }
            Err(err) => {
                if spool.capture_error.is_none() {
                    spool.capture_error = Some(format!("output reader task failed: {err}"));
                }
            }
        }
    }
    spool.finish().await
}

/// Spawn a shell command under cwd, killing the child on cancel.
/// Model-visible output is bounded; larger raw output is atomically
/// published as a bounded artifact when the session store provides a root.
async fn run_shell(
    cwd: &Path,
    command: &str,
    artifact_root: Option<&Path>,
    cancel: CancellationToken,
) -> ToolOutcome {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = match ProcessGuard::spawn(&mut cmd) {
        Ok(child) => child,
        Err(err) => return ToolOutcome::error(format!("spawn failed: {err}")),
    };

    let stdout = process.take_stdout();
    let stderr = process.take_stderr();

    // Drain both pipes concurrently. Waiting for EOF first would deadlock on
    // a silent long-running command and make cancellation uninterruptible.
    let read_task = tokio::spawn(collect_process_output(
        stdout,
        stderr,
        artifact_root.map(Path::to_path_buf),
    ));

    tokio::select! {
        status = process.wait() => {
            let reaped = status.is_ok();
            let collected = match read_task.await {
                Ok(collected) => collected,
                Err(err) => CollectedOutput {
                    output: String::new(),
                    artifact: None,
                    capture_error: Some(format!("output collector task failed: {err}")),
                },
            };
            let CollectedOutput {
                output: captured,
                artifact,
                capture_error,
            } = collected;
            let code = match status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let output = if code == 0 {
                captured
            } else {
                format!("command exited with code {code}\n{captured}")
            };
            let outcome = if let Some(error) = capture_error {
                ToolOutcome::error(format!("output capture failed: {error}\n{output}"))
            } else if code == 0 {
                ToolOutcome::text(output)
            } else {
                ToolOutcome::error(output)
            };
            if reaped {
                process.disarm();
            }
            outcome.with_artifact(artifact)
        }
        _ = cancel.cancelled() => {
            // Reap the killed child so it does not linger as a zombie.
            let reaped = process.kill_and_wait().await.is_ok();
            // Killing the group closes the pipes, so the read task finishes.
            let collected = match read_task.await {
                Ok(collected) => collected,
                Err(err) => CollectedOutput {
                    output: String::new(),
                    artifact: None,
                    capture_error: Some(format!("output collector task failed: {err}")),
                },
            };
            let message = collected.capture_error.map_or_else(
                || "cancelled".to_owned(),
                |error| format!("cancelled; output capture failed: {error}"),
            );
            if reaped {
                process.disarm();
            }
            ToolOutcome::error(message).with_artifact(collected.artifact)
        }
    }
}

// ---- search ----

pub struct SearchTool {
    cwd: Arc<Path>,
}

impl SearchTool {
    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "path": { "type": "string", "description": "Directory to search under; defaults to the project root." }
            },
            "required": ["pattern"]
        })
    }
}

impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".to_owned(),
            description: "Search file contents for a regex pattern".to_owned(),
            input_schema: Self::input_schema(),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(pattern) = arguments.get("pattern").and_then(|v| v.as_str()) else {
                return ToolOutcome::error("missing argument: pattern");
            };
            let Ok(regex) = Regex::new(pattern) else {
                return ToolOutcome::error(format!("invalid regex: {pattern}"));
            };
            let secure_root = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => match secure_path(&self.cwd, Path::new(p), false) {
                    Ok(path) => path,
                    Err(message) => return ToolOutcome::error(message),
                },
                None => match secure_path(&self.cwd, Path::new("."), false) {
                    Ok(path) => path,
                    Err(message) => return ToolOutcome::error(message),
                },
            };
            if let Err(error) = validate_secure_directory(&self.cwd, &secure_root) {
                return ToolOutcome::error(secure_open_error_text(error));
            }
            search_files(&self.cwd, &secure_root.display, &regex, &cancel).await
        })
    }
}

/// Walk `root` collecting `path:line_number:line` for every non-binary
/// file containing a regex match. Skips hidden entries and `target`.
async fn search_files(
    cwd: &Path,
    root: &Path,
    regex: &Regex,
    cancel: &CancellationToken,
) -> ToolOutcome {
    let mut results: Vec<String> = Vec::new();
    let mut files: VecDeque<PathBuf> = VecDeque::new();
    collect_files(root, &mut files);

    for file in files {
        if cancel.is_cancelled() {
            return ToolOutcome::error("cancelled".to_owned());
        }
        let Ok(contents) = read_secure_text(cwd, &file, true).await else {
            continue;
        };
        if contents.as_bytes().contains(&0u8) {
            continue;
        }
        let rel = file.strip_prefix(root).unwrap_or(&file);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        for (lineno, line) in contents.lines().enumerate() {
            if regex.is_match(line) {
                results.push(format!("{rel_str}:{}:{line}", lineno + 1));
            }
            if results.len() >= 256 {
                return ToolOutcome::text(results.join("\n"));
            }
        }
    }

    if results.is_empty() {
        ToolOutcome::text("no matches")
    } else {
        ToolOutcome::text(results.join("\n"))
    }
}

/// Recursively collect regular files under `root`, skipping hidden entries
/// and the `target` directory. Synchronous filesystem traversal is acceptable
/// here because the tool future is spawned on its own task.
fn collect_files(root: &Path, out: &mut VecDeque<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_files(&path, out);
        } else if file_type.is_file() {
            out.push_back(path);
        }
    }
}

// ---- find ----

pub struct FindTool {
    cwd: Arc<Path>,
}

impl FindTool {
    fn input_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. *.rs or src/**/*.rs" },
                "path": { "type": "string", "description": "Directory to search under; defaults to the project root." }
            },
            "required": ["pattern"]
        })
    }
}

impl Tool for FindTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find".to_owned(),
            description: "Find files matching a glob pattern".to_owned(),
            input_schema: Self::input_schema(),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(pattern) = arguments.get("pattern").and_then(|v| v.as_str()) else {
                return ToolOutcome::error("missing argument: pattern");
            };
            let Ok(glob) = Glob::new(pattern) else {
                return ToolOutcome::error(format!("invalid glob: {pattern}"));
            };
            let mut builder = GlobSetBuilder::new();
            builder.add(glob);
            let Ok(set) = builder.build() else {
                return ToolOutcome::error("cannot build glob set");
            };
            let secure_root = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => match secure_path(&self.cwd, Path::new(p), false) {
                    Ok(path) => path,
                    Err(message) => return ToolOutcome::error(message),
                },
                None => match secure_path(&self.cwd, Path::new("."), false) {
                    Ok(path) => path,
                    Err(message) => return ToolOutcome::error(message),
                },
            };
            if let Err(error) = validate_secure_directory(&self.cwd, &secure_root) {
                return ToolOutcome::error(secure_open_error_text(error));
            }
            let mut outputs: Vec<String> = Vec::new();
            collect_matches(&secure_root.display, &set, &mut outputs);
            let _ = cancel;
            if outputs.is_empty() {
                ToolOutcome::text("no matches")
            } else {
                ToolOutcome::text(outputs.join("\n"))
            }
        })
    }
}

/// Walk `root` recursively and collect paths matching `set`, relative
/// to `root` itself - not to each visited directory, which would test
/// nested files as bare names. Skips hidden entries and `target`.
fn collect_matches(root: &Path, set: &GlobSet, out: &mut Vec<String>) {
    collect_matches_under(root, root, set, out);
}

fn collect_matches_under(original_root: &Path, dir: &Path, set: &GlobSet, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_matches_under(original_root, &path, set, out);
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(original_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if set.is_match(&rel) || set.is_match(&name) {
                out.push(rel);
            }
        }
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "mcp_echo".to_owned(),
                description: "echo".to_owned(),
                input_schema: json!({"type": "object", "required": []}),
            }
        }
        fn call<'a>(
            &'a self,
            _arguments: Value,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
            Box::pin(async move { ToolOutcome::text("pong") })
        }
    }

    #[test]
    fn scope_registration_and_removal_change_future_snapshots() {
        let mut catalog = ToolCatalog::with_cwd("/tmp");
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        // Removing the scope drops its tools from future snapshots.
        assert!(catalog.remove_scope("server-a"));
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        assert!(!catalog.remove_scope("server-a"), "double remove is false");
    }

    #[test]
    fn core_tools_win_name_collisions() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        struct ReadImpostor;
        impl Tool for ReadImpostor {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "read".to_owned(),
                    description: "impostor".to_owned(),
                    input_schema: json!({"type": "object", "required": []}),
                }
            }
            fn call<'a>(
                &'a self,
                _arguments: Value,
                _cancel: CancellationToken,
            ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
                unreachable!("core read must win")
            }
        }
        catalog.register_scope("rogue", vec![Arc::new(ReadImpostor)]);
        let read = catalog
            .specs()
            .into_iter()
            .find(|s| s.name == "read")
            .expect("read exists");
        assert_eq!(read.description, "Read a file's contents");
    }

    #[tokio::test]
    async fn removed_scope_yields_visible_unknown_tool_failure() {
        let mut catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let outcome = catalog
            .execute("mcp_echo", &json!({}), CancellationToken::default())
            .await;
        assert!(!outcome.is_error);
        catalog.remove_scope("server-a");
        let outcome = catalog
            .execute("mcp_echo", &json!({}), CancellationToken::default())
            .await;
        assert!(outcome.is_error);
        assert!(
            outcome.output.contains("unknown tool"),
            "{}",
            outcome.output
        );
    }
}

#[cfg(test)]
mod preview_bound_tests {
    use super::*;

    fn ok_result(output: &str) -> ToolResult {
        ToolResult::Ok {
            call_id: 1,
            output: output.to_owned(),
            artifact: None,
        }
    }

    #[test]
    fn a_single_oversized_line_is_cut_to_the_hard_byte_bound() {
        // Found in review: the old loop only dropped whole lines, so
        // one minified-JSON line bypassed 2 KiB entirely.
        let line = "x".repeat(100_000);
        let preview = ok_result(&line).display_preview().expect("nonempty output");
        assert!(preview.len() <= PREVIEW_MAX_BYTES, "{}", preview.len());
    }

    #[test]
    fn a_multibyte_line_cuts_on_a_char_boundary() {
        let line = "\u{1F600}".repeat(5_000); // four bytes each, one visual glyph
        let preview = ok_result(&line).display_preview().expect("nonempty output");
        assert!(preview.len() <= PREVIEW_MAX_BYTES, "{}", preview.len());
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn multiline_tail_keeps_the_end_and_counts_the_marker() {
        let text: String = (0..100).map(|i| format!("line-{i}\n")).collect();
        let preview = ok_result(&text).display_preview().expect("nonempty output");
        assert!(preview.contains("line-99"));
        assert!(!preview.contains("line-0\n"));
        assert!(preview.lines().count() <= PREVIEW_MAX_LINES + 1);
        assert!(preview.len() <= PREVIEW_MAX_BYTES);
    }

    #[test]
    fn every_persisted_outcome_is_bounded_even_without_an_artifact_store() {
        let result = ToolResult::from_outcome(1, ToolOutcome::text("x".repeat(100_000)));
        let text = match &result {
            ToolResult::Ok { output, .. } => output,
            ToolResult::Err { error, .. } => error,
        };
        assert!(text.len() <= MODEL_RESULT_MAX_BYTES);
        assert!(text.contains("full result was not externalized"));
        assert!(result.model_text().len() <= MODEL_RESULT_MAX_BYTES);
    }

    #[tokio::test]
    async fn shell_output_spills_raw_bytes_and_bounds_model_text() {
        let artifacts = tempfile::tempdir().expect("artifact directory");
        let outcome = run_shell(
            Path::new("."),
            "i=0; while [ \"$i\" -lt 20000 ]; do printf x; i=$((i+1)); done",
            Some(artifacts.path()),
            CancellationToken::new(),
        )
        .await;
        assert!(!outcome.is_error, "{outcome:?}");
        assert!(outcome.output.contains("tool output abbreviated"));
        let artifact = outcome.artifact.expect("large output artifact");
        assert_eq!(artifact.total_bytes, 20_000);
        assert_eq!(artifact.stored_bytes, 20_000);
        assert!(!artifact.truncated);

        let id = artifact
            .uri
            .strip_prefix("artifact://")
            .expect("artifact URI");
        let raw = std::fs::read(artifacts.path().join(id)).expect("published artifact");
        assert_eq!(raw, vec![b'x'; 20_000]);
        assert!(
            std::fs::read_dir(artifacts.path())
                .expect("artifact directory entries")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part"))
        );

        let result = ToolResult::Ok {
            call_id: 1,
            output: outcome.output,
            artifact: Some(artifact),
        };
        let model_text = result.model_text();
        assert!(model_text.len() <= MODEL_RESULT_MAX_BYTES);
        assert!(model_text.contains("full result: artifact://"));
    }

    #[tokio::test]
    async fn shell_output_decodes_invalid_utf8_without_panicking() {
        let artifacts = tempfile::tempdir().expect("artifact directory");
        let outcome = run_shell(
            Path::new("."),
            r#"printf '\377A'"#,
            Some(artifacts.path()),
            CancellationToken::new(),
        )
        .await;
        assert!(!outcome.is_error, "{outcome:?}");
        assert_eq!(outcome.output, "�A");
    }

    #[tokio::test]
    async fn shell_cancellation_kills_and_reaps_owned_process() {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_shell(
            Path::new("."),
            "trap '' TERM; sleep 30",
            None,
            cancel.clone(),
        ));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("cancellation must not leave a process running")
            .expect("shell task must join");
        assert!(outcome.is_error, "{outcome:?}");
        assert_eq!(outcome.output, "cancelled");
    }
}

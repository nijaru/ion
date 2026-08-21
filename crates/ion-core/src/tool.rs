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
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde_json::{Value, json};
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::ids::OperationId;

/// Identifier for an in-flight tool call. Monotonic per provider.
pub type ToolCallId = u64;

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
    Ok { call_id: ToolCallId, output: String },
    Err { call_id: ToolCallId, error: String },
}

impl ToolResult {
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
        match self {
            Self::Ok { output, .. } => output,
            Self::Err { error, .. } => error,
        }
    }
}

/// Outcome of tool execution, before it is classified into a [`ToolResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
}

impl ToolOutcome {
    #[must_use]
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            output: message.into(),
            is_error: true,
        }
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

/// Lexically normalize a path without touching the filesystem:
/// collapse `.` and resolve `..` against the path itself.
#[must_use]
pub fn normalize(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Registry and executor for tools. Holds an `Arc<Path>` so a tool task
/// can clone the working directory cheaply before invoking a tool.
/// SHA-256 of one file's contents, or `None` when it does not exist.
pub(crate) async fn file_hash(path: &Path) -> Option<[u8; 32]> {
    match fs::read(path).await {
        Ok(bytes) => {
            use sha2::{Digest, Sha256};
            Some(Sha256::digest(&bytes).into())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    let full = resolve_under(cwd, path_arg).map_err(|e| e.output)?;
    let preimage = match file_hash(&full).await {
        Some(hash) => serde_json::json!({ "exists": true, "hash": hex(&hash) }),
        None => serde_json::json!({ "exists": false }),
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
            let original = fs::read_to_string(&full)
                .await
                .map_err(|err| format!("read failed: {err}"))?;
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

/// Classify a pending Reconcile effect against the current file state.
#[must_use]
pub fn classify_reconciliation(evidence: &Value, current: Option<[u8; 32]>) -> ReconcileVerdict {
    let current_hex = current.map(|hash| hex(&hash));
    let postimage = evidence.get("postimage_hash").and_then(|v| v.as_str());
    if postimage == current_hex.as_deref() && postimage.is_some() {
        return ReconcileVerdict::AlreadyApplied;
    }
    let Some(preimage) = evidence.get("preimage") else {
        return ReconcileVerdict::Unknown;
    };
    let preimage_matches = if preimage.get("exists").and_then(|v| v.as_bool()) == Some(true) {
        preimage.get("hash").and_then(|v| v.as_str()) == current_hex.as_deref()
    } else {
        current.is_none()
    };
    if preimage_matches {
        ReconcileVerdict::SafeToExecute
    } else {
        ReconcileVerdict::Conflict
    }
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
            let joined = if std::path::Path::new(raw).is_absolute() {
                std::path::PathBuf::from(raw)
            } else {
                self.cwd.join(raw)
            };
            Ok(normalize(&joined))
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
                        path: normalize(&self.cwd),
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
}

/// Build the default core-tool entries under `cwd`.
/// The model-invoked compaction trigger (DESIGN.md §14.7.3). Always
/// allowed: compaction is harness maintenance, not a capability grant.
pub struct CompactTool;

impl Tool for CompactTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "compact".to_owned(),
            description: "Compact the conversation context into a summary. Call this at a task boundary when the context is large and the next phase of work needs room. Optionally name what must be preserved."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "instructions": {
                        "type": "string",
                        "description": "What the summary must preserve (decisions, paths, next steps)."
                    },
                    "continue_after_compaction": {
                        "type": "boolean",
                        "description": "Start a recovery turn after compaction to finish unfinished work."
                    }
                },
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _arguments: Value,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            ToolOutcome::text("compaction scheduled; it runs when this step settles")
        })
    }
}

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
        // Harness maintenance action (14.7.3): the runtime intercepts
        // the call at admission; execution itself is a no-op so the
        // normal tool-result path settles it durably.
        (Arc::new(CompactTool), RecoveryClass::ReplaySafe),
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

/// Resolve a user-supplied relative path under `cwd`, lexically normalizing
/// `.` and `..` and rejecting escapes above the project root.
fn resolve_under(cwd: &Path, raw: &str) -> Result<PathBuf, ToolOutcome> {
    let relative = Path::new(raw);
    if relative.is_absolute() {
        return Err(ToolOutcome::error(format!(
            "refusing absolute path outside the project root: {raw}"
        )));
    }
    let candidate = cwd.join(relative);
    let normalized = lexically_normalize(&candidate);
    if !normalized.starts_with(cwd) {
        return Err(ToolOutcome::error(format!(
            "path escapes the project root: {raw}"
        )));
    }
    Ok(normalized)
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
            let full = match resolve_under(&self.cwd, &path) {
                Ok(p) => p,
                Err(e) => return e,
            };
            match fs::read(&full).await {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => ToolOutcome::text(text),
                    Err(err) => ToolOutcome::error(format!("file is not valid UTF-8: {err}")),
                },
                Err(err) => ToolOutcome::error(format!("read failed: {err}")),
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
            let full = match resolve_under(&self.cwd, &path) {
                Ok(p) => p,
                Err(e) => return e,
            };
            if let Some(parent) = full.parent() {
                match fs::create_dir_all(parent).await {
                    Ok(()) => {}
                    Err(err) => {
                        return ToolOutcome::error(format!("cannot create directory: {err}"));
                    }
                }
            }
            match fs::write(&full, contents).await {
                Ok(()) => ToolOutcome::text("written"),
                Err(err) => ToolOutcome::error(format!("write failed: {err}")),
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
            let full = match resolve_under(&self.cwd, &path) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let original = match fs::read_to_string(&full).await {
                Ok(text) => text,
                Err(err) => return ToolOutcome::error(format!("read failed: {err}")),
            };
            if !original.contains(&old_str) {
                return ToolOutcome::error("old_str not found in file");
            }
            let updated = original.replacen(&old_str, &new_str, 1);
            match fs::write(&full, updated).await {
                Ok(()) => ToolOutcome::text("edited"),
                Err(err) => ToolOutcome::error(format!("write failed: {err}")),
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
            run_shell(&self.cwd, &command, cancel).await
        })
    }
}

/// Spawn `sh -c command` under `cwd`, killing the child on cancel.
/// Returns combined stdout/stderr as text.
async fn run_shell(cwd: &Path, command: &str, cancel: CancellationToken) -> ToolOutcome {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Put the child in its own process group so cancellation can kill the
    // whole tree (including orphaned grandchildren like a forked `sleep`)
    // rather than only the direct `sh`.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return ToolOutcome::error(format!("spawn failed: {err}")),
    };
    let pid = child.id();

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Drain both pipes concurrently. Waiting for EOF first would deadlock on
    // a silent long-running command and make cancellation uninterruptible.
    let read_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_string(&mut buf).await;
        }
        if let Some(mut e) = stderr {
            let mut err_text = String::new();
            let _ = e.read_to_string(&mut err_text).await;
            buf.push_str(&err_text);
        }
        buf
    });

    tokio::select! {
        status = child.wait() => {
            let out = read_task.await.unwrap_or_default();
            let code = match status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(_) => -1,
            };
            if code == 0 {
                ToolOutcome::text(out)
            } else {
                ToolOutcome::error(format!("command exited with code {code}\n{out}"))
            }
        }
        _ = cancel.cancelled() => {
            if let Some(pid) = pid {
                kill_process_group(pid as i32);
            }
            // Reap the killed child so it does not linger as a zombie.
            let _ = child.wait().await;
            // Killing the group closes the pipes, so the read task finishes.
            let _ = read_task.await;
            ToolOutcome::error("cancelled".to_owned())
        }
    }
}

/// Kill a child process group. On Unix, a negative pid targets the whole
/// group, so orphaned grandchildren (e.g. a `sleep` forked by `sh -c`) die
/// too. A race with a naturally-finishing command is harmless (ESRCH is
/// ignored). Non-Unix falls back to killing nothing here; the caller also
/// reaps the direct child.
fn kill_process_group(pgid: i32) {
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-pgid, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
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
            let root = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => match resolve_under(&self.cwd, p) {
                    Ok(r) => r,
                    Err(e) => return e,
                },
                None => self.cwd.as_ref().to_path_buf(),
            };
            search_files(&root, &regex, &cancel).await
        })
    }
}

/// Walk `root` collecting `path:line_number:line` for every non-binary
/// file containing a regex match. Skips hidden entries and `target`.
async fn search_files(root: &Path, regex: &Regex, cancel: &CancellationToken) -> ToolOutcome {
    let mut results: Vec<String> = Vec::new();
    let mut files: VecDeque<PathBuf> = VecDeque::new();
    collect_files(root, &mut files);

    for file in files {
        if cancel.is_cancelled() {
            return ToolOutcome::error("cancelled".to_owned());
        }
        let Ok(contents) = fs::read_to_string(&file).await else {
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
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.is_file() {
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
            let root = match arguments.get("path").and_then(|v| v.as_str()) {
                Some(p) => match resolve_under(&self.cwd, p) {
                    Ok(r) => r,
                    Err(e) => return e,
                },
                None => self.cwd.as_ref().to_path_buf(),
            };
            let mut outputs: Vec<String> = Vec::new();
            collect_matches(&root, &set, &mut outputs);
            let _ = cancel;
            if outputs.is_empty() {
                ToolOutcome::text("no matches")
            } else {
                ToolOutcome::text(outputs.join("\n"))
            }
        })
    }
}

/// Walk `root` recursively and collect relative paths matching `set`,
/// skipping hidden entries and `target`.
fn collect_matches(root: &Path, set: &GlobSet, out: &mut Vec<String>) {
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
        if path.is_dir() {
            collect_matches(&path, set, out);
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if set.is_match(&rel_str) || set.is_match(&name) {
                out.push(rel_str);
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

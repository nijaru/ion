//! Model-facing context projection (DESIGN.md §14).
//!
//! Local semantic state is canonical; the model sees only a
//! deterministic projection of it (P7, §31 invariant 15): the same
//! entries and configuration always yield the same
//! [`ContextPlan`]. Content-addressed capability/manifests and explicit
//! trusted resources are owned here; compaction remains a runtime boundary.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::session::SessionEntry;
use crate::tool::{ToolCall, ToolSpec};

/// The small, stable system section every model step sees (DESIGN.md
/// §14.4: no timestamps or random values in early prompt sections).
pub const SYSTEM_SECTION: &str = "You are Ion, a terminal coding agent. \
You work inside the user's project directory. \
Use the provided tools to read, write, edit, search, and run commands. \
Prefer tools over guessing; report failures plainly.";

/// A content-addressed immutable capability set for one model step.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapabilitySnapshot {
    pub id: String,
    pub tools: Vec<ToolSpec>,
    /// Stable internal identity for each tool, aligned with `tools` by name.
    /// Providers receive only `tools`; recovery uses identities to fence a
    /// call to the capability generation that produced it.
    pub identities: Vec<String>,
}

impl CapabilitySnapshot {
    #[must_use]
    pub fn new(tools: Vec<ToolSpec>) -> Self {
        let entries = tools.into_iter().map(|tool| {
            let identity = format!("tool:{}@1", tool.name);
            (tool, identity)
        });
        Self::from_entries(entries.collect())
    }

    #[must_use]
    pub(crate) fn from_entries(mut entries: Vec<(ToolSpec, String)>) -> Self {
        entries.sort_by(|left, right| left.0.name.cmp(&right.0.name));
        let (tools, identities): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        let id = digest_json(&(&tools, &identities));
        Self {
            id,
            tools,
            identities,
        }
    }

    #[must_use]
    pub fn identity(&self, tool_name: &str) -> Option<&str> {
        self.tools
            .iter()
            .position(|tool| tool.name == tool_name)
            .and_then(|index| self.identities.get(index))
            .map(String::as_str)
    }

    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.identities.len() == self.tools.len()
            && Self::from_entries(
                self.tools
                    .clone()
                    .into_iter()
                    .zip(self.identities.clone())
                    .collect(),
            )
            .id == self.id
    }
}

/// One explicitly trusted project-local model-facing resource.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustedResource {
    pub path: String,
    pub content: String,
    pub sha256: String,
}

impl TrustedResource {
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        digest_bytes(self.content.as_bytes()) == self.sha256
    }
}

/// Stable model-facing material shared by model steps until a capability or
/// trusted-resource boundary changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextManifest {
    pub id: String,
    pub system: String,
    pub capability_snapshot_id: String,
    pub resources: Vec<TrustedResource>,
}

impl ContextManifest {
    #[must_use]
    pub fn new(capability_snapshot: &CapabilitySnapshot, resources: Vec<TrustedResource>) -> Self {
        let system = render_system(&resources);
        let identity = (&system, &capability_snapshot.id, &resources);
        let id = digest_json(&identity);
        Self {
            id,
            system,
            capability_snapshot_id: capability_snapshot.id.clone(),
            resources,
        }
    }

    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.resources.iter().all(TrustedResource::is_consistent)
            && render_system(&self.resources) == self.system
            && digest_json(&(&self.system, &self.capability_snapshot_id, &self.resources))
                == self.id
    }

    /// Fingerprint the provider-visible stable prefix for one model. The
    /// model is included because prompt caches are never shared across model
    /// identities merely because the local manifest is equal.
    #[must_use]
    pub fn stable_prefix_fingerprint(&self, model_ref: &str) -> String {
        digest_json(&("ion-prefix-v1", model_ref, &self.id))
    }
}

/// Load only explicitly trusted, root-scoped instruction resources.
///
/// The loader has a closed candidate set. It rejects symlinked resources and
/// canonical paths outside the trusted project root; retrieved text never
/// grants trust by itself.
pub fn load_trusted_resources(
    root: &Path,
    trust_project: bool,
) -> Result<Vec<TrustedResource>, String> {
    if !trust_project {
        return Ok(Vec::new());
    }
    let canonical_root =
        std::fs::canonicalize(root).map_err(|err| format!("trust root unavailable: {err}"))?;
    let candidates = [Path::new("AGENTS.md"), Path::new(".ion/instructions.md")];
    let mut resources = Vec::new();
    for relative in candidates {
        let path = root.join(relative);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(format!(
                    "inspect trusted resource {}: {err}",
                    relative.display()
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "trusted resource {} must be a regular non-symlink file",
                relative.display()
            ));
        }
        let canonical = std::fs::canonicalize(&path)
            .map_err(|err| format!("resolve trusted resource {}: {err}", relative.display()))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(format!(
                "trusted resource {} escapes the project root",
                relative.display()
            ));
        }
        let bytes = std::fs::read(&canonical)
            .map_err(|err| format!("read trusted resource {}: {err}", relative.display()))?;
        let content = String::from_utf8(bytes.clone())
            .map_err(|_| format!("trusted resource {} is not UTF-8", relative.display()))?;
        resources.push(TrustedResource {
            path: relative.to_string_lossy().into_owned(),
            content,
            sha256: digest_bytes(&bytes),
        });
    }
    Ok(resources)
}

fn render_system(resources: &[TrustedResource]) -> String {
    let mut system = SYSTEM_SECTION.to_owned();
    for resource in resources {
        system.push_str("\n\n[Trusted project resource: ");
        system.push_str(&resource.path);
        system.push_str("]\n");
        system.push_str(&resource.content);
    }
    system
}

fn digest_json<T: serde::Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("content-addressed values are serializable");
    digest_bytes(&bytes)
}

fn digest_bytes(bytes: &[u8]) -> String {
    crate::tool::hex(&Sha256::digest(bytes))
}

/// One model-facing message in the projected conversation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContextMessage {
    User {
        content: String,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    /// A model-visible tool result, paired by call id.
    Tool {
        call_id: u64,
        content: String,
    },
}

/// The instruction sent with a compaction step (DESIGN.md §14.7).
/// Part of the deterministic plan, so recovery replays identically.
pub const SUMMARIZE_INSTRUCTION: &str = "Summarize the conversation above into a compact \
handoff for a future assistant instance. Preserve: the user's goal, \
decisions made, files touched, and the exact next step. Omit pleasantries.";

/// The exact semantic projection for one model step (DESIGN.md §6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextPlan {
    pub system: String,
    pub messages: Vec<ContextMessage>,
}

impl ContextMessage {
    /// The readable text content of this message, whatever its role.
    #[must_use]
    pub fn prompt_text(&self) -> &str {
        match self {
            Self::User { content } | Self::Tool { content, .. } => content,
            Self::Assistant { content, .. } => content,
        }
    }
}

/// Project session entries into a model-neutral plan. Pure and
/// deterministic. `first_seq` is the durable seq of `entries[0]`, so a
/// compaction baseline can name what it covers (§14.7).
#[must_use]
pub fn project<'a>(
    entries: impl IntoIterator<Item = &'a SessionEntry>,
    first_seq: u64,
) -> ContextPlan {
    project_with_system(entries, first_seq, SYSTEM_SECTION.to_owned())
}

/// Project entries using one stable context manifest.
#[must_use]
pub fn project_with_manifest<'a>(
    entries: impl IntoIterator<Item = &'a SessionEntry>,
    first_seq: u64,
    manifest: &ContextManifest,
) -> ContextPlan {
    project_with_system(entries, first_seq, manifest.system.clone())
}

fn project_with_system<'a>(
    entries: impl IntoIterator<Item = &'a SessionEntry>,
    first_seq: u64,
    system: String,
) -> ContextPlan {
    let mut messages: Vec<ContextMessage> = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        match entry {
            SessionEntry::Compaction {
                covers_through_seq,
                summary,
            } => {
                // Lossy projection maintenance (§14.7): the summary
                // replaces everything through its coverage boundary;
                // canonical entries stay durable.
                if (*covers_through_seq + 1) >= first_seq + index as u64 {
                    messages.clear();
                }
                messages.push(ContextMessage::User {
                    content: format!("[Context summary of the earlier conversation]\n{summary}"),
                });
            }
            SessionEntry::UserMessage { text } => {
                messages.push(ContextMessage::User {
                    content: text.clone(),
                });
            }
            SessionEntry::ModelChanged { .. } => {
                // Configuration lineage is canonical session state, not
                // a conversational message.
            }
            SessionEntry::AssistantMessage { text } => {
                messages.push(ContextMessage::Assistant {
                    content: text.clone(),
                    tool_calls: Vec::new(),
                });
            }
            SessionEntry::ToolCall { call } => {
                // Attach to the preceding assistant message; synthesize
                // one if the stream starts with a call.
                match messages.last_mut() {
                    Some(ContextMessage::Assistant { tool_calls, .. }) => {
                        tool_calls.push(call.clone());
                    }
                    _ => {
                        messages.push(ContextMessage::Assistant {
                            content: String::new(),
                            tool_calls: vec![call.clone()],
                        });
                    }
                }
            }
            SessionEntry::ToolResult { result } => {
                let call_id = result.call_id();
                let content = result.model_text();
                messages.push(ContextMessage::Tool { call_id, content });
            }
        }
    }
    ContextPlan { system, messages }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_owned(),
            description: format!("{name} description"),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn capability_and_manifest_ids_are_stable_across_tool_input_order() {
        let first = CapabilitySnapshot::new(vec![tool("write"), tool("read")]);
        let second = CapabilitySnapshot::new(vec![tool("read"), tool("write")]);
        assert_eq!(first, second);

        let resources = vec![TrustedResource {
            path: "AGENTS.md".to_owned(),
            content: "Use the project conventions.".to_owned(),
            sha256: digest_bytes(b"Use the project conventions."),
        }];
        let first_manifest = ContextManifest::new(&first, resources.clone());
        let second_manifest = ContextManifest::new(&second, resources);
        assert_eq!(first_manifest, second_manifest);
        assert!(
            first_manifest
                .system
                .contains("[Trusted project resource: AGENTS.md]")
        );
    }

    #[test]
    fn trusted_resource_loading_is_explicit_and_root_scoped() {
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join("AGENTS.md"), "trusted").expect("resource");
        assert!(
            load_trusted_resources(root.path(), false)
                .expect("untrusted load")
                .is_empty()
        );
        let resources = load_trusted_resources(root.path(), true).expect("trusted load");
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].path, "AGENTS.md");
        assert_eq!(resources[0].content, "trusted");
        assert_eq!(resources[0].sha256.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_resource_loading_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("instructions.md"), "outside").expect("outside");
        symlink(
            outside.path().join("instructions.md"),
            root.path().join("AGENTS.md"),
        )
        .expect("symlink");
        let error = load_trusted_resources(root.path(), true).expect_err("symlink must fail");
        assert!(error.contains("regular non-symlink"));
    }
}

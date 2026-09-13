//! Built-in task kinds: generation, one tool call, and the post-tools join.
//!
//! These are ordinary registered task kinds. They use the same `TaskContext`,
//! plan and registry contract as any other kind, and the scheduler never
//! branches on their names. A client composes them by registering all three.

mod generation;
mod post_tools;
mod tool;

pub use generation::GenerationKind;
pub use post_tools::PostToolsKind;
pub use tool::{Tool, ToolCatalog, ToolCatalogError, ToolError, ToolFuture, ToolKind};

use std::sync::Arc;

use crate::{TaskKindName, TaskRegistry, TaskRegistryError};

/// Canonical task kind names. The names are data, not scheduler policy.
pub const GENERATION: &str = "generation";
pub const TOOL: &str = "tool";
pub const POST_TOOLS: &str = "post_tools";

/// Entry kinds these built-ins append. The transcript is append-only; these
/// kinds describe the origin of each entry, not its provider wire shape.
pub const USER_ENTRY: &str = "user";
pub const ASSISTANT_ENTRY: &str = "assistant";
pub const TOOL_RESULT_ENTRY: &str = "tool_result";

/// Schema revision of every built-in kind.
pub const SCHEMA_VERSION: u32 = 1;

/// The three built-ins sharing one model service and one tool catalogue.
pub struct Builtins {
    pub model: ion_ai::ModelRef,
    pub service: Arc<dyn ion_ai::ModelService>,
    pub tools: Arc<ToolCatalog>,
}

impl Builtins {
    /// Register every built-in kind under its canonical name.
    pub fn register(&self, registry: &mut TaskRegistry) -> Result<(), TaskRegistryError> {
        registry.register(
            task_kind(GENERATION),
            SCHEMA_VERSION,
            Arc::new(GenerationKind::new(
                self.model.clone(),
                self.service.clone(),
                self.tools.clone(),
            )),
        )?;
        registry.register(
            task_kind(TOOL),
            SCHEMA_VERSION,
            Arc::new(ToolKind::new(self.tools.clone())),
        )?;
        registry.register(
            task_kind(POST_TOOLS),
            SCHEMA_VERSION,
            Arc::new(PostToolsKind),
        )
    }
}

pub(crate) fn task_kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("built-in task kind names are non-empty")
}

pub(crate) fn entry_kind(name: &str) -> crate::EntryKind {
    crate::EntryKind::new(name).expect("built-in entry kind names are non-empty")
}

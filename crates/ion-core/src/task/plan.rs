use serde_json::Value;

use super::TaskKindName;
use crate::conversation::context::ContextControl;
use crate::{ConversationId, EntryKind, TaskId};

/// A bounded set of canonical writes that a trusted task kind commits atomically
/// with its terminal outcome. This is the only way a task creates successors or
/// appends transcript entries; ordinary tools receive no such capability.
///
/// Objects planned here receive real session-local IDs only when the writer
/// applies the plan. References between planned objects use plan-local handles,
/// so no ID escapes before the creating transaction is durable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskPlan {
    entries: Vec<PlannedEntry>,
    tasks: Vec<PlannedTask>,
}

impl TaskPlan {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.tasks.is_empty()
    }

    /// Queue an immutable transcript entry. Entries are applied in call order
    /// before any planned task, so a successor can rely on the entry existing.
    pub fn append_entry(&mut self, entry: PlannedEntry) {
        self.entries.push(entry);
    }

    /// Queue a successor task. The returned handle can be used as a dependency
    /// of a task planned later in the same plan.
    pub fn create_task(&mut self, task: PlannedTask) -> PlannedTaskRef {
        let reference = PlannedTaskRef(self.tasks.len());
        self.tasks.push(task);
        reference
    }

    pub(crate) fn entries(&self) -> &[PlannedEntry] {
        &self.entries
    }

    pub(crate) fn tasks(&self) -> &[PlannedTask] {
        &self.tasks
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedEntry {
    pub conversation_id: ConversationId,
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<ion_ai::Message>,
    pub context: ContextControl,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedTask {
    pub conversation_id: ConversationId,
    pub kind: TaskKindName,
    pub schema_version: u32,
    pub input: Value,
    pub dependencies: Vec<TaskDependency>,
}

/// A dependency on an existing durable task or on a task planned earlier in the
/// same plan. A plan may only reference already-planned tasks, which keeps the
/// successor graph acyclic by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskDependency {
    Existing(TaskId),
    Planned(PlannedTaskRef),
}

/// Plan-local handle for a successor task. Not a durable identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlannedTaskRef(usize);

impl PlannedTaskRef {
    #[must_use]
    pub fn index(self) -> usize {
        self.0
    }
}

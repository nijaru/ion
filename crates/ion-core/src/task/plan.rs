use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::TaskKindName;
use crate::conversation::context::ContextControl;
use crate::{ConversationId, EntryKind, TaskId};

/// Provisional per-plan bounds. The writer rejects an over-large plan so a bad
/// or hostile trusted kind cannot commit an unbounded transaction. These are
/// deliberately generous pending measured limits (P2).
pub const MAX_PLAN_ENTRIES: usize = 256;
pub const MAX_PLAN_TASKS: usize = 256;

static NEXT_PLAN_ID: AtomicU64 = AtomicU64::new(1);

/// A bounded set of canonical writes that a trusted task kind commits atomically
/// with its terminal outcome. This is the only way a task creates successors or
/// appends transcript entries; ordinary tools receive no such capability.
///
/// Objects planned here receive real session-local IDs only when the writer
/// applies the plan. References between planned objects use plan-local handles
/// scoped to one plan identity, so no ID escapes before the creating transaction
/// is durable and a handle cannot silently resolve inside a different plan.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskPlan {
    id: u64,
    entries: Vec<PlannedEntry>,
    tasks: Vec<PlannedTask>,
}

impl Default for TaskPlan {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskPlan {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: NEXT_PLAN_ID.fetch_add(1, Ordering::Relaxed),
            entries: Vec::new(),
            tasks: Vec::new(),
        }
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
        let reference = PlannedTaskRef {
            plan: self.id,
            index: self.tasks.len(),
        };
        self.tasks.push(task);
        reference
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
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
    /// Background successors inherit no foreground turn and survive turn
    /// cancellation. Use this for retained workers.
    pub background: bool,
}

/// A dependency on an existing durable task or on a task planned earlier in the
/// same plan. A plan may only reference already-planned tasks, which keeps the
/// successor graph acyclic by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskDependency {
    Existing(TaskId),
    Planned(PlannedTaskRef),
}

/// Plan-local handle for a successor task. Not a durable identifier. It carries
/// the identity of the plan that minted it so a handle cannot resolve inside an
/// unrelated plan that happens to have the same local index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlannedTaskRef {
    plan: u64,
    index: usize,
}

impl PlannedTaskRef {
    #[must_use]
    pub fn index(self) -> usize {
        self.index
    }

    #[must_use]
    pub fn plan_id(self) -> u64 {
        self.plan
    }
}

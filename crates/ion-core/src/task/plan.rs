use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::TaskKindName;
use crate::conversation::context::ContextControl;
use crate::{ConversationId, EntryKind, HistoryParent, TaskId};

/// Provisional per-plan bounds. The writer rejects an over-large plan so a bad
/// or hostile trusted kind cannot commit an unbounded transaction. These are
/// deliberately generous pending measured limits (P2).
pub const MAX_PLAN_ENTRIES: usize = 256;
pub const MAX_PLAN_TASKS: usize = 256;
pub const MAX_PLAN_CONVERSATIONS: usize = 64;

static NEXT_PLAN_ID: AtomicU64 = AtomicU64::new(1);

/// A bounded set of canonical writes that a trusted task kind commits atomically
/// with its terminal outcome. This is the only way a task creates successors,
/// appends transcript entries or consumes an admitted input on its own behalf;
/// ordinary tools receive no such capability.
///
/// Objects planned here receive real session-local IDs only when the writer
/// applies the plan. References between planned objects use plan-local handles
/// scoped to one plan identity, so no ID escapes before the creating transaction
/// is durable and a handle cannot silently resolve inside a different plan.
#[derive(Debug, PartialEq)]
pub struct TaskPlan {
    id: u64,
    conversations: Vec<PlannedConversation>,
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
            conversations: Vec::new(),
            entries: Vec::new(),
            tasks: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conversations.is_empty() && self.entries.is_empty() && self.tasks.is_empty()
    }

    /// Queue a conversation this plan creates and the settling task owns. It is
    /// created before any planned entry or task, so those may target it, and it
    /// becomes durable in the same commit as the outcome that created it.
    pub fn create_conversation(
        &mut self,
        conversation: PlannedConversation,
    ) -> PlannedConversationRef {
        let reference = PlannedConversationRef {
            plan: self.id,
            index: self.conversations.len(),
        };
        self.conversations.push(conversation);
        reference
    }

    /// Queue an immutable transcript entry and return a plan-local handle to it.
    /// Entries are applied in call order before any planned task, so a successor
    /// can rely on the entry existing.
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

    pub(crate) fn conversations(&self) -> &[PlannedConversation] {
        &self.conversations
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
    pub conversation_id: PlannedTarget,
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<ion_ai::Message>,
    pub context: ContextControl,
}

/// A conversation a plan writes to: one that already exists, or one this plan
/// creates. Mirrors [`TaskDependency`], so no durable ID escapes before commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlannedTarget {
    Existing(ConversationId),
    Planned(PlannedConversationRef),
}

impl PlannedTarget {
    #[must_use]
    pub const fn existing(conversation_id: ConversationId) -> Self {
        Self::Existing(conversation_id)
    }

    #[must_use]
    pub const fn planned(reference: PlannedConversationRef) -> Self {
        Self::Planned(reference)
    }
}

impl From<ConversationId> for PlannedTarget {
    fn from(conversation_id: ConversationId) -> Self {
        Self::Existing(conversation_id)
    }
}

/// A conversation this plan creates. Context seed is explicit: `parent` inherits
/// history at a stable cutoff, and `None` starts fresh.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedConversation {
    pub parent: Option<HistoryParent>,
}

impl PlannedConversation {
    #[must_use]
    pub const fn fresh() -> Self {
        Self { parent: None }
    }

    #[must_use]
    pub const fn inherited(parent: HistoryParent) -> Self {
        Self {
            parent: Some(parent),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedTask {
    pub conversation_id: PlannedTarget,
    pub kind: TaskKindName,
    pub schema_version: u32,
    pub input: Value,
    pub dependencies: Vec<TaskDependency>,
    pub turn: PlannedTurn,
}

/// What turn a successor belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlannedTurn {
    /// Join the settling task's turn, so the creator's turn covers this work.
    #[default]
    Inherit,
    /// Open the target conversation's own foreground slot for this successor,
    /// making it the root of an independent turn.
    ///
    /// A worker uses this so its run is not idle in its own conversation:
    /// a follow-up queues behind it instead of starting a second chain beside
    /// the first, and cancelling the worker's turn stops exactly that run.
    Own,
    /// Stay outside any turn. The work survives cancellation of the creator's
    /// turn; this is the trusted kind's explicit lifetime choice.
    Background,
}

/// A dependency on an existing durable task or on a task planned earlier in the
/// same plan. A plan may only reference already-planned tasks, which keeps the
/// successor graph acyclic by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskDependency {
    Existing(TaskId),
    Planned(PlannedTaskRef),
}

/// Plan-local handle for a conversation this plan creates. Not a durable
/// identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlannedConversationRef {
    plan: u64,
    index: usize,
}

impl PlannedConversationRef {
    #[must_use]
    pub fn index(self) -> usize {
        self.index
    }

    #[must_use]
    pub fn plan_id(self) -> u64 {
        self.plan
    }
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

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::TaskKindName;
use crate::conversation::context::ContextControl;
use crate::{ConversationId, EntryKind, HistoryParent, InputId, TaskId};

/// Provisional per-plan bounds. The writer rejects an over-large plan so a bad
/// or hostile trusted kind cannot commit an unbounded transaction. These are
/// deliberately generous pending measured limits (P2).
pub const MAX_PLAN_ENTRIES: usize = 256;
pub const MAX_PLAN_TASKS: usize = 256;
pub const MAX_PLAN_INPUTS: usize = 256;
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
#[derive(Debug, Clone, PartialEq)]
pub struct TaskPlan {
    id: u64,
    conversations: Vec<PlannedConversation>,
    entries: Vec<PlannedEntry>,
    inputs: Vec<PlannedInput>,
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
            inputs: Vec::new(),
            tasks: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conversations.is_empty()
            && self.entries.is_empty()
            && self.inputs.is_empty()
            && self.tasks.is_empty()
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
    pub fn append_entry(&mut self, entry: PlannedEntry) -> PlannedEntryRef {
        let reference = PlannedEntryRef {
            plan: self.id,
            index: self.entries.len(),
        };
        self.entries.push(entry);
        reference
    }

    /// Bind an admitted input to the planned entry that carries it. The input is
    /// marked consumed in the same transaction, so an input and the transcript
    /// content that answers it become durable together or not at all.
    pub fn consume_input(&mut self, input: InputId, entry: PlannedEntryRef) {
        self.inputs.push(PlannedInput { input, entry });
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

    pub(crate) fn inputs(&self) -> &[PlannedInput] {
        &self.inputs
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

/// Plan-local handle for a queued transcript entry. Not a durable identifier;
/// it resolves to a real `EntryId` only when the writer applies the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlannedEntryRef {
    plan: u64,
    index: usize,
}
impl PlannedEntryRef {
    #[must_use]
    pub fn index(self) -> usize {
        self.index
    }

    #[must_use]
    pub fn plan_id(self) -> u64 {
        self.plan
    }
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

/// One admitted input bound to the planned entry that carries its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedInput {
    pub(crate) input: InputId,
    pub(crate) entry: PlannedEntryRef,
}

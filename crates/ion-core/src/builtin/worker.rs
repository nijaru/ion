//! Spawning a retained worker conversation.
//!
//! A worker is an owned conversation, so spawning one is an ordinary plan write:
//! the conversation, its brief, its reciprocal ownership edge and its initial
//! task become durable in the same commit as the outcome that created them. A
//! crash before the settlement therefore leaves no worker, and a recovery drive
//! creates exactly one.
//!
//! The worker's run is an independent turn in its own conversation, so it is
//! never idle while it works: follow-ups queue behind it and cancelling the
//! worker's turn stops exactly that run. This is intentionally only the
//! retained spawn. The brief is seeded as a
//! transcript entry with a user projection rather than an admitted input, so
//! there is no admission receipt to replay and no input to consume; DESIGN §11
//! already allows a task with no assigned input to read its transcript, and
//! later inter-worker messages use the ordinary input substrate. Joined runs,
//! worker-local turn scope, follow-up and retirement are separate work.

use ion_ai::{Content, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{BRIEF_ENTRY, entry_kind};
use crate::conversation::context::ContextControl;
use crate::task::{
    PlannedConversation, PlannedEntry, PlannedTarget, PlannedTask, PlannedTurn, TaskPlan,
};
use crate::{
    AbortContext, ConversationId, EntryId, HistoryParent, ResourceDomain, RunningTask,
    TaskCompletion, TaskContext, TaskFuture, TaskKind, TaskKindName,
};

/// What a spawn request asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerSpec {
    /// The worker's initial instruction. Seeded as its first transcript entry.
    pub brief: String,
    #[serde(default)]
    pub seed: WorkerSeed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerSeed {
    /// No inherited history.
    #[default]
    Fresh,
    /// Inherit the parent prefix at a stable, complete-exchange cutoff.
    Inherited {
        conversation: ConversationId,
        at: EntryId,
    },
}

impl WorkerSeed {
    #[must_use]
    pub const fn parent(self) -> Option<HistoryParent> {
        match self {
            Self::Fresh => None,
            Self::Inherited { conversation, at } => Some(HistoryParent {
                conversation_id: conversation,
                at,
            }),
        }
    }
}

/// The trusted adapter that creates a retained worker conversation.
///
/// The worker's initial task shape is configuration, so this kind needs no
/// knowledge of the generation chain beyond the kind it is told to start.
pub struct WorkerKind {
    initial_kind: TaskKindName,
    initial_schema_version: u32,
}

impl WorkerKind {
    #[must_use]
    pub fn new(initial_kind: TaskKindName, initial_schema_version: u32) -> Self {
        Self {
            initial_kind,
            initial_schema_version,
        }
    }
}

impl TaskKind for WorkerKind {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        None
    }

    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let Some(spec) = parse(&task.input) else {
                // The request is client data, so a malformed one is a known
                // failure rather than an interruption to recover into.
                return Ok(TaskCompletion::failed(json!({
                    "reason": "worker task input is not a worker spec",
                })));
            };

            let mut plan = TaskPlan::new();
            let worker = match spec.seed.parent() {
                Some(parent) => plan.create_conversation(PlannedConversation::inherited(parent)),
                None => plan.create_conversation(PlannedConversation::fresh()),
            };
            plan.append_entry(PlannedEntry {
                conversation_id: PlannedTarget::planned(worker),
                kind: entry_kind(BRIEF_ENTRY),
                data: json!({"brief": spec.brief, "spawned_by": task.id.get()}),
                projection: vec![Message {
                    role: Role::User,
                    content: vec![Content::Text(spec.brief.clone())],
                    provider_replay: None,
                }],
                context: ContextControl::none(),
            });
            // The worker's run occupies its *own* conversation's turn slot, so it
            // survives cancellation of the creator's turn while a follow-up
            // queues behind it instead of starting a second chain beside it.
            plan.create_task(PlannedTask {
                conversation_id: PlannedTarget::planned(worker),
                kind: self.initial_kind.clone(),
                schema_version: self.initial_schema_version,
                input: json!({}),
                dependencies: Vec::new(),
                turn: PlannedTurn::Own,
            });

            Ok(TaskCompletion::completed(json!({
                "brief": spec.brief,
                "retained": true,
            }))
            .with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!({"reason": "spawn aborted"}))) })
    }
}

fn parse(input: &serde_json::Value) -> Option<WorkerSpec> {
    serde_json::from_value(input.clone()).ok()
}

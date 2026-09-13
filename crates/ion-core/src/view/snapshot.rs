use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, SessionId, TaskRecord,
};

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub root_conversation: ConversationId,
    pub last_commit: CommitSeq,
    pub conversations: Vec<Conversation>,
    pub entries: Vec<Entry>,
    pub inputs: Vec<Input>,
    pub tasks: Vec<TaskRecord>,
}

/// Bounded overview that does not materialize transcript or task payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub root_conversation: ConversationId,
    pub last_commit: CommitSeq,
    pub conversations: usize,
    pub entries: usize,
    pub inputs: usize,
    pub tasks: TaskCounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskCounts {
    pub pending: usize,
    pub running: usize,
    pub terminal: usize,
}

/// One page of a conversation's fork-visible transcript. `next` is the cursor
/// for the following page and is `None` when the range is exhausted.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryPage {
    pub entries: Vec<Entry>,
    pub next: Option<EntryId>,
}

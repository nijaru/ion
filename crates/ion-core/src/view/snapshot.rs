use crate::{CommitSeq, Conversation, ConversationId, Entry, Input, SessionId, TaskRecord};

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

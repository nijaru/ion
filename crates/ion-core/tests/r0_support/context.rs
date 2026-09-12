use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConversationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelMessage {
    User(String),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextEdit {
    Omit { target: EntryId },
    Replace {
        target: EntryId,
        messages: Vec<ModelMessage>,
    },
}

impl ContextEdit {
    fn target(&self) -> EntryId {
        match self {
            Self::Omit { target } | Self::Replace { target, .. } => *target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub conversation_id: ConversationId,
    pub kind: String,
    pub model: Vec<ModelMessage>,
    pub head: Option<EntryId>,
    pub edits: Vec<ContextEdit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryParent {
    pub conversation_id: ConversationId,
    pub at: EntryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: ConversationId,
    pub parent: Option<HistoryParent>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContextError {
    #[error("conversation {0:?} was not found")]
    MissingConversation(ConversationId),
    #[error("entry {0:?} was not found")]
    MissingEntry(EntryId),
    #[error("entry {entry:?} is not visible in conversation {conversation:?}")]
    InvisibleEntry {
        conversation: ConversationId,
        entry: EntryId,
    },
    #[error("context head {head:?} is not visible before entry {entry:?}")]
    InvalidHead { entry: EntryId, head: EntryId },
    #[error("context head would move backwards from {previous:?} to {next:?}")]
    HeadMovedBackwards {
        previous: EntryId,
        next: EntryId,
    },
    #[error("context edit target {target:?} is not visible before entry {entry:?}")]
    InvalidEditTarget { entry: EntryId, target: EntryId },
    #[error("fork cutoff {0:?} splits a tool exchange")]
    IncompleteExchange(EntryId),
    #[error("duplicate tool result for call {0}")]
    DuplicateToolResult(String),
    #[error("tool result references unknown call {0}")]
    UnknownToolCall(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextProjection {
    pub entry_ids: Vec<EntryId>,
    pub messages: Vec<ModelMessage>,
}

#[derive(Default)]
pub struct ContextStore {
    next_conversation: u64,
    next_entry: u64,
    conversations: HashMap<ConversationId, Conversation>,
    entries: BTreeMap<EntryId, Entry>,
}

impl ContextStore {
    pub fn create_root(&mut self) -> ConversationId {
        self.next_conversation += 1;
        let id = ConversationId(self.next_conversation);
        self.conversations.insert(id, Conversation { id, parent: None });
        id
    }

    pub fn fork(
        &mut self,
        source: ConversationId,
        at: EntryId,
    ) -> Result<ConversationId, ContextError> {
        self.require_visible(source, at)?;
        self.require_complete_exchange(source, at)?;
        self.next_conversation += 1;
        let id = ConversationId(self.next_conversation);
        self.conversations.insert(
            id,
            Conversation {
                id,
                parent: Some(HistoryParent {
                    conversation_id: source,
                    at,
                }),
            },
        );
        Ok(id)
    }

    pub fn append(
        &mut self,
        conversation_id: ConversationId,
        kind: impl Into<String>,
        model: Vec<ModelMessage>,
        head: Option<EntryId>,
        edits: Vec<ContextEdit>,
    ) -> Result<EntryId, ContextError> {
        if !self.conversations.contains_key(&conversation_id) {
            return Err(ContextError::MissingConversation(conversation_id));
        }
        self.next_entry += 1;
        let id = EntryId(self.next_entry);
        let visible_before = self.logical_entries(conversation_id, None)?;
        let visible_ids: Vec<_> = visible_before.iter().map(|entry| entry.id).collect();

        if let Some(head) = head {
            let Some(head_position) = visible_ids.iter().position(|candidate| *candidate == head) else {
                return Err(ContextError::InvalidHead { entry: id, head });
            };
            if let Some(previous_head) = visible_before.iter().rev().find_map(|entry| entry.head) {
                let previous_position = visible_ids
                    .iter()
                    .position(|candidate| *candidate == previous_head)
                    .expect("prior validated head is visible");
                if head_position < previous_position {
                    return Err(ContextError::HeadMovedBackwards {
                        previous: previous_head,
                        next: head,
                    });
                }
            }
        }

        for edit in &edits {
            let target = edit.target();
            if !visible_ids.contains(&target) {
                return Err(ContextError::InvalidEditTarget { entry: id, target });
            }
        }

        self.entries.insert(
            id,
            Entry {
                id,
                conversation_id,
                kind: kind.into(),
                model,
                head,
                edits,
            },
        );
        Ok(id)
    }

    pub fn append_self_head(
        &mut self,
        conversation_id: ConversationId,
        kind: impl Into<String>,
        model: Vec<ModelMessage>,
    ) -> Result<EntryId, ContextError> {
        if !self.conversations.contains_key(&conversation_id) {
            return Err(ContextError::MissingConversation(conversation_id));
        }
        self.next_entry += 1;
        let id = EntryId(self.next_entry);
        self.entries.insert(
            id,
            Entry {
                id,
                conversation_id,
                kind: kind.into(),
                model,
                head: Some(id),
                edits: Vec::new(),
            },
        );
        Ok(id)
    }

    pub fn project(
        &self,
        conversation_id: ConversationId,
        through: Option<EntryId>,
    ) -> Result<ContextProjection, ContextError> {
        let logical = self.logical_entries(conversation_id, through)?;
        if logical.is_empty() {
            return Ok(ContextProjection {
                entry_ids: Vec::new(),
                messages: Vec::new(),
            });
        }

        let newest_head = logical.iter().rev().find(|entry| entry.head.is_some());
        let mut selected = if let Some(head_entry) = newest_head {
            let boundary = head_entry.head.expect("head entry has boundary");
            let start = logical
                .iter()
                .position(|entry| entry.id == boundary)
                .ok_or(ContextError::InvalidHead {
                    entry: head_entry.id,
                    head: boundary,
                })?;
            let mut selected = Vec::with_capacity(logical.len() - start + 1);
            selected.push(*head_entry);
            selected.extend(
                logical[start..]
                    .iter()
                    .copied()
                    .filter(|entry| entry.head.is_none()),
            );
            selected
        } else {
            logical.clone()
        };

        let selected_ids: Vec<_> = selected.iter().map(|entry| entry.id).collect();
        let mut winning_edits = HashMap::new();
        for entry in &logical {
            if selected_ids.contains(&entry.id) {
                for edit in &entry.edits {
                    winning_edits.insert(edit.target(), edit.clone());
                }
            }
        }

        let mut projected = Vec::new();
        for entry in selected.drain(..) {
            match winning_edits.get(&entry.id) {
                Some(ContextEdit::Omit { .. }) => {}
                Some(ContextEdit::Replace { messages, .. }) => projected.extend(messages.clone()),
                None => projected.extend(entry.model.clone()),
            }
        }
        let messages = normalize_tool_results(projected)?;
        Ok(ContextProjection {
            entry_ids: selected_ids,
            messages,
        })
    }

    pub fn logical_entry_ids(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<EntryId>, ContextError> {
        Ok(self
            .logical_entries(conversation_id, None)?
            .into_iter()
            .map(|entry| entry.id)
            .collect())
    }

    fn logical_entries(
        &self,
        conversation_id: ConversationId,
        through: Option<EntryId>,
    ) -> Result<Vec<&Entry>, ContextError> {
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(ContextError::MissingConversation(conversation_id))?;
        let mut logical = if let Some(parent) = conversation.parent {
            self.logical_entries(parent.conversation_id, Some(parent.at))?
        } else {
            Vec::new()
        };
        logical.extend(
            self.entries
                .values()
                .filter(|entry| entry.conversation_id == conversation_id),
        );
        if let Some(through) = through {
            let Some(position) = logical.iter().position(|entry| entry.id == through) else {
                return Err(ContextError::InvisibleEntry {
                    conversation: conversation_id,
                    entry: through,
                });
            };
            logical.truncate(position + 1);
        }
        Ok(logical)
    }

    fn require_visible(
        &self,
        conversation_id: ConversationId,
        entry_id: EntryId,
    ) -> Result<(), ContextError> {
        if self
            .logical_entries(conversation_id, None)?
            .iter()
            .any(|entry| entry.id == entry_id)
        {
            Ok(())
        } else if self.entries.contains_key(&entry_id) {
            Err(ContextError::InvisibleEntry {
                conversation: conversation_id,
                entry: entry_id,
            })
        } else {
            Err(ContextError::MissingEntry(entry_id))
        }
    }

    fn require_complete_exchange(
        &self,
        conversation_id: ConversationId,
        through: EntryId,
    ) -> Result<(), ContextError> {
        let logical = self.logical_entries(conversation_id, Some(through))?;
        let mut open_calls = Vec::new();
        for entry in logical {
            for message in &entry.model {
                match message {
                    ModelMessage::Assistant { calls, .. } => {
                        open_calls.extend(calls.iter().map(|call| call.id.clone()));
                    }
                    ModelMessage::ToolResult { call_id, .. } => {
                        open_calls.retain(|candidate| candidate != call_id);
                    }
                    ModelMessage::User(_) => {}
                }
            }
        }
        if open_calls.is_empty() {
            Ok(())
        } else {
            Err(ContextError::IncompleteExchange(through))
        }
    }
}

fn normalize_tool_results(messages: Vec<ModelMessage>) -> Result<Vec<ModelMessage>, ContextError> {
    let mut output = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        match &messages[index] {
            ModelMessage::Assistant { calls, .. } if !calls.is_empty() => {
                output.push(messages[index].clone());
                index += 1;
                let mut results = HashMap::new();
                while index < messages.len() {
                    let ModelMessage::ToolResult { call_id, .. } = &messages[index] else {
                        break;
                    };
                    if results.insert(call_id.clone(), messages[index].clone()).is_some() {
                        return Err(ContextError::DuplicateToolResult(call_id.clone()));
                    }
                    index += 1;
                }
                for call in calls {
                    if let Some(result) = results.remove(&call.id) {
                        output.push(result);
                    }
                }
                if let Some(unknown) = results.keys().next() {
                    return Err(ContextError::UnknownToolCall(unknown.clone()));
                }
            }
            _ => {
                output.push(messages[index].clone());
                index += 1;
            }
        }
    }
    Ok(output)
}

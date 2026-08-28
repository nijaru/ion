from pathlib import Path

p = Path('.github/agent_message_patch.py')
text = p.read_text()

old = '''text = text.replace(\n    \'\'\'    /// True when queued steers wait for the next reasoning boundary.\\n    #[must_use]\\n    pub fn has_queued_steers(&self) -> bool {\\n        !self.steers.is_empty()\\n    }\\n\'\'\',\n    \'\'\'    /// True when durable continuation inputs wait for the next reasoning boundary.\\n    #[must_use]\\n    pub fn has_queued_inputs(&self) -> bool {\\n        !self.pending_inputs.is_empty()\\n    }\\n\'\'\',\n)\n# Add explicit root acceptance'''
new = '''text = text.replace(\n    \'\'\'    /// True when queued steers wait for the next reasoning boundary.\\n    #[must_use]\\n    pub fn has_queued_steers(&self) -> bool {\\n        !self.steers.is_empty()\\n    }\\n\'\'\',\n    \'\'\'    /// True when durable continuation inputs wait for the next reasoning boundary.\\n    #[must_use]\\n    pub fn has_queued_inputs(&self) -> bool {\\n        !self.pending_inputs.is_empty()\\n    }\\n\'\'\',\n)\np.write_text(text)\n# Add explicit root acceptance'''
if old not in text:
    raise SystemExit('could not install pre-insert write')
text = text.replace(old, new, 1)

old = ''')\n# Replace apply_inbox wholesale for typed continuation input.\nold_apply ='''
new = ''')\ntext = p.read_text()\n# Replace apply_inbox wholesale for typed continuation input.\nold_apply ='''
if old not in text:
    raise SystemExit('could not install post-insert reload')
text = text.replace(old, new, 1)

text = text.replace(
    'if matches!(root_inbox.kind, InboxKind::Prompt)',
    'if matches!(&root_inbox.kind, InboxKind::Prompt)',
)
text = text.replace(
    '.filter(|item| !matches!(item.kind, InboxKind::Prompt))',
    '.filter(|item| !matches!(&item.kind, InboxKind::Prompt))',
)
text = text.replace(
    'if let InboxKind::AgentMessage { from } = item.kind {',
    'if let InboxKind::AgentMessage { from } = &item.kind {',
)

text = text.replace(
    '''        let (machine, applied) = match input.kind {\\n            InboxKind::Prompt => {\\n                OperationMachine::accept(operation_id, input.text.clone(), tool_registry.specs())\\n            }\\n            InboxKind::AgentMessage { from } => OperationMachine::accept_agent_message(\\n                operation_id,\\n                from,\\n                input.text.clone(),\\n                tool_registry.specs(),\\n            ),\\n            InboxKind::Steer => unreachable!("steer cannot open a new operation"),\\n        };''',
    '''        let (machine, applied) = match &input.kind {\\n            InboxKind::Prompt => {\\n                OperationMachine::accept(operation_id, input.text.clone(), tool_registry.specs())\\n            }\\n            InboxKind::AgentMessage { from } => OperationMachine::accept_agent_message(\\n                operation_id,\\n                *from,\\n                input.text.clone(),\\n                tool_registry.specs(),\\n            ),\\n            InboxKind::Steer => unreachable!("steer cannot open a new operation"),\\n        };''',
)

text += r"""

p = Path("crates/ion-core/src/store/sql.rs")
source = p.read_text()
source = source.replace(
    '''        SessionEntry::UserMessage { .. } => "user_message",\n        SessionEntry::AssistantMessage { .. } => "assistant_message",''',
    '''        SessionEntry::UserMessage { .. } => "user_message",\n        SessionEntry::AgentMessage { .. } => "agent_message",\n        SessionEntry::AssistantMessage { .. } => "assistant_message",''',
    1,
)
p.write_text(source)

p = Path("crates/ion-core/src/tests/compaction.rs")
source = p.read_text()
source = source.replace(
    '''        SessionEntry::UserMessage { .. } => "user_message",\n        SessionEntry::AssistantMessage { .. } => "assistant_message",''',
    '''        SessionEntry::UserMessage { .. } => "user_message",\n        SessionEntry::AgentMessage { .. } => "agent_message",\n        SessionEntry::AssistantMessage { .. } => "assistant_message",''',
    1,
)
p.write_text(source)
"""

p.write_text(text)

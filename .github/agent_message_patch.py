from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"anchor missing in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))


def insert_after(path: str, anchor: str, addition: str) -> None:
    replace(path, anchor, anchor + addition)


# Operation input is no longer synonymous with a user steer. Preserve durable
# sender identity all the way into semantic history and projection.
p = Path("crates/ion-core/src/operation/mod.rs")
text = p.read_text()
text = text.replace("use crate::ids::OperationId;", "use crate::ids::{AgentId, OperationId};")
text = text.replace(
    '''    /// Joins the active operation; applied at the next safe continuation\n    /// boundary (the next model step).\n    Steer,\n''',
    '''    /// Joins the active operation; applied at the next safe continuation\n    /// boundary (the next model step).\n    Steer,\n    /// Durable direct input from another retained agent in the same family.\n    AgentMessage { from: AgentId },\n''',
)
text = text.replace(
    '''    UserMessage {\n        text: String,\n    },\n''',
    '''    UserMessage {\n        text: String,\n    },\n    AgentMessage {\n        from: AgentId,\n        text: String,\n    },\n''',
)
text = text.replace("    steers: Vec<InboxItem>,", "    pending_inputs: Vec<InboxItem>,")
text = text.replace("            steers: Vec::new(),", "            pending_inputs: Vec::new(),")
text = text.replace("        steers: Vec<InboxItem>,", "        pending_inputs: Vec<InboxItem>,")
text = text.replace("            steers,", "            pending_inputs,")
text = text.replace(
    '''    /// True when queued steers wait for the next reasoning boundary.\n    #[must_use]\n    pub fn has_queued_steers(&self) -> bool {\n        !self.steers.is_empty()\n    }\n''',
    '''    /// True when durable continuation inputs wait for the next reasoning boundary.\n    #[must_use]\n    pub fn has_queued_inputs(&self) -> bool {\n        !self.pending_inputs.is_empty()\n    }\n''',
)
# Add explicit root acceptance for an agent message without changing the public
# prompt convenience constructor used by existing callers/tests.
insert_after(
    "crates/ion-core/src/operation/mod.rs",
    '''    pub fn accept(\n        operation_id: OperationId,\n        prompt: impl Into<String>,\n        tools: Vec<ToolSpec>,\n    ) -> (Self, Applied) {\n        let prompt = prompt.into();\n        let applied = Applied {\n            state: OperationState::Accepted,\n            entries: vec![SessionEntry::UserMessage {\n                text: prompt.clone(),\n            }],\n            intents: Vec::new(),\n            cancel_effects: false,\n        };\n        let machine = Self {\n            operation_id,\n            cancel_requested: false,\n            state: OperationState::Accepted,\n            pending_inputs: Vec::new(),\n            step_tools: tools,\n            prompt,\n        };\n        (machine, applied)\n    }\n''',
    '''\n    #[must_use]\n    pub(crate) fn accept_agent_message(\n        operation_id: OperationId,\n        from: AgentId,\n        text: String,\n        tools: Vec<ToolSpec>,\n    ) -> (Self, Applied) {\n        let applied = Applied {\n            state: OperationState::Accepted,\n            entries: vec![SessionEntry::AgentMessage {\n                from,\n                text: text.clone(),\n            }],\n            intents: Vec::new(),\n            cancel_effects: false,\n        };\n        let machine = Self {\n            operation_id,\n            cancel_requested: false,\n            state: OperationState::Accepted,\n            pending_inputs: Vec::new(),\n            step_tools: tools,\n            prompt: text,\n        };\n        (machine, applied)\n    }\n''',
)
# Replace apply_inbox wholesale for typed continuation input.
old_apply = '''    fn apply_inbox(&mut self, item: InboxItem) -> Result<Applied, TransitionError> {\n        let applies_now = match item.kind {\n            // Steer joins at the next reasoning boundary (§9.2).\n            InboxKind::Steer => matches!(\n                self.state,\n                OperationState::Accepted\n                    | OperationState::NeedAssistant\n                    | OperationState::NeedContinuation\n            ),\n            InboxKind::Prompt => {\n                return Err(TransitionError {\n                    state: self.state.kind(),\n                    transition: "apply_inbox",\n                });\n            }\n        };\n        if applies_now {\n            self.state = OperationState::NeedAssistant;\n            Ok(Applied {\n                state: self.state.clone(),\n                entries: vec![SessionEntry::UserMessage { text: item.text }],\n                intents: Vec::new(),\n                cancel_effects: false,\n            })\n        } else if matches!(\n            self.state,\n            OperationState::Accepted\n                | OperationState::NeedAssistant\n                | OperationState::NeedContinuation\n                | OperationState::AssistantEffectPending\n                | OperationState::ToolsPlanned { .. }\n                | OperationState::ApprovalPending { .. }\n                | OperationState::ToolEffectPending { .. }\n        ) {\n            match item.kind {\n                InboxKind::Steer => self.steers.push(item),\n                InboxKind::Prompt => unreachable!("rejected above"),\n            }\n            Ok(Applied {\n                state: self.state.clone(),\n                entries: Vec::new(),\n                intents: Vec::new(),\n                cancel_effects: false,\n            })\n        } else {\n            Err(TransitionError {\n                state: self.state.kind(),\n                transition: "apply_inbox",\n            })\n        }\n    }\n'''
new_apply = '''    fn apply_inbox(&mut self, item: InboxItem) -> Result<Applied, TransitionError> {\n        let applies_now = match &item.kind {\n            // Continuation inputs join at the next reasoning boundary (§9.2).\n            InboxKind::Steer | InboxKind::AgentMessage { .. } => matches!(\n                self.state,\n                OperationState::Accepted\n                    | OperationState::NeedAssistant\n                    | OperationState::NeedContinuation\n            ),\n            InboxKind::Prompt => {\n                return Err(TransitionError {\n                    state: self.state.kind(),\n                    transition: "apply_inbox",\n                });\n            }\n        };\n        if applies_now {\n            self.state = OperationState::NeedAssistant;\n            let entry = match item.kind {\n                InboxKind::Steer => SessionEntry::UserMessage { text: item.text },\n                InboxKind::AgentMessage { from } => SessionEntry::AgentMessage {\n                    from,\n                    text: item.text,\n                },\n                InboxKind::Prompt => unreachable!("rejected above"),\n            };\n            Ok(Applied {\n                state: self.state.clone(),\n                entries: vec![entry],\n                intents: Vec::new(),\n                cancel_effects: false,\n            })\n        } else if matches!(\n            self.state,\n            OperationState::Accepted\n                | OperationState::NeedAssistant\n                | OperationState::NeedContinuation\n                | OperationState::AssistantEffectPending\n                | OperationState::ToolsPlanned { .. }\n                | OperationState::ApprovalPending { .. }\n                | OperationState::ToolEffectPending { .. }\n        ) {\n            match item.kind {\n                InboxKind::Steer | InboxKind::AgentMessage { .. } => {\n                    self.pending_inputs.push(item);\n                }\n                InboxKind::Prompt => unreachable!("rejected above"),\n            }\n            Ok(Applied {\n                state: self.state.clone(),\n                entries: Vec::new(),\n                intents: Vec::new(),\n                cancel_effects: false,\n            })\n        } else {\n            Err(TransitionError {\n                state: self.state.kind(),\n                transition: "apply_inbox",\n            })\n        }\n    }\n'''
if old_apply not in text:
    raise SystemExit("apply_inbox anchor missing")
text = text.replace(old_apply, new_apply, 1)
text = text.replace("if self.has_queued_steers()", "if self.has_queued_inputs()")
text = text.replace(
    '''    /// Drain queued steers at a reasoning boundary. Each applied item\n    /// moves the machine to [`OperationState::NeedAssistant`].\n    pub fn drain_steers(&mut self) -> Result<Vec<Applied>, TransitionError> {\n        let mut applied = Vec::new();\n        while let Some(item) = self.steers.first().cloned() {\n            self.steers.remove(0);\n            applied.push(self.apply_inbox(item)?);\n        }\n        Ok(applied)\n    }\n''',
    '''    /// Drain queued continuation inputs at a reasoning boundary. Each\n    /// applied item moves the machine to [`OperationState::NeedAssistant`].\n    pub fn drain_inputs(&mut self) -> Result<Vec<Applied>, TransitionError> {\n        let pending = std::mem::take(&mut self.pending_inputs);\n        pending\n            .into_iter()\n            .map(|item| self.apply_inbox(item))\n            .collect()\n    }\n''',
)
p.write_text(text)

# Context projects provenance as a stable user-role message; providers do not
# need a provider-specific role to preserve who sent it.
p = Path("crates/ion-core/src/context.rs")
text = p.read_text()
text = text.replace(
    '''            SessionEntry::UserMessage { text } => {\n                messages.push(ContextMessage::User {\n                    content: text.clone(),\n                });\n            }\n''',
    '''            SessionEntry::UserMessage { text } => {\n                messages.push(ContextMessage::User {\n                    content: text.clone(),\n                });\n            }\n            SessionEntry::AgentMessage { from, text } => {\n                messages.push(ContextMessage::User {\n                    content: format!("[Message from {from}]\\n{text}"),\n                });\n            }\n''',
)
p.write_text(text)

# Store acceptance validates that typed root inbox and semantic entry agree,
# and validates an agent sender against the session family before insertion.
p = Path("crates/ion-core/src/store/sql.rs")
text = p.read_text()
old = '''    let user_prompt = match &entry.entry {\n        SessionEntry::UserMessage { text } => text,\n        _ => {\n            return Err(rusqlite::Error::InvalidParameterName(\n                "operation acceptance must append its user prompt".to_owned(),\n            ));\n        }\n    };\n    if user_prompt != &checkpoint.payload.prompt || root_inbox.text.as_str() != user_prompt {\n        return Err(rusqlite::Error::InvalidParameterName(\n            "operation prompt disagrees with its accepted entry".to_owned(),\n        ));\n    }\n    match (pending_entry, pending_prompt) {\n        (None, None) => {}\n        (Some(reserved_entry), Some(reserved_prompt))\n            if reserved_entry == entry_id && reserved_prompt.as_str() == user_prompt => {}\n        _ => {\n            return Err(rusqlite::Error::InvalidParameterName(\n                "operation does not match the lane's reserved next run".to_owned(),\n            ));\n        }\n    }\n'''
new = '''    let accepted_text = match (&root_inbox.kind, &entry.entry) {\n        (InboxKind::Prompt, SessionEntry::UserMessage { text }) => text,\n        (\n            InboxKind::AgentMessage { from },\n            SessionEntry::AgentMessage {\n                from: entry_from,\n                text,\n            },\n        ) if from == entry_from => text,\n        _ => {\n            return Err(rusqlite::Error::InvalidParameterName(\n                "operation root inbox disagrees with its semantic entry".to_owned(),\n            ));\n        }\n    };\n    if accepted_text != &checkpoint.payload.prompt || root_inbox.text.as_str() != accepted_text {\n        return Err(rusqlite::Error::InvalidParameterName(\n            "operation input disagrees with its accepted entry".to_owned(),\n        ));\n    }\n    match (pending_entry, pending_prompt) {\n        (None, None) => {}\n        (Some(reserved_entry), Some(reserved_prompt))\n            if matches!(root_inbox.kind, InboxKind::Prompt)\n                && reserved_entry == entry_id\n                && reserved_prompt.as_str() == accepted_text => {}\n        _ => {\n            return Err(rusqlite::Error::InvalidParameterName(\n                "operation does not match the lane's reserved next run".to_owned(),\n            ));\n        }\n    }\n'''
if old not in text:
    raise SystemExit("begin operation input validation anchor missing")
text = text.replace(old, new, 1)
# Sender validation in insert_inbox.
old_insert = '''    let status = match item.status {\n        InboxStatus::Pending => "pending",\n        InboxStatus::Applied => "applied",\n    };\n    connection.execute(\n'''
new_insert = '''    let status = match item.status {\n        InboxStatus::Pending => "pending",\n        InboxStatus::Applied => "applied",\n    };\n    if let InboxKind::AgentMessage { from } = item.kind {\n        let sender_exists: bool = connection.query_row(\n            "SELECT EXISTS(\n                SELECT 1 FROM agents WHERE id = ?1 AND family_session_id = ?2\n            )",\n            rusqlite::params![\n                from.as_uuid().to_string(),\n                session_id.as_uuid().to_string(),\n            ],\n            |row| row.get(0),\n        )?;\n        if !sender_exists {\n            return Err(rusqlite::Error::InvalidParameterName(format!(\n                "agent message sender {from} is not retained by this family"\n            )));\n        }\n    }\n    connection.execute(\n'''
if old_insert not in text:
    raise SystemExit("insert inbox anchor missing")
text = text.replace(old_insert, new_insert, 1)
text = text.replace(
    '''        SessionEntry::UserMessage { .. } => "user",\n''',
    '''        SessionEntry::UserMessage { .. } => "user",\n        SessionEntry::AgentMessage { .. } => "agent_message",\n''',
)
p.write_text(text)

# Runtime pending-input residency + command surface.
p = Path("crates/ion-core/src/runtime/mod.rs")
text = p.read_text()
text = text.replace("    pending_steers: Vec<InboxId>,", "    pending_inputs: Vec<InboxId>,")
text = text.replace("                pending_steers: Vec::new(),", "                pending_inputs: Vec::new(),")
text = text.replace("                pending_steers: operation", "                pending_inputs: operation")
text = text.replace(
    '''                    .filter(|item| item.kind == InboxKind::Steer)\n''',
    '''                    .filter(|item| !matches!(item.kind, InboxKind::Prompt))\n''',
)
# There are two recovery filters; replace remaining exact form too.
text = text.replace(
    '''                    .filter(|item| item.kind == InboxKind::Steer)\n''',
    '''                    .filter(|item| !matches!(item.kind, InboxKind::Prompt))\n''',
)
text = text.replace("pending_steers", "pending_inputs")
text = text.replace("has_queued_steers()", "has_queued_inputs()")
text = text.replace("drain_steers()", "drain_inputs()")
text = text.replace("queued steers", "queued continuation inputs")
text = text.replace("steer drain", "input drain")
# Add command variant.
text = text.replace(
    '''    Steer {\n        text: String,\n        reply: oneshot::Sender<Result<(), CommandError>>,\n    },\n''',
    '''    Steer {\n        text: String,\n        reply: oneshot::Sender<Result<(), CommandError>>,\n    },\n    SendAgentMessage {\n        from: AgentId,\n        lane_name: String,\n        text: String,\n        reply: oneshot::Sender<Result<OperationId, CommandError>>,\n    },\n''',
)
# SessionHandle crate-private message primitive.
anchor = '''    /// Join the active operation at its next reasoning boundary\n    /// (DESIGN.md §9.2).\n    pub async fn steer(&self, text: impl Into<String>) -> Result<(), CommandError> {\n        let (reply, rx) = oneshot::channel();\n        self.tx\n            .try_send(SessionCommand::Steer {\n                text: text.into(),\n                reply,\n            })\n            .map_err(command_send_error)?;\n        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n'''
addition = '''\n    pub(crate) async fn send_agent_message(\n        &self,\n        from: AgentId,\n        lane_name: impl Into<String>,\n        text: impl Into<String>,\n    ) -> Result<OperationId, CommandError> {\n        let (reply, rx) = oneshot::channel();\n        self.tx\n            .try_send(SessionCommand::SendAgentMessage {\n                from,\n                lane_name: lane_name.into(),\n                text: text.into(),\n                reply,\n            })\n            .map_err(command_send_error)?;\n        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n'''
if anchor not in text:
    raise SystemExit("steer handle anchor missing")
text = text.replace(anchor, anchor + addition, 1)
# Command handler.
text = text.replace(
    '''            SessionCommand::Steer { text, reply } => {\n                let _ = reply.send(self.enqueue_steer(text).await);\n                false\n            }\n''',
    '''            SessionCommand::Steer { text, reply } => {\n                let _ = reply.send(self.enqueue_steer(text).await);\n                false\n            }\n            SessionCommand::SendAgentMessage {\n                from,\n                lane_name,\n                text,\n                reply,\n            } => {\n                let _ = reply.send(self.send_agent_message(from, lane_name, text).await);\n                false\n            }\n''',
)
# Generalize operation acceptance internally while preserving prompt wrapper.
old_accept_sig = '''    async fn accept_operation_record(\n        &mut self,\n        lane_name: &str,\n        prompt: String,\n        reservation: Option<crate::session::lane::NextRun>,\n    ) -> Result<(ActiveOperation, crate::ids::EntryId), CommandError> {\n        let operation_id = OperationId::generate();\n        let tool_registry = self.tools.snapshot();\n        let (machine, applied) =\n            OperationMachine::accept(operation_id, prompt.clone(), tool_registry.specs());\n        let capability_snapshot = tool_registry.capability_snapshot();\n        let root_inbox = InboxRecord {\n            id: InboxId::generate(),\n            kind: InboxKind::Prompt,\n            text: prompt,\n            status: InboxStatus::Applied,\n        };\n'''
new_accept_sig = '''    async fn accept_operation_record(\n        &mut self,\n        lane_name: &str,\n        prompt: String,\n        reservation: Option<crate::session::lane::NextRun>,\n    ) -> Result<(ActiveOperation, crate::ids::EntryId), CommandError> {\n        self.accept_operation_input(\n            lane_name,\n            InboxItem {\n                kind: InboxKind::Prompt,\n                text: prompt,\n            },\n            reservation,\n        )\n        .await\n    }\n\n    async fn accept_operation_input(\n        &mut self,\n        lane_name: &str,\n        input: InboxItem,\n        reservation: Option<crate::session::lane::NextRun>,\n    ) -> Result<(ActiveOperation, crate::ids::EntryId), CommandError> {\n        let operation_id = OperationId::generate();\n        let tool_registry = self.tools.snapshot();\n        let (machine, applied) = match input.kind {\n            InboxKind::Prompt => {\n                OperationMachine::accept(operation_id, input.text.clone(), tool_registry.specs())\n            }\n            InboxKind::AgentMessage { from } => OperationMachine::accept_agent_message(\n                operation_id,\n                from,\n                input.text.clone(),\n                tool_registry.specs(),\n            ),\n            InboxKind::Steer => unreachable!("steer cannot open a new operation"),\n        };\n        let capability_snapshot = tool_registry.capability_snapshot();\n        let root_inbox = InboxRecord {\n            id: InboxId::generate(),\n            kind: input.kind,\n            text: input.text,\n            status: InboxStatus::Applied,\n        };\n'''
if old_accept_sig not in text:
    raise SystemExit("accept operation anchor missing")
text = text.replace(old_accept_sig, new_accept_sig, 1)
# Refactor steer into a generic operation inbox helper and add idle/active message delivery.
old_steer = '''    async fn enqueue_steer(&mut self, text: String) -> Result<(), CommandError> {\n        if self.closed {\n            return Err(CommandError::Closed);\n        }\n        let inbox_id = InboxId::generate();\n        // Stage on a full clone; a failed commit discards the clone and\n        // never mutates live state (DESIGN.md §26.2).\n        let mut staged = self\n            .main_active()\n            .cloned()\n            .ok_or(CommandError::NoActiveOperation)?;\n        let applied = staged\n            .machine\n            .apply(Transition::ApplyInbox {\n                item: InboxItem {\n                    kind: InboxKind::Steer,\n                    text: text.clone(),\n                },\n            })\n            .expect("inbox apply from an active operation");\n        let applied_now = !applied.entries.is_empty();\n        let record = InboxRecord {\n            id: inbox_id,\n            kind: InboxKind::Steer,\n            text,\n            status: if applied_now {\n                InboxStatus::Applied\n            } else {\n                InboxStatus::Pending\n            },\n        };\n        let (request, new_entry_seq) = build_commit_request(\n            self.session_id,\n            &staged,\n            staged.state_seq + 1,\n            self.next_entry_seq,\n            applied.entries,\n            Vec::new(),\n            Vec::new(),\n            Vec::new(),\n            vec![record],\n            Vec::new(),\n            Vec::new(),\n        );\n        self.commit_transition(request)\n            .await\n            .map_err(persistence_command_error)?;\n\n        self.next_entry_seq = new_entry_seq;\n        staged.state_seq += 1;\n        if !applied_now {\n            staged.pending_inputs.push(inbox_id);\n        }\n        let operation_id = staged.machine.operation_id();\n        self.install_active(staged);\n        self.advance(operation_id).await;\n        Ok(())\n    }\n'''
new_steer = '''    async fn enqueue_steer(&mut self, text: String) -> Result<(), CommandError> {\n        if self.closed {\n            return Err(CommandError::Closed);\n        }\n        let operation_id = self\n            .main_active()\n            .map(|active| active.machine.operation_id())\n            .ok_or(CommandError::NoActiveOperation)?;\n        self.enqueue_operation_input(\n            operation_id,\n            InboxItem {\n                kind: InboxKind::Steer,\n                text,\n            },\n        )\n        .await\n    }\n\n    async fn send_agent_message(\n        &mut self,\n        from: AgentId,\n        lane_name: String,\n        text: String,\n    ) -> Result<OperationId, CommandError> {\n        if self.closed {\n            return Err(CommandError::Closed);\n        }\n        if self.lane(&lane_name).is_none() {\n            return Err(CommandError::LaneNotFound(lane_name));\n        }\n        let input = InboxItem {\n            kind: InboxKind::AgentMessage { from },\n            text,\n        };\n        if let Some(operation_id) = self.lane_resident_id(&lane_name) {\n            self.enqueue_operation_input(operation_id, input).await?;\n            return Ok(operation_id);\n        }\n        if let Some(pending) = self.lane_pending_next_run(&lane_name) {\n            return Err(CommandError::NextRunQueued {\n                entry_id: pending.entry_id,\n            });\n        }\n        let (active, _) = self\n            .accept_operation_input(&lane_name, input, None)\n            .await?;\n        let operation_id = active.machine.operation_id();\n        self.start_active(&lane_name, active);\n        self.advance(operation_id).await;\n        Ok(operation_id)\n    }\n\n    async fn enqueue_operation_input(\n        &mut self,\n        operation_id: OperationId,\n        item: InboxItem,\n    ) -> Result<(), CommandError> {\n        let inbox_id = InboxId::generate();\n        // Stage on a full clone; a failed commit discards the clone and\n        // never mutates live state (DESIGN.md §26.2).\n        let mut staged = self\n            .active(operation_id)\n            .cloned()\n            .ok_or(CommandError::NotActive { operation_id })?;\n        let record_kind = item.kind.clone();\n        let record_text = item.text.clone();\n        let applied = staged\n            .machine\n            .apply(Transition::ApplyInbox { item })\n            .expect("continuation input apply from an active operation");\n        let applied_now = !applied.entries.is_empty();\n        let record = InboxRecord {\n            id: inbox_id,\n            kind: record_kind,\n            text: record_text,\n            status: if applied_now {\n                InboxStatus::Applied\n            } else {\n                InboxStatus::Pending\n            },\n        };\n        let (request, new_entry_seq) = build_commit_request(\n            self.session_id,\n            &staged,\n            staged.state_seq + 1,\n            self.next_entry_seq,\n            applied.entries,\n            Vec::new(),\n            Vec::new(),\n            Vec::new(),\n            vec![record],\n            Vec::new(),\n            Vec::new(),\n        );\n        self.commit_transition(request)\n            .await\n            .map_err(persistence_command_error)?;\n\n        self.next_entry_seq = new_entry_seq;\n        staged.state_seq += 1;\n        if !applied_now {\n            staged.pending_inputs.push(inbox_id);\n        }\n        self.install_active(staged);\n        self.advance(operation_id).await;\n        Ok(())\n    }\n'''
if old_steer not in text:
    raise SystemExit("enqueue steer anchor missing")
text = text.replace(old_steer, new_steer, 1)
p.write_text(text)

# Family-level semantic send validates both addresses and delegates delivery to
# the target session writer.
p = Path("crates/ion-core/src/agent.rs")
text = p.read_text()
anchor = '''    /// Observe authoritative durable operation state for one retained agent.\n'''
addition = '''    /// Send a durable message between retained agents. An active target\n    /// receives it as continuation input at the next reasoning boundary; an\n    /// idle target starts a new operation rooted in the typed agent message.\n    pub async fn send(\n        &self,\n        from: AgentId,\n        to: AgentId,\n        text: impl Into<String>,\n    ) -> Result<OperationId, Error> {\n        let lane_name = {\n            let retained = self.retained.lock().expect("agent family poisoned");\n            if !retained.contains_key(&from) {\n                return Err(Error::UnknownAgent(from));\n            }\n            retained\n                .get(&to)\n                .map(|agent| agent.lane_name.clone())\n                .ok_or(Error::UnknownAgent(to))?\n        };\n        Ok(self\n            .session\n            .send_agent_message(from, lane_name, text)\n            .await?)\n    }\n\n'''
if anchor not in text:
    raise SystemExit("family status anchor missing")
text = text.replace(anchor, addition + anchor, 1)
p.write_text(text)

# Update test helper exhaustive entry-kind projection.
p = Path("crates/ion-core/src/tests/support.rs")
text = p.read_text().replace(
    '''            crate::SessionEntry::UserMessage { .. } => "user_message",\n''',
    '''            crate::SessionEntry::UserMessage { .. } => "user_message",\n            crate::SessionEntry::AgentMessage { .. } => "agent_message",\n''',
)
p.write_text(text)

# Mechanical test/internal API rename for generalized pending input vocabulary.
for path in Path("crates/ion-core/src").rglob("*.rs"):
    text = path.read_text()
    text = text.replace("drain_steers", "drain_inputs")
    text = text.replace("has_queued_steers", "has_queued_inputs")
    path.write_text(text)

# Behavioral messaging tests: active delivery causes a continuation; idle
# delivery creates a new operation; sender provenance survives durable history
# and the model-neutral projection.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
text += r'''

#[tokio::test]
async fn agent_messages_use_durable_inbox_and_preserve_sender_provenance() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(180),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        provider.clone(),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    let sender = family.admit_lane(root).await.expect("sender admission");
    let target = family.admit_lane(root).await.expect("target admission");
    let target_operation = family
        .start(target, "initial target work")
        .await
        .expect("target start");

    timeout(Duration::from_secs(2), async {
        loop {
            if !provider.requests().is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first provider step started");

    assert_eq!(
        family
            .send(sender, target, "coordinate on the API boundary")
            .await
            .expect("active message delivery"),
        target_operation
    );
    family
        .wait_one(target, CancellationToken::new(), None)
        .await
        .expect("target completion");

    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "queued message must cause a continuation step");
    assert!(requests[1].plan.messages.iter().any(|message| {
        matches!(
            message,
            crate::ContextMessage::User { content }
                if content == &format!(
                    "[Message from {sender}]\ncoordinate on the API boundary"
                )
        )
    }));

    let idle_target = family.admit_lane(root).await.expect("idle target admission");
    let idle_operation = family
        .send(sender, idle_target, "start from this handoff")
        .await
        .expect("idle message delivery");
    assert!(matches!(
        family.status(idle_target).await.expect("idle target became active"),
        crate::AgentStatus::Active { operation_id, .. } if operation_id == idle_operation
    ));
    family
        .wait_one(idle_target, CancellationToken::new(), None)
        .await
        .expect("idle-message operation completion");

    let loaded = store.load(runtime.session_id()).await.expect("load messages");
    let messages = loaded
        .entries
        .iter()
        .filter_map(|entry| match &entry.entry {
            crate::SessionEntry::AgentMessage { from, text } => Some((*from, text.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec![
            (sender, "coordinate on the API boundary".to_owned()),
            (sender, "start from this handoff".to_owned()),
        ]
    );

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
p.write_text(text)

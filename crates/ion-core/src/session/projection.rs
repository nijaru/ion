//! Replay and project committed Session records into request/display state.

use super::*;

pub(super) fn model_context_state(context: &ModelContextSnapshot) -> ModelContextState {
    ModelContextState {
        instructions: Some(context.instructions.clone()),
        tools: context.tools.clone(),
    }
}

pub(super) fn valid_model_context(context: &ModelContextSnapshot) -> bool {
    let mut names = BTreeSet::new();
    context
        .tools
        .iter()
        .all(|tool| !tool.name.trim().is_empty() && names.insert(tool.name.as_str()))
}

pub(super) fn shell_outcome_at(
    entries: &[SessionEntry],
    settlement_index: usize,
) -> &UserShellOutcome {
    match entries.get(settlement_index) {
        Some(SessionEntry::UserShellSettled { outcome, .. }) => outcome,
        _ => &UserShellOutcome::Unknown,
    }
}

pub(super) fn projected_entry_messages(
    entries: &[SessionEntry],
    display: bool,
) -> Vec<Option<Message>> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| match entry {
            SessionEntry::UserShellAdmitted {
                command,
                exclude_from_context,
            } if display || !exclude_from_context => Some(shell_message(
                command,
                shell_outcome_at(entries, index + 1),
                *exclude_from_context,
            )),
            SessionEntry::ToolResult { result, .. } if display => {
                Some(result.message(index as u64 + 1, true))
            }
            SessionEntry::Assistant { message, .. } if !display => Some(model_message(message)),
            other => message_from_entry(index as u64 + 1, other),
        })
        .collect()
}

// Human thinking is presentation evidence. Only the adapter-owned replay may
// carry it back to a provider; summaries and cross-provider context use answers.
pub(super) fn model_message(message: &Message) -> Message {
    Message {
        role: message.role,
        content: message
            .content
            .iter()
            .filter(|part| !matches!(part, Content::Thinking(_)))
            .cloned()
            .collect(),
        provider_replay: message.provider_replay.clone(),
    }
}

pub(super) fn message_from_entry(coordinate: u64, entry: &SessionEntry) -> Option<Message> {
    match entry {
        SessionEntry::TurnStarted { input, .. } => Some(input.clone()),
        SessionEntry::Steering { input, .. } => Some(input.clone()),
        SessionEntry::Completion { notices, .. } => {
            Some(crate::completion::notice_message(notices))
        }
        SessionEntry::Assistant { message, .. } => Some(message.clone()),
        SessionEntry::ToolResult { result, .. } => Some(result.message(coordinate, false)),
        SessionEntry::ChildToolAdmitted { .. }
        | SessionEntry::ChildToolResult { .. }
        | SessionEntry::DelegationAdmitted { .. }
        | SessionEntry::DelegationTransferred { .. }
        | SessionEntry::DelegationSettled { .. }
        | SessionEntry::ChildSessionOf { .. }
        | SessionEntry::UserShellAdmitted { .. }
        | SessionEntry::UserShellSettled { .. }
        | SessionEntry::ModelSelected { .. }
        | SessionEntry::ReasoningSelected { .. }
        | SessionEntry::EffectiveModelChanged { .. }
        | SessionEntry::ProviderReplayRebased { .. }
        | SessionEntry::ModelContextChanged { .. }
        | SessionEntry::TurnResumed { .. }
        | SessionEntry::ContextSelected { .. }
        | SessionEntry::CacheWarm { .. }
        | SessionEntry::Compacted { .. }
        | SessionEntry::TurnEnded { .. } => None,
    }
}

pub(super) fn shell_message(command: &str, outcome: &UserShellOutcome, excluded: bool) -> Message {
    let mut body = format!(
        "User {} shell command{}:\n$ {command}",
        match outcome {
            UserShellOutcome::Observed { .. } => "ran",
            UserShellOutcome::Unknown => "admitted",
        },
        if excluded {
            " (not shared with model)"
        } else {
            ""
        }
    );
    match outcome {
        UserShellOutcome::Observed { .. } => {
            // Host owns capture semantics. Preserve its entire observation,
            // rather than a Core whitelist that silently loses new metadata.
            body.push_str("\nObserved host result:\n");
            body.push_str(
                &serde_json::to_string(outcome)
                    .expect("a shell observation contains only serializable JSON"),
            );
        }
        UserShellOutcome::Unknown => {
            body.push('\n');
            body.push_str(UserShellOutcome::unknown_notice());
        }
    }
    Message {
        role: Role::User,
        content: vec![Content::Text(body)],
        provider_replay: None,
    }
}

#[derive(Clone, Copy)]
pub(super) enum ProjectionMode {
    Full,
    Metadata,
}

pub(super) fn project<E: Borrow<SessionEntry>>(
    entries: impl IntoIterator<Item = Result<E, SessionError>>,
    mode: ProjectionMode,
    read: &impl Fn(u64) -> Result<SessionEntry, SessionError>,
) -> Result<HistoryProjection, SessionError> {
    let mut state = State::default();
    let mut settled = BTreeSet::new();
    let mut messages = Vec::new();
    let mut context_changes = Vec::new();
    let mut child_records = ChildRecordIndex::default();
    let mut context_index = ContextIndex::default();
    let mut turns = 0usize;
    let mut request_keys = BTreeSet::new();
    let mut delegation_runs = BTreeSet::new();
    let mut created_delegations = BTreeSet::new();
    let mut binding_runs = BTreeSet::new();
    let mut created_children = BTreeSet::new();
    let mut first_input = None;
    for entry in entries {
        let entry = entry?;
        let entry = entry.borrow();
        if let SessionEntry::TurnStarted {
            request: Some(key), ..
        } = entry
            && !request_keys.insert(key.clone())
        {
            return Err(SessionError::InvalidHistory);
        }
        if let SessionEntry::DelegationAdmitted { record } = entry
            && (!delegation_runs.insert(record.run)
                || (!matches!(record.spec.target, crate::DelegationTarget::Resume { .. })
                    && !created_delegations.insert(record.child)))
        {
            return Err(SessionError::InvalidHistory);
        }
        if let SessionEntry::ChildSessionOf { record } = entry
            && (!binding_runs.insert(record.run)
                || (!matches!(record.spec.target, crate::DelegationTarget::Resume { .. })
                    && !created_children.insert(record.child)))
        {
            return Err(SessionError::InvalidHistory);
        }
        let mut new_settled = Vec::new();
        state.apply(entry, &mut messages, &settled, &mut new_settled)?;
        if let SessionEntry::ContextSelected { head_entry } = entry {
            let restored = restore_context(&context_index, *head_entry, mode, read)?;
            state.restore_context(restored.state);
            messages = restored.messages;
            settled = restored.settled;
            context_changes = restored.changes;
        }
        context_index.observe(entry);
        if matches!(mode, ProjectionMode::Full) {
            child_records.observe(state.sequence, entry);
        }
        if matches!(entry, SessionEntry::TurnStarted { .. }) {
            turns = turns.checked_add(1).ok_or(SessionError::InvalidHistory)?;
        }
        if first_input.is_none() {
            first_input = input_preview(entry);
        }
        match mode {
            ProjectionMode::Full => {
                if let SessionEntry::ModelContextChanged { context, .. } = entry {
                    context_changes.push(ModelContextChange {
                        after_message: messages.len(),
                        context: model_context_state(context),
                    });
                }
            }
            ProjectionMode::Metadata => {
                // apply needs only the final shared-shell slot for settlement.
                // No intervening message is legal while that shell is pending;
                // keep its slot until settlement and release all other payloads.
                if !state
                    .pending_shell
                    .as_ref()
                    .is_some_and(|shell| !shell.exclude_from_context)
                {
                    messages.clear();
                }
            }
        }
        settled.extend(new_settled);
    }
    Ok(HistoryProjection {
        turns,
        first_input,
        state,
        settled,
        messages,
        context_changes,
        child_records,
        context_index,
    })
}

pub(super) fn input_preview(entry: &SessionEntry) -> Option<SessionInputPreview> {
    let label = |text: &str| {
        text.lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect()
    };
    match entry {
        SessionEntry::TurnStarted { input, .. } => Some(SessionInputPreview::Prompt {
            text: input.content.iter().find_map(|part| match part {
                Content::Text(text) if !text.trim().is_empty() => Some(label(text)),
                _ => None,
            }),
        }),
        SessionEntry::UserShellAdmitted {
            command,
            exclude_from_context,
        } => Some(SessionInputPreview::Shell {
            command: label(command),
            excluded: *exclude_from_context,
        }),
        _ => None,
    }
}

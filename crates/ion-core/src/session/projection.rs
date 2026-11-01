//! Replay and project committed Session records into request/display state.

use super::*;

pub(super) fn append_message(
    messages: &mut Vec<ProjectedMessage>,
    coordinate: u64,
    entry: &SessionEntry,
    closed_shell: Option<&PendingShell>,
) {
    let message = match entry {
        SessionEntry::UserShellAdmitted {
            command,
            exclude_from_context: false,
        } => Some(shell_message(command, &UserShellOutcome::Unknown, false)),
        SessionEntry::UserShellSettled { outcome, .. } => {
            let shell = closed_shell.expect("validated shell settlement owns its admission");
            if !shell.exclude_from_context {
                messages
                    .last_mut()
                    .expect("shared shell owns the final projected message")
                    .message = shell_message(&shell.command, outcome, false);
            }
            None
        }
        SessionEntry::Assistant { message, .. } => Some(model_message(message)),
        _ => message_from_entry(coordinate, entry),
    };
    if let Some(message) = message {
        messages.push(ProjectedMessage::new(coordinate, message));
    }
}

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

pub(super) fn shell_outcome_at(entries: &[SessionEntry], admission: u64) -> &UserShellOutcome {
    let suffix = usize::try_from(admission)
        .ok()
        .and_then(|start| entries.get(start..))
        .unwrap_or_default();
    // Shared shells cannot overlap. Searching from each admission visits only
    // its own interval, including intervening metadata, not the whole journal.
    suffix
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::UserShellSettled {
                admission_entry,
                outcome,
            } if *admission_entry == admission => Some(outcome),
            _ => None,
        })
        .unwrap_or(&UserShellOutcome::Unknown)
}

#[derive(Clone, Copy)]
pub(super) enum MessageAudience {
    Model,
    Display,
}

pub(super) fn selected_messages(
    entries: &[SessionEntry],
    path: &[u64],
    audience: MessageAudience,
) -> Vec<Message> {
    let display = matches!(audience, MessageAudience::Display);
    path.iter()
        .filter_map(|&coordinate| match &entries[(coordinate - 1) as usize] {
            SessionEntry::UserShellAdmitted {
                command,
                exclude_from_context,
            } if display || !exclude_from_context => Some(shell_message(
                command,
                shell_outcome_at(entries, coordinate),
                *exclude_from_context,
            )),
            SessionEntry::ToolResult { result, .. } if display => {
                Some(result.message(coordinate, true))
            }
            SessionEntry::Assistant { message, .. } if !display => Some(model_message(message)),
            other => message_from_entry(coordinate, other),
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
        | SessionEntry::UncommittedUsage { .. }
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
    Working,
    Metadata,
}

pub(super) fn project<E: Borrow<SessionEntry>>(
    entries: impl IntoIterator<Item = Result<E, SessionError>>,
    mode: ProjectionMode,
    read: &impl Fn(u64) -> Result<SessionEntry, SessionError>,
) -> Result<HistoryProjection, SessionError> {
    let mut state = State::default();
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
        state.apply(entry, &|coordinate| context_index.is_settled(coordinate))?;
        if let SessionEntry::ContextSelected { head_entry } = entry {
            if !context_index.can_select(*head_entry) {
                return Err(SessionError::InvalidContextPoint);
            }
            state.restore_context(context_index.read_context(*head_entry, read)?);
        }
        context_index.observe(entry);
        if !matches!(mode, ProjectionMode::Metadata) {
            child_records.observe(state.sequence, entry);
        }
        if matches!(entry, SessionEntry::TurnStarted { .. }) {
            turns = turns.checked_add(1).ok_or(SessionError::InvalidHistory)?;
        }
        if first_input.is_none() {
            first_input = input_preview(entry);
        }
    }
    let (settled, messages, context_changes) = if matches!(mode, ProjectionMode::Metadata) {
        (BTreeSet::new(), Vec::new(), Vec::new())
    } else {
        let restored = working_context(&context_index, context_index.head(), &state, read)?;
        (restored.settled, restored.messages, restored.changes)
    };
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

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
        } => Some(shell_message(
            command,
            &UserShellOutcome::Unknown,
            false,
            coordinate,
        )),
        SessionEntry::UserShellSettled { outcome, .. } => {
            let shell = closed_shell.expect("validated shell settlement owns its admission");
            if !shell.exclude_from_context {
                *messages
                    .last_mut()
                    .expect("shared shell owns the final projected message") =
                    ProjectedMessage::new(
                        shell.admission_entry,
                        shell_message(&shell.command, outcome, false, shell.admission_entry),
                    );
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
                coordinate,
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

pub(super) fn shell_message(
    command: &str,
    outcome: &UserShellOutcome,
    excluded: bool,
    admission_entry: u64,
) -> Message {
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
        UserShellOutcome::Observed { output } => {
            // Preserve opaque host JSON and typed references, but never stringify
            // typed image bytes into model/history text.
            #[derive(Serialize)]
            struct SharedOutput<'a> {
                value: &'a serde_json::Value,
                images: &'a [ion_ai::ImageContent],
                artifacts: &'a [crate::ArtifactRef],
                is_error: bool,
            }
            #[derive(Serialize)]
            struct Observation<'a> {
                kind: &'static str,
                output: SharedOutput<'a>,
            }
            body.push_str("\nObserved host result:\n");
            body.push_str(
                &serde_json::to_string(&Observation {
                    kind: "observed",
                    output: SharedOutput {
                        value: &output.value,
                        images: &[],
                        artifacts: &output.artifacts,
                        is_error: output.is_error,
                    },
                })
                .expect("a shell observation contains serializable JSON"),
            );
            if !output.artifacts.is_empty() {
                body.push_str(&format!("\nRetained artifacts: history action artifact, entry {admission_entry}, zero-based artifact index. Pages contain data with encoding utf8 or base64 and raw-byte next_offset; do not rerun the command to recover them."));
            }
            if !output.images.is_empty() {
                body.push_str(&format!(
                    "\n{} typed images retained for inspection, omitted from shell text.",
                    output.images.len()
                ));
            }
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

/// Uniqueness constraints that cannot be recovered from just the active Turn
/// or the selected model context. Native SQLite also enforces them at insert.
#[derive(Default)]
pub(super) struct JournalKeys {
    requests: BTreeMap<RequestKey, (u64, u64)>,
    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "cloudflare-journal"
    ))]
    endings: BTreeMap<u64, TurnEndReason>,
    runs: BTreeSet<uuid::Uuid>,
    created_delegations: BTreeSet<uuid::Uuid>,
    bindings: BTreeSet<uuid::Uuid>,
    created_children: BTreeSet<uuid::Uuid>,
}

impl JournalKeys {
    fn observe(
        &mut self,
        committed: Option<&Self>,
        sequence: u64,
        entry: &SessionEntry,
    ) -> Result<(), SessionError> {
        fn insert<T: Ord>(
            pending: &mut BTreeSet<T>,
            committed: Option<&BTreeSet<T>>,
            key: T,
        ) -> Result<(), SessionError> {
            if committed.is_some_and(|keys| keys.contains(&key)) || !pending.insert(key) {
                return Err(SessionError::InvalidHistory);
            }
            Ok(())
        }
        match entry {
            SessionEntry::TurnStarted {
                turn,
                request: Some(key),
                ..
            } => {
                if committed.is_some_and(|keys| keys.requests.contains_key(key))
                    || self
                        .requests
                        .insert(key.clone(), (sequence, *turn))
                        .is_some()
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            #[cfg(all(
                target_arch = "wasm32",
                target_os = "unknown",
                feature = "cloudflare-journal"
            ))]
            SessionEntry::TurnEnded { turn, reason } => {
                self.endings.insert(*turn, reason.clone());
            }
            SessionEntry::DelegationAdmitted { record } => {
                insert(&mut self.runs, committed.map(|keys| &keys.runs), record.run)?;
                if !matches!(record.spec.target, crate::DelegationTarget::Resume { .. }) {
                    insert(
                        &mut self.created_delegations,
                        committed.map(|keys| &keys.created_delegations),
                        record.child,
                    )?;
                }
            }
            SessionEntry::ChildSessionOf { record } => {
                insert(
                    &mut self.bindings,
                    committed.map(|keys| &keys.bindings),
                    record.run,
                )?;
                if !matches!(record.spec.target, crate::DelegationTarget::Resume { .. }) {
                    insert(
                        &mut self.created_children,
                        committed.map(|keys| &keys.created_children),
                        record.child,
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "cloudflare-journal"
    ))]
    pub(super) fn extension(
        &self,
        mut sequence: u64,
        entries: &[SessionEntry],
    ) -> Result<Self, SessionError> {
        let mut pending = Self::default();
        for entry in entries {
            sequence = sequence
                .checked_add(1)
                .ok_or(SessionError::InvalidHistory)?;
            pending.observe(Some(self), sequence, entry)?;
        }
        Ok(pending)
    }

    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "cloudflare-journal"
    ))]
    pub(super) fn request(&self, key: &RequestKey) -> Option<(u64, u64, Option<TurnEndReason>)> {
        let &(sequence, turn) = self.requests.get(key)?;
        Some((sequence, turn, self.endings.get(&turn).cloned()))
    }

    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "cloudflare-journal"
    ))]
    pub(super) fn extend(&mut self, pending: Self) {
        self.requests.extend(pending.requests);
        self.endings.extend(pending.endings);
        self.runs.extend(pending.runs);
        self.created_delegations.extend(pending.created_delegations);
        self.bindings.extend(pending.bindings);
        self.created_children.extend(pending.created_children);
    }
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
    let mut keys = JournalKeys::default();
    let mut first_input = None;
    let mut latest_prompt = None;
    for entry in entries {
        let entry = entry?;
        let entry = entry.borrow();
        let transition = state.apply(entry, &|coordinate| context_index.is_settled(coordinate))?;
        keys.observe(None, state.sequence, entry)?;
        if let SessionEntry::ContextSelected { head_entry } = entry {
            if !context_index.can_select(*head_entry) {
                return Err(SessionError::InvalidContextPoint);
            }
            state.restore_context(context_index.read_context(*head_entry, read)?);
        }
        context_index.observe(entry);
        if !matches!(mode, ProjectionMode::Metadata) {
            context_index.link_display(entry, transition.resolved_tool);
            child_records.observe(state.sequence, entry);
        }
        if matches!(entry, SessionEntry::TurnStarted { .. }) {
            turns = turns.checked_add(1).ok_or(SessionError::InvalidHistory)?;
        }
        if matches!(mode, ProjectionMode::Metadata)
            && (first_input.is_none() || matches!(entry, SessionEntry::TurnStarted { .. }))
        {
            let preview = input_preview(entry);
            if let Some(SessionInputPreview::Prompt { text: Some(text) }) = &preview {
                latest_prompt = Some(text.clone());
            }
            if first_input.is_none() {
                first_input = preview;
            }
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
        latest_prompt,
        state,
        settled,
        messages,
        context_changes,
        child_records,
        context_index,
        #[cfg(all(
            target_arch = "wasm32",
            target_os = "unknown",
            feature = "cloudflare-journal"
        ))]
        keys,
    })
}

pub(super) fn input_preview(entry: &SessionEntry) -> Option<SessionInputPreview> {
    let label = |text: &str| {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
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

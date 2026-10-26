//! Durable context selection is separate from the ordered execution ledger.
use super::*;

/// Optimistic navigation token. Even an unrelated late fact invalidates a stale
/// preview; callers must inspect again rather than silently discard new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPosition {
    pub session: uuid::Uuid,
    pub revision: u64,
    pub head_entry: u64,
}

#[derive(Debug, Clone)]
pub struct ContextPoint {
    pub entry: u64,
    pub head_entry: u64,
    pub message: Message,
    pub restore_input: bool,
    pub selected_path: bool,
}

#[derive(Default)]
pub(super) struct ContextIndex {
    nodes: Vec<Node>,
    head: u64,
    pending: usize,
    shell: bool,
    admitted: BTreeMap<uuid::Uuid, crate::DelegationSite>,
    backgrounds: BTreeMap<uuid::Uuid, crate::DelegationSite>,
}

struct Node {
    parent: u64,
    selectable: bool,
    settled: bool,
}

impl ContextIndex {
    pub(super) fn head(&self) -> u64 {
        self.head
    }
    fn can_select(&self, head: u64) -> bool {
        head == 0
            || self
                .nodes
                .get((head - 1) as usize)
                .is_some_and(|node| node.selectable)
    }

    pub(super) fn path(&self, mut head: u64) -> Vec<u64> {
        let mut path = Vec::new();
        while head != 0 {
            path.push(head);
            head = self.nodes[(head - 1) as usize].parent;
        }
        path.reverse();
        path
    }

    // Called only after execution validation (and after durable commit for a
    // writer). Detached outcomes retain causal affinity without moving the head.
    pub(super) fn observe(&mut self, entry: &SessionEntry) {
        let coordinate = self.nodes.len() as u64 + 1;
        let mut detached = None;
        let mut settled = false;
        match entry {
            SessionEntry::Assistant { message, .. } => {
                self.pending = message
                    .content
                    .iter()
                    .filter(|part| matches!(part, Content::ToolCall(_)))
                    .count();
                settled = self.pending == 0;
            }
            SessionEntry::ToolResult { .. } => {
                self.pending -= 1;
                settled = self.pending == 0;
            }
            SessionEntry::UserShellAdmitted { .. } => self.shell = true,
            SessionEntry::UserShellSettled { .. } => {
                self.shell = false;
                settled = true;
            }
            SessionEntry::TurnEnded { .. } => settled = true,
            SessionEntry::DelegationAdmitted { record } => {
                self.admitted.insert(record.run, record.site);
            }
            SessionEntry::DelegationTransferred { run, .. } => {
                if let Some(site) = self.admitted.remove(run) {
                    self.backgrounds.insert(*run, site);
                }
            }
            SessionEntry::DelegationSettled { run, .. } => {
                detached = self
                    .backgrounds
                    .remove(run)
                    .map(|site| site.tool.assistant_entry);
                self.admitted.remove(run);
            }
            _ => {}
        }
        let selection = if let SessionEntry::ContextSelected { head_entry } = entry {
            Some(*head_entry)
        } else {
            None
        };
        self.nodes.push(Node {
            parent: detached.or(selection).unwrap_or(self.head),
            selectable: selection.is_none()
                && detached.is_none()
                && self.pending == 0
                && !self.shell,
            settled,
        });
        if let Some(head) = selection {
            self.head = head;
        } else if detached.is_none() {
            self.head = coordinate;
        }
    }
}

impl State {
    /// The only reducer of context preferences, used for live facts and replay of
    /// an already validated selected path. It never changes execution custody.
    pub(super) fn apply_context(&mut self, entry: &SessionEntry) {
        match entry {
            SessionEntry::ReasoningSelected { reasoning } => self.reasoning = *reasoning,
            SessionEntry::ModelSelected { model } | SessionEntry::TurnStarted { model, .. } => {
                self.select_model(model)
            }
            SessionEntry::EffectiveModelChanged { model, .. } => self.select_effective_model(model),
            SessionEntry::ProviderReplayRebased { .. } => {
                self.replay_epoch_start = self.sequence + 1
            }
            SessionEntry::ModelContextChanged { context, .. } => {
                self.last_context = Some(context.clone())
            }
            SessionEntry::Compacted {
                through_entry,
                summary,
                ..
            } => self.compaction = Some((*through_entry, summary.clone())),
            SessionEntry::Assistant {
                execution, usage, ..
            } => {
                if self.last_effective_model.is_none() {
                    self.last_effective_model = Some(execution.route.effective.clone());
                }
                self.last_execution = Some(execution.clone());
                self.last_usage = Some(*usage);
            }
            _ => {}
        }
    }

    pub(super) fn restore_context(&mut self, source: State) {
        self.last_model = source.last_model;
        self.reasoning = source.reasoning;
        self.last_effective_model = source.last_effective_model;
        self.last_context = source.last_context;
        self.compaction = source.compaction;
        self.last_execution = source.last_execution;
        self.last_usage = source.last_usage;
        // Navigation never revives opaque continuations or calibration from an
        // abandoned request, even when returning to an earlier route/path.
        self.replay_epoch_start = self.sequence + 1;
    }
}

pub(super) struct RestoredContext {
    pub(super) state: State,
    pub(super) messages: Vec<ProjectedMessage>,
    pub(super) settled: BTreeSet<u64>,
    pub(super) changes: Vec<ModelContextChange>,
}

pub(super) fn restore_context(
    index: &ContextIndex,
    head: u64,
    mode: ProjectionMode,
    read: &impl Fn(u64) -> Result<SessionEntry, SessionError>,
) -> Result<RestoredContext, SessionError> {
    if !index.can_select(head) {
        return Err(SessionError::InvalidContextPoint);
    }
    let mut restored = RestoredContext {
        state: State::default(),
        messages: Vec::new(),
        settled: BTreeSet::new(),
        changes: Vec::new(),
    };
    for coordinate in index.path(head) {
        let entry = read(coordinate)?;
        restored.state.sequence = coordinate - 1;
        restored.state.apply_context(&entry);
        if index.nodes[(coordinate - 1) as usize].settled {
            restored.settled.insert(coordinate);
        }
        if matches!(mode, ProjectionMode::Full) {
            match &entry {
                SessionEntry::UserShellAdmitted {
                    command,
                    exclude_from_context,
                } => {
                    restored.state.pending_shell = Some(PendingShell {
                        admission_entry: coordinate,
                        command: command.clone(),
                        exclude_from_context: *exclude_from_context,
                    });
                    if !exclude_from_context {
                        restored.messages.push(ProjectedMessage::new(
                            coordinate,
                            shell_message(command, &UserShellOutcome::Unknown, false),
                        ));
                    }
                }
                SessionEntry::UserShellSettled { outcome, .. } => {
                    let shell = restored
                        .state
                        .pending_shell
                        .take()
                        .ok_or(SessionError::InvalidHistory)?;
                    if !shell.exclude_from_context {
                        restored
                            .messages
                            .last_mut()
                            .ok_or(SessionError::InvalidHistory)?
                            .message = shell_message(&shell.command, outcome, false);
                    }
                }
                SessionEntry::Assistant { message, .. } => restored
                    .messages
                    .push(ProjectedMessage::new(coordinate, model_message(message))),
                _ => {
                    if let Some(message) = message_from_entry(coordinate, &entry) {
                        restored
                            .messages
                            .push(ProjectedMessage::new(coordinate, message));
                    }
                }
            }
            if let SessionEntry::ModelContextChanged { context, .. } = &entry {
                restored.changes.push(ModelContextChange {
                    after_message: restored.messages.len(),
                    context: model_context_state(context),
                });
            }
        }
    }
    Ok(restored)
}

/// Holds the engine operation gate across host preflight and durable publication.
/// Dropping an uncommitted navigation leaves history and selection untouched.
pub struct PreparedNavigation<'a> {
    session: &'a Session,
    _idle: IdleSession<'a>,
    expected: ContextPosition,
    head: u64,
    model: Option<ModelRef>,
    reasoning: Reasoning,
}

impl PreparedNavigation<'_> {
    pub fn model(&self) -> Option<&ModelRef> {
        self.model.as_ref()
    }
    pub fn reasoning(&self) -> Reasoning {
        self.reasoning
    }
    /// Retain this guard until the caller has published its prepared binding.
    pub fn commit(&mut self) -> Result<ContextPosition, SessionError> {
        let mut store = self
            .session
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?;
        self.session.check_position(&store, self.expected)?;
        let mut entries = interrupted_operation_entries(&store, self.session.id());
        entries.push(SessionEntry::ContextSelected {
            head_entry: self.head,
        });
        append(&mut store, &entries)?;
        Ok(self.session.position(&store))
    }
}

impl Session {
    fn position(&self, store: &Store) -> ContextPosition {
        ContextPosition {
            session: self.id(),
            revision: store.state.sequence,
            head_entry: store.context_index.head,
        }
    }
    fn check_position(&self, store: &Store, expected: ContextPosition) -> Result<(), SessionError> {
        if self.position(store) != expected {
            return Err(SessionError::StaleContextPosition);
        }
        Ok(())
    }
    pub fn context_position(&self) -> Result<ContextPosition, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(self.position(&store))
    }
    pub fn prepare_navigation(
        &self,
        expected: ContextPosition,
        head_entry: u64,
    ) -> Result<PreparedNavigation<'_>, SessionError> {
        let idle = self.reserve_idle()?;
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        self.check_position(&store, expected)?;
        let context = restore_context(
            &store.context_index,
            head_entry,
            ProjectionMode::Metadata,
            &|entry| read_entry(&store.connection, entry),
        )?;
        Ok(PreparedNavigation {
            session: self,
            _idle: idle,
            expected,
            head: head_entry,
            model: context.state.last_model,
            reasoning: context.state.reasoning,
        })
    }
}

impl SessionView {
    pub fn context_position(&self) -> ContextPosition {
        ContextPosition {
            session: self.id,
            revision: self.entries.len() as u64,
            head_entry: self.context_path.last().copied().unwrap_or(0),
        }
    }
    /// User points restore input from before its admission. Assistant points are
    /// offered only after a complete call/result group, never mid-tool dispatch.
    pub fn context_points(&self) -> Vec<ContextPoint> {
        let mut index = ContextIndex::default();
        let selected: BTreeSet<_> = self.context_path.iter().copied().collect();
        let mut points = Vec::new();
        for (offset, entry) in self.entries.iter().enumerate() {
            index.observe(entry);
            let coordinate = offset as u64 + 1;
            let tool_message;
            let (message, restore_input) = match entry {
                SessionEntry::TurnStarted {
                    input, completion, ..
                } => (input, completion.is_empty()),
                SessionEntry::Steering { input, .. } => (input, true),
                SessionEntry::Assistant { message, .. } if index.can_select(coordinate) => {
                    (message, false)
                }
                SessionEntry::ToolResult { result, .. } if index.can_select(coordinate) => {
                    tool_message = result.message(coordinate, true);
                    (&tool_message, false)
                }
                _ => continue,
            };
            points.push(ContextPoint {
                entry: coordinate,
                head_entry: if restore_input {
                    index.nodes[offset].parent
                } else {
                    coordinate
                },
                message: message.clone(),
                restore_input,
                selected_path: selected.contains(&coordinate),
            });
        }
        points
    }
}

pub(super) fn read_entry(
    connection: &Connection,
    entry: u64,
) -> Result<SessionEntry, SessionError> {
    let mut statement = connection.prepare_cached("SELECT body FROM entries WHERE seq = ?1")?;
    let body: Vec<u8> = statement.query_row(
        params![i64::try_from(entry).map_err(|_| SessionError::InvalidContextPoint)?],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_slice(&body)?)
}

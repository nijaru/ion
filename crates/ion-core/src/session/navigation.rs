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

impl State {
    /// The only reducer of context preferences, used for live facts and replay of
    /// an already validated selected path. It never changes execution custody.
    pub(super) fn apply_context(&mut self, entry: &SessionEntry) {
        match entry {
            SessionEntry::ReasoningSelected { reasoning } => self.reasoning = *reasoning,
            SessionEntry::ModelSelected { model } => self.select_model(model),
            SessionEntry::TurnStarted { model, .. } => {
                self.select_model(model);
                self.active_inputs.clear();
                self.active_inputs.push(self.sequence + 1);
            }
            SessionEntry::Steering { .. } => self.active_inputs.push(self.sequence + 1),
            SessionEntry::TurnEnded { .. } => self.active_inputs.clear(),
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
                retained_inputs,
                ..
            } => {
                self.compaction = Some((*through_entry, summary.clone()));
                self.compaction_inputs = retained_inputs.clone();
            }
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
        self.compaction_inputs = source.compaction_inputs;
        self.last_execution = source.last_execution;
        self.last_usage = source.last_usage;
        // Navigation never revives opaque continuations or calibration from an
        // abandoned request, even when returning to an earlier route/path.
        self.replay_epoch_start = self.sequence + 1;
    }
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
        let mut store = self.session.lock_store()?;
        self.session.check_position(&store, self.expected)?;
        let mut entries = interrupted_operation_entries(&store, self.session.id());
        entries.push(SessionEntry::ContextSelected {
            head_entry: self.head,
        });
        store.append(&entries)?;
        Ok(self.session.position(&store))
    }
}

impl Session {
    fn position(&self, store: &Store) -> ContextPosition {
        ContextPosition {
            session: self.id(),
            revision: store.state.sequence,
            head_entry: store.context_index.head(),
        }
    }
    fn check_position(&self, store: &Store, expected: ContextPosition) -> Result<(), SessionError> {
        if self.position(store) != expected {
            return Err(SessionError::StaleContextPosition);
        }
        Ok(())
    }
    pub fn context_position(&self) -> Result<ContextPosition, SessionError> {
        let store = self.lock_store()?;
        Ok(self.position(&store))
    }
    pub fn prepare_navigation(
        &self,
        expected: ContextPosition,
        head_entry: u64,
    ) -> Result<PreparedNavigation<'_>, SessionError> {
        let idle = self.reserve_idle()?;
        let store = self.lock_store()?;
        self.check_position(&store, expected)?;
        if !store.context_index.can_select(head_entry) {
            return Err(SessionError::InvalidContextPoint);
        }
        let context = store
            .context_index
            .read_context(head_entry, &|entry| read_entry(&store.connection, entry))?;
        Ok(PreparedNavigation {
            session: self,
            _idle: idle,
            expected,
            head: head_entry,
            model: context.last_model,
            reasoning: context.reasoning,
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
                    index.parent(coordinate)
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

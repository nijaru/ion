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
            SessionEntry::Steering { .. } | SessionEntry::Completion { .. } => {
                self.active_inputs.push(self.sequence + 1)
            }
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PointKind {
    RestoreInput,
    AutomatedInput,
    Assistant,
    ToolResult,
}

pub(super) struct PointLocation {
    pub(super) entry: u64,
    pub(super) head_entry: u64,
    pub(super) kind: PointKind,
    pub(super) selected_path: bool,
}

impl PointLocation {
    fn read(self, snapshot: &Connection) -> Result<ContextPoint, SessionError> {
        let message = match (self.kind, read_entry(snapshot, self.entry)?) {
            (
                PointKind::RestoreInput,
                SessionEntry::TurnStarted {
                    input, completion, ..
                },
            ) if completion.is_empty() => input,
            (PointKind::RestoreInput, SessionEntry::Steering { input, .. }) => input,
            (
                PointKind::AutomatedInput,
                SessionEntry::TurnStarted {
                    input, completion, ..
                },
            ) if !completion.is_empty() => input,
            (PointKind::Assistant, SessionEntry::Assistant { message, .. }) => message,
            (PointKind::ToolResult, SessionEntry::ToolResult { result, .. }) => {
                result.message(self.entry, true)
            }
            _ => return Err(SessionError::InvalidHistory),
        };
        Ok(ContextPoint {
            entry: self.entry,
            head_entry: self.head_entry,
            message,
            restore_input: self.kind == PointKind::RestoreInput,
            selected_path: self.selected_path,
        })
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
        let mut entries = store.working.interrupted_operation_entries();
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
            revision: store.working.state.sequence,
            head_entry: store.working.context_index.head(),
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
    /// Visit navigation boundaries in journal order, across all alternatives,
    /// or read just one requested entry. The position and each point belong to
    /// one committed cut. The visitor runs outside Store locks and receives one
    /// owned point at a time. Project data only; the file-backed walk holds a
    /// read transaction until it finishes. An ineligible target produces no point.
    pub fn visit_context_points(
        &self,
        entry: Option<u64>,
        mut visit: impl FnMut(ContextPosition, ContextPoint),
    ) -> Result<ContextPosition, SessionError> {
        if self.journal().file.is_none() {
            // The in-memory journal can't hold an independent read cut. Own
            // the results before releasing the Store and calling host code.
            let (position, points) = {
                let store = self.lock_store()?;
                let points = store
                    .working
                    .context_index
                    .navigation_locations(entry)
                    .into_iter()
                    .map(|location| location.read(&store.connection))
                    .collect::<Result<Vec<_>, _>>()?;
                (self.position(&store), points)
            };
            for point in points {
                visit(position, point);
            }
            return Ok(position);
        }

        let connection = self.open_file_reader()?;
        let (snapshot, position, locations) = {
            let store = self.lock_store()?;
            let position = self.position(&store);
            let snapshot = connection.unchecked_transaction()?;
            self.check_read_snapshot(&snapshot, position.revision)?;
            (
                snapshot,
                position,
                store.working.context_index.navigation_locations(entry),
            )
        };
        for location in locations {
            visit(position, location.read(&snapshot)?);
        }
        Ok(position)
    }

    pub fn prepare_navigation(
        &self,
        expected: ContextPosition,
        head_entry: u64,
    ) -> Result<PreparedNavigation<'_>, SessionError> {
        let idle = self.reserve_idle()?;
        let store = self.lock_store()?;
        self.check_position(&store, expected)?;
        if !store.working.context_index.can_select(head_entry) {
            return Err(SessionError::InvalidContextPoint);
        }
        let context = store
            .working
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
}

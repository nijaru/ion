//! The single durable authority for a Session and its execution facts.
//! SQLite commits related events atomically before any external effect runs.
use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::operation::{IdleSession, Operation, OperationError, OperationFailure, OperationOwner};
use crate::tool_set::ToolActivity;
#[cfg(test)]
use ion_ai::ToolResult;
use ion_ai::{
    Content, IncompleteReason, Message, ModelContextChange, ModelContextState,
    ModelContextTimeline, ModelExecution, ModelRef, Reasoning, ResponseTermination, Role, ToolCall,
    ToolSpec, Usage,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub(crate) mod code_inspection;
mod context;
mod navigation;
#[cfg(test)]
mod navigation_tests;
use navigation::{ContextIndex, restore_context};
pub use navigation::{ContextPoint, ContextPosition, PreparedNavigation};
mod delegation;
#[cfg(test)]
mod delegation_background_tests;
#[cfg(test)]
mod delegation_fork_tests;
#[cfg(test)]
mod delegation_resume_tests;
#[cfg(test)]
mod delegation_tests;
pub(crate) mod history;
mod records;
mod recovery;
#[cfg(test)]
mod recovery_tests;
mod submission;
#[cfg(test)]
mod submission_tests;
use crate::request::{RequestCalibration, RequestMeasurement, RequestScope};
use code_inspection::ChildRecordIndex;
use context::{ProjectedMessage, context_projection};
pub use records::{
    ForkPoint, ModelContextSnapshot, SessionEntry, SessionStatus, SessionView, StoredToolActivity,
    TurnEndReason, TurnSummary, UnobservedUserShell, UserShellOutcome,
};
pub use recovery::{AuthorityScope, RecoveryBinding, RecoveryPoint, RecoveryState};
pub(crate) use submission::TurnAdmission;
pub use submission::{RequestKey, RequestReceipt, SubmissionRequest};

const FORMAT_VERSION: u32 = 18;
// An 8 MiB raw prompt or streamed response can grow up to sixfold when JSON
// escapes control characters. Keep the storage bound above that encoded size.
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

fn completed_termination() -> ResponseTermination {
    ResponseTermination::Completed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    id: uuid::Uuid,
    provider_session_id: uuid::Uuid,
    cwd: Option<PathBuf>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct CommittedInputEntries {
    pub steering: std::ops::Range<u64>,
    pub completion: Option<u64>,
}

/// A single committed projection; catalogue and service preparation must happen
/// after releasing the store lock.
pub(crate) struct RequestBasis {
    pub turn: Option<u64>,
    pub messages: Vec<Message>,
    pub previous: Option<ModelContextSnapshot>,
    pub reasoning: Reasoning,
    pub assistant_seen: bool,
    pub prefix_bound: bool,
    context_changes: Option<Vec<ModelContextChange>>,
}

impl RequestBasis {
    pub fn timeline(&self, current: &ModelContextSnapshot) -> Option<ModelContextTimeline> {
        let changes = self.context_changes.as_ref()?;
        let current = model_context_state(current);
        let Some((first, rest)) = changes.split_first() else {
            return Some(ModelContextTimeline {
                initial: current,
                changes: Vec::new(),
            });
        };
        let mut changes = rest.to_vec();
        if changes.last().unwrap_or(first).context != current {
            changes.push(ModelContextChange {
                after_message: self.messages.len(),
                context: current,
            });
        }
        Some(ModelContextTimeline {
            initial: first.context.clone(),
            changes,
        })
    }
}

#[derive(Debug)]
pub(crate) struct CommittedAssistant {
    pub entry: u64,
    pub complete: bool,
}

impl SessionView {
    /// Conversation display, including user commands kept out of model context.
    pub fn display_messages(&self) -> Vec<Message> {
        let selected: BTreeSet<_> = self.context_path.iter().copied().collect();
        projected_entry_messages(&self.entries, true)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| selected.contains(&(*index as u64 + 1)))
            .filter_map(|(_, message)| message)
            .collect()
    }

    /// Missing settlement is passively projected as unknown, never repaired here.
    pub fn user_shell_outcome(&self, admission_entry: u64) -> &UserShellOutcome {
        shell_outcome_at(&self.entries, admission_entry as usize)
    }

    pub fn turns(&self) -> Vec<TurnSummary> {
        let mut turns: Vec<TurnSummary> = Vec::new();
        let mut reasoning = Reasoning::ProviderDefault;
        let mut reasoning_at = vec![reasoning];
        for entry in &self.entries {
            match entry {
                SessionEntry::ContextSelected { head_entry } => {
                    reasoning = reasoning_at[*head_entry as usize]
                }
                SessionEntry::ReasoningSelected {
                    reasoning: selected,
                } => reasoning = *selected,
                SessionEntry::TurnStarted {
                    turn, input, model, ..
                } => turns.push(TurnSummary {
                    turn: *turn,
                    reasoning,
                    input: input.clone(),
                    model: model.clone(),
                    end: None,
                }),
                SessionEntry::TurnEnded { turn, reason } => {
                    if let Some(last) = turns.last_mut()
                        && last.turn == *turn
                    {
                        last.end = Some(reason.clone());
                    }
                }
                _ => {}
            }
            reasoning_at.push(reasoning);
        }
        turns
    }
}

pub(crate) struct CompactionPlan {
    pub through_entry: u64,
    pub messages: Vec<Message>,
    pub chunked: bool,
    pub smaller_through_entry: Option<u64>,
}

#[derive(Clone)]
struct PendingTool {
    occurrence: crate::ToolOccurrence,
    call_id: String,
    name: String,
    activity: ToolActivity,
    next_child: usize,
    children: BTreeSet<usize>,
    delegations: delegation::PendingDelegations,
}

#[derive(Clone)]
struct PendingShell {
    admission_entry: u64,
    command: String,
    exclude_from_context: bool,
}

#[derive(Default, Clone)]
struct State {
    active: Option<u64>,
    pending_shell: Option<PendingShell>,
    assistant_seen_in_turn: bool,
    prefix_bound_in_turn: bool,
    recovery: Option<RecoveryBinding>,
    pending: Vec<PendingTool>,
    backgrounds: BTreeMap<uuid::Uuid, delegation::DelegationLink>,
    last_id: u64,
    last_end: Option<(u64, TurnEndReason)>,
    last_model: Option<ModelRef>,
    reasoning: Reasoning,
    last_effective_model: Option<ModelRef>,
    last_context: Option<ModelContextSnapshot>,
    // Derived from effective-model transitions/rebases; older opaque replay stays
    // in raw history and is never revived by switching a route back.
    replay_epoch_start: u64,
    sequence: u64,
    compaction: Option<(u64, String)>,
    last_execution: Option<ModelExecution>,
    last_usage: Option<Usage>,
}

impl State {
    fn select_model(&mut self, model: &ModelRef) {
        self.last_model = Some(model.clone());
    }

    fn select_effective_model(&mut self, model: &ModelRef) {
        if self
            .last_effective_model
            .as_ref()
            .is_some_and(|prior| prior != model)
        {
            self.replay_epoch_start = self.sequence + 1;
        }
        self.last_effective_model = Some(model.clone());
    }

    fn apply(
        &mut self,
        entry: &SessionEntry,
        messages: &mut Vec<ProjectedMessage>,
        settled: &BTreeSet<u64>,
        new_settled: &mut Vec<u64>,
    ) -> Result<(), SessionError> {
        match entry {
            SessionEntry::ContextSelected { .. } => {
                if self.active.is_some() || self.pending_shell.is_some() || !self.pending.is_empty()
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            // Selection owns only future-request preferences. The operation gate
            // excludes live execution; unfinished history can await recovery.
            SessionEntry::ReasoningSelected { reasoning } => {
                reasoning
                    .validate()
                    .map_err(|_| SessionError::InvalidHistory)?;
            }
            SessionEntry::ModelSelected { .. } => {}
            SessionEntry::EffectiveModelChanged { turn, model } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || self.last_effective_model.as_ref() == Some(model)
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::ProviderReplayRebased { turn } => {
                if self.active != Some(*turn)
                    || self.assistant_seen_in_turn
                    || !self.pending.is_empty()
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::ModelContextChanged { turn, context } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || self.last_context.as_ref() == Some(context)
                    || !valid_model_context(context)
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::UserShellAdmitted {
                command,
                exclude_from_context,
            } => {
                if self.active.is_some()
                    || self.pending_shell.is_some()
                    || command.trim().is_empty()
                    || command.contains('\0')
                {
                    return Err(SessionError::InvalidHistory);
                }
                if !exclude_from_context {
                    messages.push(ProjectedMessage::new(
                        self.sequence + 1,
                        shell_message(command, &UserShellOutcome::Unknown, false),
                    ));
                }
                self.pending_shell = Some(PendingShell {
                    admission_entry: self.sequence + 1,
                    command: command.clone(),
                    exclude_from_context: *exclude_from_context,
                });
            }
            SessionEntry::UserShellSettled {
                admission_entry,
                outcome,
            } => {
                let shell = self
                    .pending_shell
                    .as_ref()
                    .ok_or(SessionError::InvalidHistory)?;
                if self.active.is_some() || shell.admission_entry != *admission_entry {
                    return Err(SessionError::InvalidHistory);
                }
                if !shell.exclude_from_context {
                    // No intervening message is legal while a shell is pending.
                    messages
                        .last_mut()
                        .ok_or(SessionError::InvalidHistory)?
                        .message = shell_message(&shell.command, outcome, false);
                }
                self.pending_shell = None;
                new_settled.push(self.sequence + 1);
            }
            SessionEntry::CacheWarm {
                turn, execution, ..
            } => {
                if self.active != Some(*turn)
                    || self.last_model.as_ref() != Some(&execution.route.logical)
                    || self.last_effective_model.as_ref() != Some(&execution.route.effective)
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::Compacted {
                through_entry,
                summary,
                ..
            } => {
                if !self.pending.is_empty()
                    || self.pending_shell.is_some()
                    || summary.trim().is_empty()
                    || !(settled.contains(through_entry) || new_settled.contains(through_entry))
                    || self
                        .compaction
                        .as_ref()
                        .is_some_and(|(previous, _)| through_entry <= previous)
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::TurnResumed { turn, checkpoint } => {
                if self.active != Some(*turn)
                    || self.recovery.is_none()
                    || *checkpoint != self.sequence
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::TurnStarted {
                turn,
                input,
                recovery,
                completion,
                ..
            } => {
                if (!completion.is_empty()
                    && *input != crate::completion::notice_message(completion))
                    || self.active.is_some()
                    || self.pending_shell.is_some()
                    || *turn != self.last_id.saturating_add(1)
                    || !valid_user_message(input)
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.active = Some(*turn);
                self.assistant_seen_in_turn = false;
                self.prefix_bound_in_turn = false;
                self.recovery = recovery.clone();
                self.last_id = *turn;
                messages.push(ProjectedMessage::new(self.sequence + 1, input.clone()));
            }
            SessionEntry::Completion { turn, notices } => {
                if self.active != Some(*turn) || !self.pending.is_empty() || notices.is_empty() {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(ProjectedMessage::new(
                    self.sequence + 1,
                    crate::completion::notice_message(notices),
                ));
            }
            SessionEntry::Steering { turn, input } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || !valid_user_message(input)
                {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(ProjectedMessage::new(self.sequence + 1, input.clone()));
            }
            SessionEntry::Assistant {
                turn,
                message,
                tool_activities,
                execution,
                termination,
                ..
            } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || message.role != Role::Assistant
                    || self.last_model.as_ref() != Some(&execution.route.logical)
                    || self
                        .last_effective_model
                        .as_ref()
                        .is_some_and(|model| model != &execution.route.effective)
                {
                    return Err(SessionError::InvalidHistory);
                }
                if matches!(termination, ResponseTermination::Incomplete(_))
                    && (!matches!(
                        termination,
                        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
                    ) || !message
                        .content
                        .iter()
                        .any(|part| matches!(part, Content::ToolCall(_))))
                {
                    return Err(SessionError::InvalidHistory);
                }
                let mut activity_index = 0usize;
                for part in &message.content {
                    match part {
                        Content::ToolCall(ToolCall { id, name, .. })
                            if !id.is_empty() && !name.is_empty() =>
                        {
                            if self.pending.iter().any(|pending| pending.call_id == *id) {
                                return Err(SessionError::InvalidHistory);
                            }
                            let Some(stored) = tool_activities.get(activity_index) else {
                                return Err(SessionError::InvalidHistory);
                            };
                            if stored.call_id != *id {
                                return Err(SessionError::InvalidHistory);
                            }
                            self.pending.push(PendingTool {
                                occurrence: crate::ToolOccurrence {
                                    assistant_entry: self.sequence + 1,
                                    ordinal: activity_index,
                                },
                                call_id: id.clone(),
                                name: name.clone(),
                                activity: stored.activity.clone(),
                                next_child: 0,
                                children: BTreeSet::new(),
                                delegations: delegation::PendingDelegations::default(),
                            });
                            activity_index += 1;
                        }
                        Content::Text(_) | Content::Thinking(_) => {}
                        _ => return Err(SessionError::InvalidHistory),
                    }
                }
                if activity_index != tool_activities.len() {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(ProjectedMessage::new(
                    self.sequence + 1,
                    model_message(message),
                ));
                self.assistant_seen_in_turn = true;
                self.prefix_bound_in_turn |= message
                    .provider_replay
                    .as_ref()
                    .is_some_and(|replay| replay.prefix_bound);
                if self.pending.is_empty() {
                    new_settled.push(self.sequence + 1);
                }
            }
            SessionEntry::ToolResult { turn, result } => {
                let position = self.pending.iter().position(|pending| {
                    pending.call_id == result.call_id
                        && pending.name == result.name
                        && pending.children.is_empty()
                        && pending.delegations.empty()
                });
                if self.active != Some(*turn) || position.is_none() {
                    return Err(SessionError::InvalidHistory);
                }
                let position = position.expect("checked above");
                if matches!(result.outcome, crate::ToolOutcome::NotDispatched { .. })
                    && (self.pending[position].next_child != 0
                        || self.pending[position].delegations.ever_admitted())
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.pending.remove(position);
                messages.push(ProjectedMessage::new(
                    self.sequence + 1,
                    result.message(self.sequence + 1, false),
                ));
                if self.pending.is_empty() {
                    new_settled.push(self.sequence + 1);
                }
            }
            SessionEntry::ChildToolAdmitted { turn, intent } => {
                let parent = self
                    .pending
                    .iter_mut()
                    .find(|pending| pending.occurrence == intent.parent)
                    .ok_or(SessionError::InvalidHistory)?;
                if self.active != Some(*turn)
                    || intent.child != parent.next_child
                    || intent.call.name.is_empty()
                    || intent.call.name != intent.definition.name
                    || intent.call.id.is_empty()
                    || !intent.call.arguments.is_object()
                    || intent.call.raw_arguments.is_some()
                {
                    return Err(SessionError::InvalidHistory);
                }
                parent.next_child = parent
                    .next_child
                    .checked_add(1)
                    .ok_or(SessionError::InvalidHistory)?;
                parent.children.insert(intent.child);
            }
            SessionEntry::ChildToolResult {
                turn,
                parent,
                child,
                ..
            } => {
                let pending = self
                    .pending
                    .iter_mut()
                    .find(|pending| pending.occurrence == *parent)
                    .ok_or(SessionError::InvalidHistory)?;
                if self.active != Some(*turn)
                    || pending.delegations.unresolved(Some(*child))
                    || !pending.children.remove(child)
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::DelegationAdmitted { .. }
            | SessionEntry::DelegationTransferred { .. }
            | SessionEntry::DelegationSettled { .. }
            | SessionEntry::ChildSessionOf { .. } => self.apply_delegation(entry)?,
            SessionEntry::TurnEnded { turn, reason } => {
                if self.active != Some(*turn) || !self.pending.is_empty() {
                    return Err(SessionError::InvalidHistory);
                }
                self.active = None;
                self.recovery = None;
                self.assistant_seen_in_turn = false;
                self.prefix_bound_in_turn = false;
                self.last_end = Some((*turn, reason.clone()));
                new_settled.push(self.sequence + 1);
            }
        }
        self.apply_context(entry);
        self.sequence += 1;
        Ok(())
    }
}

/// Passive, bounded input labels and preferences, not a raw-history export.
#[derive(Debug, PartialEq, Eq)]
pub struct SessionMetadata {
    pub id: uuid::Uuid,
    pub cwd: Option<PathBuf>,
    pub name: Option<String>,
    pub last_model: Option<ModelRef>,
    pub turns: usize,
    pub first_input: Option<SessionInputPreview>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SessionInputPreview {
    Prompt { text: Option<String> },
    Shell { command: String, excluded: bool },
}

struct HistoryProjection {
    turns: usize,
    first_input: Option<SessionInputPreview>,
    state: State,
    settled: BTreeSet<u64>,
    messages: Vec<ProjectedMessage>,
    context_changes: Vec<ModelContextChange>,
    child_records: ChildRecordIndex,
    context_index: ContextIndex,
}

struct Store {
    connection: Connection,
    state: State,
    settled: BTreeSet<u64>,
    messages: Vec<ProjectedMessage>,
    // Sparse, rebuildable projection of committed context boundaries. It does
    // not retain or reproject historical tool payloads on each model request.
    context_changes: Vec<ModelContextChange>,
    child_records: ChildRecordIndex,
    context_index: ContextIndex,
    // Process-local capacity evidence. Reopen, fork and clone start cold; this
    // is never serialized into Session history.
    calibration: Option<RequestCalibration>,
    // Process-local task custody only; absent after reopen. Not replay grants.
    live_backgrounds: BTreeMap<uuid::Uuid, std::sync::Weak<()>>,
}

/// A writable Session holds a cross-process lock and retains its exclusive
/// operation through started effects, observed-result commit and task joining.
pub struct Session {
    // Field order closes SQLite before releasing the writer lock.
    store: Mutex<Store>,
    _lock: SessionLock,
    header: Header,
    path: PathBuf,
    pub(crate) operations: Arc<OperationOwner>,
}

struct SessionLock(File);

impl Drop for SessionLock {
    fn drop(&mut self) {
        // A child may briefly inherit a duplicate file description. Release
        // this writer's lease explicitly before closing our descriptor.
        let _ = flock(&self.0, FlockOperation::Unlock);
    }
}

impl Session {
    pub fn create(path: impl AsRef<Path>, cwd: Option<&Path>) -> Result<Self, SessionError> {
        Self::create_initial(path.as_ref(), cwd, uuid::Uuid::new_v4(), &[])
    }

    fn create_initial(
        path: &Path,
        cwd: Option<&Path>,
        id: uuid::Uuid,
        entries: &[SessionEntry],
    ) -> Result<Self, SessionError> {
        let projection = project(entries.iter().map(Ok), ProjectionMode::Full, &|entry| {
            Ok(entries[(entry - 1) as usize].clone())
        })?;
        let encoded = entries
            .iter()
            .map(encode_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let cwd = cwd.map(Path::canonicalize).transpose()?;
        if cwd.as_ref().is_some_and(|cwd| !cwd.is_dir()) {
            return Err(SessionError::InvalidWorkingDirectory);
        }
        fs::create_dir_all(path.parent().ok_or(SessionError::InvalidPath)?)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let path = database_path(path)?;
        let lock = lock(&path)?;
        let mut connection = Connection::open(&path)?;
        initialize_new(&connection)?;
        let header = Header {
            version: FORMAT_VERSION,
            // Random prefixes remain useful as catalog selectors and short UI labels.
            id,
            provider_session_id: uuid::Uuid::now_v7(),
            cwd,
            name: None,
        };
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO session(id, header) VALUES (1, ?1)",
            params![serde_json::to_vec(&header)?],
        )?;
        for body in encoded {
            tx.execute("INSERT INTO entries(body) VALUES (?1)", params![body])?;
        }
        tx.commit()?;
        Ok(Self {
            store: Mutex::new(Store {
                connection,
                state: projection.state,
                settled: projection.settled,
                messages: projection.messages,
                context_changes: projection.context_changes,
                child_records: projection.child_records,
                context_index: projection.context_index,
                calibration: None,
                live_backgrounds: BTreeMap::new(),
            }),
            _lock: lock,
            header,
            path,
            operations: Arc::new(OperationOwner::default()),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = database_path(path.as_ref())?;
        let lock = lock(&path)?;
        // A disappearing resume target must fail, not create a new empty file
        // between path validation and SQLite opening it.
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::default().difference(OpenFlags::SQLITE_OPEN_CREATE),
        )?;
        configure_connection(&connection)?;
        let header = read_header(&connection)?;
        let HistoryProjection {
            state,
            settled,
            messages,
            context_changes,
            child_records,
            context_index,
            ..
        } = project_connection(&connection, ProjectionMode::Full)?;
        prepare_writer(&connection)?;
        Ok(Self {
            store: Mutex::new(Store {
                connection,
                state,
                settled,
                messages,
                context_changes,
                child_records,
                context_index,
                calibration: None,
                live_backgrounds: BTreeMap::new(),
            }),
            _lock: lock,
            header,
            path,
            operations: Arc::new(OperationOwner::default()),
        })
    }

    /// Copy committed conversation and context into a new Session. Future
    /// entries are independent; the working directory remains shared.
    pub fn clone_to(&self, path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let source = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entries = read_entries(&source.connection)?;
        drop(source);
        self.copy_entries_to(path.as_ref(), entries)
    }

    /// Copy the valid prefix at a selected Turn boundary into an independent
    /// Session. The source and target still share the live working directory.
    pub fn fork_to(&self, path: impl AsRef<Path>, point: ForkPoint) -> Result<Self, SessionError> {
        let source = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let (mut entries, selected_model) = fork_prefix(&source.connection, point)?;
        if let Some(model) = selected_model {
            entries.push(SessionEntry::ModelSelected { model });
        }
        drop(source);
        self.copy_entries_to(path.as_ref(), entries)
    }

    /// Preferences of the exact prefix a fork will copy, not merely those at
    /// Turn admission. Idle recovery/preferences can change before a Turn ends.
    pub fn fork_preferences(
        &self,
        point: ForkPoint,
    ) -> Result<(ModelRef, Reasoning), SessionError> {
        let source = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let (entries, selected_model) = fork_prefix(&source.connection, point)?;
        let projection = project(entries.iter().map(Ok), ProjectionMode::Metadata, &|entry| {
            Ok(entries[(entry - 1) as usize].clone())
        })?;
        Ok((
            selected_model
                .or(projection.state.last_model)
                .ok_or(SessionError::InvalidForkPoint)?,
            projection.state.reasoning,
        ))
    }

    fn copy_entries_to(
        &self,
        path: &Path,
        entries: Vec<SessionEntry>,
    ) -> Result<Self, SessionError> {
        Self::create_initial(
            path,
            self.header.cwd.as_deref(),
            uuid::Uuid::new_v4(),
            &without_delivery_keys(entries),
        )
    }

    /// Read only the immutable logical identity, not execution status or history
    /// validity. Catalog lookup can filter headers before validating full history.
    pub fn inspect_id(path: impl AsRef<Path>) -> Result<uuid::Uuid, SessionError> {
        let path = database_path(path.as_ref())?;
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        Ok(read_header(&connection)?.id)
    }

    /// Validate committed history without accumulating historical payloads. Labels
    /// contain at most 120 characters from the first input's first line.
    pub fn inspect_metadata(path: impl AsRef<Path>) -> Result<SessionMetadata, SessionError> {
        let path = database_path(path.as_ref())?;
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        let snapshot = connection.unchecked_transaction()?;
        let header = read_header(&snapshot)?;
        let projection = project_connection(&snapshot, ProjectionMode::Metadata)?;

        Ok(SessionMetadata {
            id: header.id,
            cwd: header.cwd,
            name: header.name,
            last_model: projection.state.last_model,
            turns: projection.turns,
            first_input: projection.first_input,
        })
    }

    /// Read persisted facts without taking write ownership or repairing history.
    pub fn inspect(path: impl AsRef<Path>) -> Result<SessionView, SessionError> {
        let path = database_path(path.as_ref())?;
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let header = read_header(&connection)?;
        let entries = read_entries(&connection)?;
        let HistoryProjection {
            state,
            messages,
            context_index,
            ..
        } = project(entries.iter().map(Ok), ProjectionMode::Full, &|entry| {
            Ok(entries[(entry - 1) as usize].clone())
        })?;
        Ok(SessionView {
            id: header.id,
            cwd: header.cwd,
            name: header.name,
            context_path: context_index.path(context_index.head()),
            entries,
            messages: messages.into_iter().map(|record| record.message).collect(),
            unfinished_turn: state.active,
            unfinished_user_shell: state.pending_shell.map(|shell| UnobservedUserShell {
                admission_entry: shell.admission_entry,
            }),
            last_end: state.last_end,
            last_model: state.last_model,
            reasoning: state.reasoning,
            last_effective_model: state.last_effective_model,
            last_context: state.last_context,
            compacted_through: state.compaction.map(|(through, _)| through),
            last_execution: state.last_execution,
            last_usage: state.last_usage,
        })
    }

    /// Logical execution identity; independent of path and provider affinity.
    pub fn id(&self) -> uuid::Uuid {
        self.header.id
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn cwd(&self) -> Option<&Path> {
        self.header.cwd.as_deref()
    }
    /// Stable transport affinity identity. Conversation copies get a fresh ID
    /// through `create`, while reopening reads the existing immutable header.
    pub fn provider_session_id(&self) -> uuid::Uuid {
        self.header.provider_session_id
    }
    /// Model-facing conversation: human thinking is excluded; opaque replay
    /// retains its producing provider. Use the view for human/raw inspection.
    pub fn messages(&self) -> Result<Vec<Message>, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .messages
            .iter()
            .map(|record| record.message.clone())
            .collect())
    }

    /// Run one admitted direct shell through retained, Session-owned custody.
    /// The host must preflight its executor before calling this entry point.
    /// Its owned callback respects stop before new effects and joins all work
    /// already started before returning the observed output.
    pub fn run_user_shell<F, W>(
        self: &Arc<Self>,
        command: String,
        exclude_from_context: bool,
        stop: CancellationToken,
        execute: F,
    ) -> Operation<crate::ToolOutput, SessionError>
    where
        F: FnOnce(String, CancellationToken) -> W + Send + 'static,
        W: std::future::Future<Output = crate::ToolOutput> + Send + 'static,
    {
        let session = self.clone();
        self.operations.start(stop.clone(), async move {
            if stop.is_cancelled() {
                return Err(SessionError::UserShellCancelled);
            }
            let entry = session.admit_user_shell(command.clone(), exclude_from_context)?;
            let output = execute(command, stop).await;
            session.record_user_shell(entry, output.value.clone(), output.is_error)?;
            Ok(output)
        })
    }

    fn admit_user_shell(
        &self,
        command: String,
        exclude_from_context: bool,
    ) -> Result<u64, SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let mut entries = interrupted_operation_entries(&store, self.id());
        entries.push(SessionEntry::UserShellAdmitted {
            command,
            exclude_from_context,
        });
        append(&mut store, &entries)?;
        Ok(store.state.sequence)
    }

    fn record_user_shell(
        &self,
        admission_entry: u64,
        output: serde_json::Value,
        is_error: bool,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::UserShellSettled {
                admission_entry,
                outcome: UserShellOutcome::Observed { output, is_error },
            }],
        )
    }

    /// Check current idleness without reserving it for a later action.
    pub fn ensure_idle(&self) -> Result<(), SessionError> {
        self.reserve_idle().map(drop)
    }

    /// Hold reversible idle authority through synchronous binding preparation
    /// and publication. Failed preparation releases it without closing execution.
    pub fn reserve_idle(&self) -> Result<IdleSession<'_>, SessionError> {
        self.operations.try_lock().map_err(SessionError::from)
    }

    /// Close execution admission, request stop and join retained work. A timed-out
    /// wait leaves admission closed. This also reports unread operation failures
    /// without consuming a retained handle's result. Complete shutdown before
    /// destroying the Tokio runtime; a genuine task panic cannot prove quiescence.
    pub async fn shutdown(&self) -> Result<(), SessionError> {
        self.operations.shutdown().await.map_err(SessionError::from)
    }

    pub fn entry_count(&self) -> Result<u64, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .sequence)
    }

    /// Provider-neutral context projection when no effective target is supplied.
    /// Opaque provider replay is retained only for the direct-model case where
    /// the logical selection and last effective physical model are identical.
    /// Request preparation uses `request_basis` with the resolved effective
    /// model to capture context, reasoning and replay state together.
    pub fn context_messages(&self) -> Result<Vec<Message>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let direct = match (
            store.state.last_model.as_ref(),
            store.state.last_effective_model.as_ref(),
        ) {
            (Some(logical), Some(effective)) if logical == effective => Some(effective),
            _ => None,
        };
        Ok(context_projection(&store, direct))
    }

    pub fn model_context(&self) -> Result<Option<ModelContextSnapshot>, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .last_context
            .clone())
    }

    pub(crate) fn request_basis(&self, model: &ModelRef) -> Result<RequestBasis, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(RequestBasis {
            turn: store.state.active,
            messages: context_projection(&store, Some(model)),
            previous: store.state.last_context.clone(),
            reasoning: store.state.reasoning,
            assistant_seen: store.state.assistant_seen_in_turn,
            prefix_bound: store.state.prefix_bound_in_turn,
            context_changes: (store.state.compaction.is_none()
                && store.state.replay_epoch_start == 0
                && store.state.last_effective_model.as_ref() == Some(model))
            .then(|| store.context_changes.clone()),
        })
    }

    /// Return a full provider-neutral context timeline only while the raw
    /// message projection is still byte-for-byte comparable to Session history.
    /// Compaction or a replay epoch change deliberately falls back to the
    /// latest leading context instead of guessing at provider prefix semantics.
    #[cfg(test)]
    pub(crate) fn context_timeline_for(
        &self,
        model: &ModelRef,
        current: &ModelContextSnapshot,
    ) -> Result<Option<ModelContextTimeline>, SessionError> {
        Ok(self.request_basis(model)?.timeline(current))
    }

    /// Project a request for one model without reviving opaque replay from a
    /// previous model epoch. The raw Session keeps every original message.
    #[cfg(test)]
    pub(crate) fn context_messages_for(
        &self,
        model: &ModelRef,
    ) -> Result<Vec<Message>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(context_projection(&store, Some(model)))
    }

    /// Preparing an incompatible request must not discard evidence belonging
    /// to the current binding. Only admitted issuance advances runtime scope.
    pub(crate) fn request_calibration_for(
        &self,
        scope: &RequestScope,
    ) -> Result<Option<RequestCalibration>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(store
            .calibration
            .as_ref()
            .filter(|calibration| calibration.matches_scope(scope))
            .cloned())
    }

    pub(crate) fn activate_request_calibration(
        &self,
        measurement: &RequestMeasurement,
        scope: &RequestScope,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store
            .calibration
            .as_ref()
            .is_some_and(|calibration| !calibration.applies(measurement, scope))
        {
            store.calibration = None;
        }
        Ok(())
    }

    pub(crate) fn set_request_calibration(
        &self,
        calibration: RequestCalibration,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        store.calibration = Some(calibration);
        Ok(())
    }

    /// Admit one prepared coding request atomically before provider issuance.
    /// A failed write publishes neither its effective-model change nor context.
    pub(crate) fn admit_request(
        &self,
        turn: u64,
        model: ModelRef,
        context: ModelContextSnapshot,
    ) -> Result<bool, SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store.state.active != Some(turn) || !store.state.pending.is_empty() {
            return Err(SessionError::InvalidHistory);
        }
        let mut entries = Vec::new();
        // The first observed assistant still establishes the initial effective
        // model; preparation must not manufacture a completed execution fact.
        if store
            .state
            .last_effective_model
            .as_ref()
            .is_some_and(|previous| previous != &model)
        {
            entries.push(SessionEntry::EffectiveModelChanged { turn, model });
        }
        if store.state.last_context.as_ref() != Some(&context) {
            entries.push(SessionEntry::ModelContextChanged { turn, context });
        }
        if entries.is_empty() {
            return Ok(false);
        }
        append(&mut store, &entries)?;
        Ok(true)
    }

    /// Permanently omit prior opaque replay from future model requests after
    /// an adapter detects a changed signed context before dispatch.
    pub(crate) fn rebase_provider_replay(&self, turn: u64) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &[SessionEntry::ProviderReplayRebased { turn }])
    }

    pub(crate) fn record_cache_warm(
        &self,
        turn: u64,
        execution: ModelExecution,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::CacheWarm {
                turn,
                execution,
                usage,
            }],
        )
    }

    pub(crate) fn record_compaction(
        &self,
        through_entry: u64,
        summary: String,
        execution: ModelExecution,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::Compacted {
                through_entry,
                summary,
                execution,
                usage,
            }],
        )
    }
    pub fn status(&self) -> Result<SessionStatus, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let header = read_header(&store.connection)?;
        Ok(SessionStatus {
            cwd: header.cwd,
            name: header.name,
            reasoning: store.state.reasoning,
            entries: store.state.sequence,
        })
    }

    pub fn view(&self) -> Result<SessionView, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entries = read_entries(&store.connection)?;
        let header = read_header(&store.connection)?;
        Ok(SessionView {
            id: header.id,
            cwd: header.cwd,
            name: header.name,
            context_path: store.context_index.path(store.context_index.head()),
            entries,
            messages: store
                .messages
                .iter()
                .map(|record| record.message.clone())
                .collect(),
            unfinished_turn: store.state.active,
            unfinished_user_shell: store.state.pending_shell.as_ref().map(|shell| {
                UnobservedUserShell {
                    admission_entry: shell.admission_entry,
                }
            }),
            last_end: store.state.last_end.clone(),
            last_model: store.state.last_model.clone(),
            reasoning: store.state.reasoning,
            last_effective_model: store.state.last_effective_model.clone(),
            last_context: store.state.last_context.clone(),
            compacted_through: store.state.compaction.as_ref().map(|(through, _)| *through),
            last_execution: store.state.last_execution.clone(),
            last_usage: store.state.last_usage,
        })
    }

    /// Change only display metadata. Conversation history remains append-only.
    pub fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        if name.is_some_and(|name| name.len() > 120 || name.chars().any(char::is_control)) {
            return Err(SessionError::InvalidName);
        }
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let mut header = read_header(&store.connection)?;
        header.name = name.map(str::to_owned);
        store.connection.execute(
            "UPDATE session SET header = ?1 WHERE id = 1",
            params![serde_json::to_vec(&header)?],
        )?;
        Ok(())
    }

    /// Persist an idle Session's identity without route validation. Use
    /// `Agent::select_model` to preflight the retained generation preference.
    pub fn select_model(&self, model: ModelRef) -> Result<(), SessionError> {
        let guard = self.operations.try_lock().map_err(SessionError::from)?;
        self.record_selection(&guard, model, None)
    }

    pub fn model_preference(&self) -> Result<Option<ModelRef>, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .last_model
            .clone())
    }

    pub fn reasoning(&self) -> Result<Reasoning, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .reasoning)
    }

    /// Caller holds this Session's operation gate through preflight and publication.
    /// Persist the validated pair together: an invocation-only model override must
    /// not leave a preference incompatible with the model restored on reopen.
    pub(crate) fn record_selection(
        &self,
        _guard: &IdleSession<'_>,
        model: ModelRef,
        reasoning: Option<Reasoning>,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let mut entries = Vec::new();
        if store.state.last_model.as_ref() != Some(&model) {
            entries.push(SessionEntry::ModelSelected { model });
        }
        if let Some(reasoning) = reasoning
            && store.state.reasoning != reasoning
        {
            entries.push(SessionEntry::ReasoningSelected { reasoning });
        }
        if entries.is_empty() {
            return Ok(());
        }
        append(&mut store, &entries)
    }

    /// Close any interrupted Turn and accept the next input in one transaction.
    /// Caller owns this Session's operation custody for the entire resulting Turn.
    #[cfg(test)]
    pub(crate) fn begin_turn(
        &self,
        prompt: String,
        model: ModelRef,
    ) -> Result<(u64, usize), SessionError> {
        self.begin_turn_message(
            Message {
                role: Role::User,
                content: vec![Content::Text(prompt)],
                provider_replay: None,
            },
            model,
        )
    }

    #[cfg(test)]
    pub(crate) fn begin_turn_message(
        &self,
        input: Message,
        model: ModelRef,
    ) -> Result<(u64, usize), SessionError> {
        match self.admit_turn(input, model, None, None)? {
            TurnAdmission::New {
                turn, interrupted, ..
            } => Ok((turn, interrupted)),
            TurnAdmission::Existing(_) => unreachable!("anonymous input cannot reconcile"),
        }
    }

    pub(crate) fn selected_context_path(&self) -> Result<Vec<u64>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(store.context_index.path(store.context_index.head()))
    }

    pub(crate) fn admit_turn(
        &self,
        input: Message,
        model: ModelRef,
        request: Option<RequestKey>,
        recovery: Option<RecoveryBinding>,
    ) -> Result<TurnAdmission, SessionError> {
        self.admit_turn_input(input, model, request, recovery, Vec::new())
    }

    pub(crate) fn admit_turn_input(
        &self,
        input: Message,
        model: ModelRef,
        request: Option<RequestKey>,
        recovery: Option<RecoveryBinding>,
        completion: Vec<crate::CompletionNotice>,
    ) -> Result<TurnAdmission, SessionError> {
        if !valid_user_message(&input)
            || (!completion.is_empty() && input != crate::completion::notice_message(&completion))
        {
            return Err(SessionError::InvalidUserInput);
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if let Some(key) = &request
            && let Some(receipt) =
                submission::request_receipt(&store.connection, self.id(), key, Some(&input))?
        {
            return Ok(TurnAdmission::Existing(receipt));
        }
        let mut entries = interrupted_operation_entries(&store, self.id());
        let interrupted = store.state.pending.len();
        let turn = store
            .state
            .last_id
            .checked_add(1)
            .ok_or(SessionError::TurnIdExhausted)?;
        entries.push(SessionEntry::TurnStarted {
            turn,
            input,
            model,
            completion,
            request,
            recovery,
        });
        append(&mut store, &entries)?;
        let Some(SessionEntry::TurnStarted { input, .. }) = entries.pop() else {
            unreachable!("admission batch ends with its committed TurnStarted");
        };
        Ok(TurnAdmission::New {
            turn,
            entry: store.state.sequence,
            interrupted,
            input,
        })
    }

    #[cfg(test)]
    pub(crate) fn record_assistant(
        &self,
        turn: u64,
        message: Message,
        usage: Usage,
        continue_turn: bool,
    ) -> Result<bool, SessionError> {
        let activities = default_tool_activities(&message);
        let logical = self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .last_model
            .clone()
            .ok_or(SessionError::InvalidHistory)?;
        let effective = logical.clone();
        {
            let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
            if store
                .state
                .last_effective_model
                .as_ref()
                .is_some_and(|previous| previous != &effective)
            {
                append(
                    &mut store,
                    &[SessionEntry::EffectiveModelChanged {
                        turn,
                        model: effective.clone(),
                    }],
                )?;
            }
        }
        self.record_assistant_with_activities(
            turn,
            message,
            activities,
            ModelExecution {
                route: ion_ai::ModelRoute {
                    logical,
                    effective,
                    reason: ion_ai::ModelRouteReason::UserRequest,
                },
                returned_model: None,
            },
            usage,
            continue_turn,
        )
        .map(|committed| committed.complete)
    }

    pub(crate) fn record_assistant_with_activities(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        continue_turn: bool,
    ) -> Result<CommittedAssistant, SessionError> {
        self.record_assistant_entries(
            turn,
            message,
            tool_activities,
            execution,
            usage,
            continue_turn,
            Vec::new(),
            Vec::new(),
        )
        .map(|(assistant, _)| assistant)
    }

    /// Publish the completed assistant and queued steering in one batch; a
    /// failed transaction must leave both unpublished for the host to recover.
    #[expect(
        clippy::too_many_arguments,
        reason = "assistant execution and typed input facts share one atomic commit"
    )]
    pub(crate) fn record_assistant_with_inputs(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        steering: Vec<Message>,
        completion: Vec<crate::CompletionNotice>,
    ) -> Result<(CommittedAssistant, CommittedInputEntries), SessionError> {
        let continue_turn = !steering.is_empty() || !completion.is_empty();
        self.record_assistant_entries(
            turn,
            message,
            tool_activities,
            execution,
            usage,
            continue_turn,
            steering,
            completion,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one atomic assistant/steering commit carries its complete durable facts"
    )]
    fn record_assistant_entries(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        continue_turn: bool,
        steering: Vec<Message>,
        completion: Vec<crate::CompletionNotice>,
    ) -> Result<(CommittedAssistant, CommittedInputEntries), SessionError> {
        let has_calls = message
            .content
            .iter()
            .any(|part| matches!(part, Content::ToolCall(_)));
        validate_tool_activities(&message, &tool_activities)?;
        let mut entries = vec![SessionEntry::Assistant {
            turn,
            message,
            tool_activities,
            execution,
            usage,
            termination: ResponseTermination::Completed,
        }];
        if !has_calls && !continue_turn {
            entries.push(SessionEntry::TurnEnded {
                turn,
                reason: TurnEndReason::Completed,
            });
        }
        entries.extend(
            steering
                .into_iter()
                .map(|input| SessionEntry::Steering { turn, input }),
        );
        if !completion.is_empty() {
            entries.push(SessionEntry::Completion {
                turn,
                notices: completion,
            });
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entry = store.state.sequence + 1;
        append(&mut store, &entries)?;
        let mut inputs = CommittedInputEntries::default();
        for (offset, fact) in entries.iter().enumerate() {
            let coordinate = entry + offset as u64;
            match fact {
                SessionEntry::Steering { .. } => {
                    if inputs.steering.is_empty() {
                        inputs.steering.start = coordinate;
                    }
                    inputs.steering.end = coordinate + 1;
                }
                SessionEntry::Completion { .. } => inputs.completion = Some(coordinate),
                _ => {}
            }
        }
        Ok((
            CommittedAssistant {
                entry,
                complete: !has_calls && !continue_turn,
            },
            inputs,
        ))
    }

    /// Publish a truncated assistant attempt and every unexecuted call's
    /// synthetic result together. A crash cannot leave these calls pending
    /// and falsely classify their effects as unknown on reopen.
    pub(crate) fn record_truncated_assistant(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
    ) -> Result<(u64, Vec<crate::ToolSettlement>), SessionError> {
        let results = message
            .content
            .iter()
            .filter_map(|part| match part {
                Content::ToolCall(call) => Some(crate::ToolSettlement {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    outcome: crate::ToolOutcome::NotDispatched {
                        reason: "the model response hit the output token limit and its arguments may be truncated. Reissue the complete call.".into(),
                    },
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        if results.is_empty() {
            return Err(SessionError::InvalidHistory);
        }
        validate_tool_activities(&message, &tool_activities)?;
        let mut entries = vec![SessionEntry::Assistant {
            turn,
            message,
            tool_activities,
            execution,
            usage,
            termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
        }];
        entries.extend(
            results
                .iter()
                .cloned()
                .map(|result| SessionEntry::ToolResult { turn, result }),
        );
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entry = store.state.sequence + 1;
        append(&mut store, &entries)?;
        Ok((entry, results))
    }

    #[cfg(test)]
    pub(crate) fn record_steerings(
        &self,
        turn: u64,
        inputs: Vec<Message>,
    ) -> Result<(), SessionError> {
        self.record_inputs(turn, inputs, Vec::new()).map(|_| ())
    }

    pub(crate) fn record_inputs(
        &self,
        turn: u64,
        inputs: Vec<Message>,
        completion: Vec<crate::CompletionNotice>,
    ) -> Result<CommittedInputEntries, SessionError> {
        let steering_count = inputs.len() as u64;
        let has_completion = !completion.is_empty();
        let mut entries = inputs
            .into_iter()
            .map(|input| SessionEntry::Steering { turn, input })
            .collect::<Vec<_>>();
        if !completion.is_empty() {
            entries.push(SessionEntry::Completion {
                turn,
                notices: completion,
            });
        }
        if entries.is_empty() {
            return Ok(CommittedInputEntries::default());
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let first = store.state.sequence + 1;
        append(&mut store, &entries)?;
        Ok(CommittedInputEntries {
            steering: first..first + steering_count,
            completion: has_completion.then_some(first + steering_count),
        })
    }

    #[cfg(test)]
    pub(crate) fn record_tool_result(
        &self,
        turn: u64,
        result: ToolResult,
    ) -> Result<(), SessionError> {
        self.record_tool_result_with_context(
            turn,
            result,
            crate::ToolResultProjection::Observed,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn record_tool_result_with_context(
        &self,
        turn: u64,
        result: ToolResult,
        projection: crate::ToolResultProjection,
        context: Option<ModelContextSnapshot>,
    ) -> Result<(), SessionError> {
        self.record_tool_settlement_with_context(
            turn,
            crate::ToolSettlement::observed(result, projection),
            context,
        )
    }

    pub(crate) fn record_tool_settlement_with_context(
        &self,
        turn: u64,
        result: crate::ToolSettlement,
        context: Option<ModelContextSnapshot>,
    ) -> Result<(), SessionError> {
        let mut entries = vec![SessionEntry::ToolResult { turn, result }];
        if let Some(context) = context {
            entries.push(SessionEntry::ModelContextChanged { turn, context });
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)
    }

    #[cfg(test)]
    pub(crate) fn tool_occurrence(
        &self,
        turn: u64,
        call_id: &str,
    ) -> Result<crate::ToolOccurrence, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store.state.active != Some(turn) {
            return Err(SessionError::InvalidHistory);
        }
        store
            .state
            .pending
            .iter()
            .find(|pending| pending.call_id == call_id)
            .map(|pending| pending.occurrence)
            .ok_or(SessionError::InvalidHistory)
    }

    pub(crate) fn record_child_intent(
        &self,
        turn: u64,
        intent: crate::ChildIntent,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::ChildToolAdmitted { turn, intent }],
        )
    }

    pub(crate) fn record_child_outcome(
        &self,
        turn: u64,
        parent: crate::ToolOccurrence,
        child: usize,
        outcome: crate::ChildOutcome,
    ) -> Result<crate::ChildOutcome, SessionError> {
        let entry = SessionEntry::ChildToolResult {
            turn,
            parent,
            child,
            outcome,
        };
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, std::slice::from_ref(&entry))?;
        let SessionEntry::ChildToolResult { outcome, .. } = entry else {
            unreachable!()
        };
        Ok(outcome)
    }

    pub(crate) fn end_turn(
        &self,
        turn: u64,
        reason: TurnEndReason,
        unstarted: &BTreeSet<String>,
    ) -> Result<Vec<(crate::ToolSettlement, ToolActivity)>, SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store.state.active != Some(turn) {
            return Err(SessionError::InvalidHistory);
        }
        if unstarted.iter().any(|id| {
            !store
                .state
                .pending
                .iter()
                .any(|pending| &pending.call_id == id)
        }) {
            return Err(SessionError::InvalidHistory);
        }
        let settlements = store
            .state
            .pending
            .iter()
            .map(|pending| {
                (
                    pending_settlement(pending, unstarted),
                    pending.activity.clone(),
                )
            })
            .collect();
        let mut entries = pending_results(turn, &store.state.pending, unstarted);
        entries.push(SessionEntry::TurnEnded { turn, reason });
        append(&mut store, &entries)?;
        Ok(settlements)
    }
}

impl Store {
    fn orphaned_backgrounds(&self, session: uuid::Uuid) -> Vec<SessionEntry> {
        self.state
            .backgrounds
            .iter()
            .filter(|(run, link)| {
                link.site.session == session
                    && self
                        .live_backgrounds
                        .get(run)
                        .is_none_or(|lease| lease.strong_count() == 0)
            })
            .map(|(run, _)| SessionEntry::DelegationSettled {
                run: *run,
                outcome: crate::DelegationOutcome::Unknown,
            })
            .collect()
    }
}

fn interrupted_operation_entries(store: &Store, session: uuid::Uuid) -> Vec<SessionEntry> {
    let state = &store.state;
    let mut entries = store.orphaned_backgrounds(session);
    if let Some(shell) = &state.pending_shell {
        entries.push(SessionEntry::UserShellSettled {
            admission_entry: shell.admission_entry,
            outcome: UserShellOutcome::Unknown,
        });
    }
    if let Some(turn) = state.active {
        entries.extend(pending_results(turn, &state.pending, &BTreeSet::new()));
        entries.push(SessionEntry::TurnEnded {
            turn,
            reason: TurnEndReason::Interrupted,
        });
    }
    entries
}

/// Preserve entry coordinates, compaction boundaries and replay facts exactly.
/// The selected settled prefix, not an arbitrary message suffix, is the fork.
fn fork_prefix(
    connection: &Connection,
    point: ForkPoint,
) -> Result<(Vec<SessionEntry>, Option<ModelRef>), SessionError> {
    let mut entries = read_entries(connection)?;
    let boundary = entries
        .iter()
        .position(|entry| match (point, entry) {
            (ForkPoint::BeforeTurn(selected), SessionEntry::TurnStarted { turn, .. })
            | (ForkPoint::AfterTurn(selected), SessionEntry::TurnEnded { turn, .. }) => {
                selected == *turn
            }
            _ => false,
        })
        .ok_or(SessionError::InvalidForkPoint)?;
    let selected_model = match (&point, &entries[boundary]) {
        (ForkPoint::BeforeTurn(_), SessionEntry::TurnStarted { model, .. }) => Some(model.clone()),
        _ => None,
    };
    entries.truncate(boundary + usize::from(matches!(point, ForkPoint::AfterTurn(_))));
    Ok((entries, selected_model))
}

fn without_delivery_keys(mut entries: Vec<SessionEntry>) -> Vec<SessionEntry> {
    // Conversation/context copying is not acceptance of external deliveries.
    for entry in &mut entries {
        if let SessionEntry::TurnStarted { request, .. } = entry {
            *request = None;
        }
    }
    entries
}

fn pending_results(
    turn: u64,
    pending: &[PendingTool],
    unstarted: &BTreeSet<String>,
) -> Vec<SessionEntry> {
    pending
        .iter()
        .flat_map(|pending| {
            pending.delegations.unknowns().chain(
                pending
                    .children
                    .iter()
                    .map(|child| SessionEntry::ChildToolResult {
                        turn,
                        parent: pending.occurrence,
                        child: *child,
                        outcome: crate::ChildOutcome::Unknown,
                    })
                    .chain(std::iter::once(SessionEntry::ToolResult {
                        turn,
                        result: pending_settlement(pending, unstarted),
                    })),
            )
        })
        .collect()
}

fn pending_settlement(
    pending: &PendingTool,
    unstarted: &BTreeSet<String>,
) -> crate::ToolSettlement {
    crate::ToolSettlement {
        call_id: pending.call_id.clone(),
        name: pending.name.clone(),
        outcome: if unstarted.contains(&pending.call_id) {
            crate::ToolOutcome::NotDispatched {
                reason: "the operation ended before dispatch".into(),
            }
        } else {
            crate::ToolOutcome::Unknown
        },
    }
}

fn append(store: &mut Store, entries: &[SessionEntry]) -> Result<(), SessionError> {
    let mut replacement = None;
    let mut candidate = store.state.clone();
    // A shared pending shell owns the final context message. Seed only that
    // slot so candidate settlement can replace it without rebuilding history.
    let pending_message = store
        .state
        .pending_shell
        .as_ref()
        .filter(|shell| !shell.exclude_from_context)
        .map(|shell| {
            ProjectedMessage::new(
                shell.admission_entry,
                shell_message(&shell.command, &UserShellOutcome::Unknown, false),
            )
        });
    let replace_pending_message = pending_message.is_some();
    let mut new_messages: Vec<_> = pending_message.into_iter().collect();
    let message_start = store.messages.len() - usize::from(replace_pending_message);
    let mut new_context_changes = Vec::new();
    let mut new_settled = Vec::new();
    let encoded = entries
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            candidate.apply(entry, &mut new_messages, &store.settled, &mut new_settled)?;
            if let SessionEntry::ContextSelected { head_entry } = entry {
                if replacement.is_some() || offset + 1 != entries.len() {
                    return Err(SessionError::InvalidHistory);
                }
                let navigation::RestoredContext {
                    state,
                    messages,
                    settled,
                    changes,
                } = restore_context(
                    &store.context_index,
                    *head_entry,
                    ProjectionMode::Full,
                    &|entry| navigation::read_entry(&store.connection, entry),
                )?;
                candidate.restore_context(state);
                replacement = Some((messages, settled, changes));
            }
            if let SessionEntry::ModelContextChanged { context, .. } = entry {
                new_context_changes.push(ModelContextChange {
                    after_message: message_start + new_messages.len(),
                    context: model_context_state(context),
                });
            }
            encode_entry(entry)
        })
        .collect::<Result<Vec<_>, SessionError>>()?;
    let tx = store.connection.transaction()?;
    for body in encoded {
        tx.execute("INSERT INTO entries(body) VALUES (?1)", params![body])?;
    }
    tx.commit()?;
    let mut sequence = store.state.sequence;
    for entry in entries {
        sequence += 1;
        store.child_records.observe(sequence, entry);
        store.context_index.observe(entry);
        if let SessionEntry::DelegationSettled { run, .. } = entry {
            store.live_backgrounds.remove(run);
        }
    }
    if entries.iter().any(|entry| {
        matches!(
            entry,
            SessionEntry::ContextSelected { .. }
                | SessionEntry::Compacted { .. }
                | SessionEntry::ProviderReplayRebased { .. }
                | SessionEntry::EffectiveModelChanged { .. }
                | SessionEntry::ModelSelected { .. }
                | SessionEntry::ReasoningSelected { .. }
                | SessionEntry::ModelContextChanged { .. }
        )
    }) {
        store.calibration = None;
    }
    store.state = candidate;
    if let Some((messages, settled, changes)) = replacement {
        store.messages = messages;
        store.settled = settled;
        store.context_changes = changes;
    } else {
        store.settled.extend(new_settled);
        if replace_pending_message {
            store.messages.pop();
        }
        store.messages.extend(new_messages);
        store.context_changes.extend(new_context_changes);
    }
    Ok(())
}

fn encode_entry(entry: &SessionEntry) -> Result<Vec<u8>, SessionError> {
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(SessionError::EntryTooLarge);
    }
    Ok(bytes)
}

fn summary_message(through_entry: u64, summary: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(format!(
            "Earlier conversation summary (selected context through entry {through_entry} was compacted; original committed records remain in Session history):\n{summary}"
        ))],
        provider_replay: None,
    }
}

#[cfg(test)]
fn default_tool_activities(message: &Message) -> Vec<StoredToolActivity> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::ToolCall(call) => Some(StoredToolActivity {
                call_id: call.id.clone(),
                activity: ToolActivity::external(call.name.clone()),
            }),
            _ => None,
        })
        .collect()
}

fn validate_tool_activities(
    message: &Message,
    activities: &[StoredToolActivity],
) -> Result<(), SessionError> {
    let call_ids = message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if call_ids.len() != activities.len()
        || call_ids
            .iter()
            .zip(activities)
            .any(|(call_id, activity)| *call_id != activity.call_id)
    {
        return Err(SessionError::InvalidHistory);
    }
    Ok(())
}

fn model_context_state(context: &ModelContextSnapshot) -> ModelContextState {
    ModelContextState {
        instructions: Some(context.instructions.clone()),
        tools: context.tools.clone(),
    }
}

fn valid_model_context(context: &ModelContextSnapshot) -> bool {
    let mut names = BTreeSet::new();
    context
        .tools
        .iter()
        .all(|tool| !tool.name.trim().is_empty() && names.insert(tool.name.as_str()))
}

fn shell_outcome_at(entries: &[SessionEntry], settlement_index: usize) -> &UserShellOutcome {
    match entries.get(settlement_index) {
        Some(SessionEntry::UserShellSettled { outcome, .. }) => outcome,
        _ => &UserShellOutcome::Unknown,
    }
}

fn projected_entry_messages(entries: &[SessionEntry], display: bool) -> Vec<Option<Message>> {
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
fn model_message(message: &Message) -> Message {
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

fn message_from_entry(coordinate: u64, entry: &SessionEntry) -> Option<Message> {
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

fn shell_message(command: &str, outcome: &UserShellOutcome, excluded: bool) -> Message {
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

pub(crate) fn valid_user_message(message: &Message) -> bool {
    message.role == Role::User
        && message.provider_replay.is_none()
        && !message.content.is_empty()
        && message.content.iter().all(|part| match part {
            Content::Text(_) => true,
            Content::Image(image) => image.validate().is_ok(),
            Content::Thinking(_) | Content::ToolCall(_) | Content::ToolResult(_) => false,
        })
        && message.content.iter().any(|part| match part {
            Content::Text(text) => !text.trim().is_empty(),
            Content::Image(_) => true,
            Content::Thinking(_) | Content::ToolCall(_) | Content::ToolResult(_) => false,
        })
}

#[derive(Clone, Copy)]
enum ProjectionMode {
    Full,
    Metadata,
}

fn project_connection(
    connection: &Connection,
    mode: ProjectionMode,
) -> Result<HistoryProjection, SessionError> {
    // Validate one fact at a time without an owned raw-history export. The
    // projection mode determines which derived values must remain resident.
    let mut statement = connection.prepare("SELECT body FROM entries ORDER BY seq")?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    project(
        rows.map(|body| Ok(serde_json::from_slice::<SessionEntry>(&body?)?)),
        mode,
        &|entry| navigation::read_entry(connection, entry),
    )
}

fn project<E: Borrow<SessionEntry>>(
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

fn input_preview(entry: &SessionEntry) -> Option<SessionInputPreview> {
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

fn read_entries(connection: &Connection) -> Result<Vec<SessionEntry>, SessionError> {
    let mut statement = connection.prepare("SELECT body FROM entries ORDER BY seq")?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut entries = Vec::new();
    for body in rows {
        entries.push(serde_json::from_slice(&body?)?);
    }
    Ok(entries)
}

fn read_header(connection: &Connection) -> Result<Header, SessionError> {
    let encoded: Vec<u8> = connection
        .query_row("SELECT header FROM session WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(SessionError::InvalidDatabase)?;
    // Reject obsolete formats before interpreting the current header payload.
    // In particular, never invent an affinity identity when opening old data.
    #[derive(Deserialize)]
    struct Format {
        version: u32,
    }
    let format: Format = serde_json::from_slice(&encoded)?;
    if format.version != FORMAT_VERSION {
        return Err(SessionError::UnsupportedFormat(format.version));
    }
    Ok(serde_json::from_slice(&encoded)?)
}

fn configure_connection(connection: &Connection) -> Result<(), SessionError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn prepare_writer(connection: &Connection) -> Result<(), SessionError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

fn initialize_new(connection: &Connection) -> Result<(), SessionError> {
    configure_connection(connection)?;
    prepare_writer(connection)?;
    connection.execute_batch("CREATE TABLE session (id INTEGER PRIMARY KEY CHECK(id=1), header BLOB NOT NULL); CREATE TABLE entries (seq INTEGER PRIMARY KEY, body BLOB NOT NULL);
CREATE UNIQUE INDEX request_keys ON entries(json_extract(body, '$.data.request.scope'), json_extract(body, '$.data.request.id')) WHERE json_extract(body, '$.kind') = 'turn_started' AND json_extract(body, '$.data.request') IS NOT NULL;
CREATE UNIQUE INDEX delegation_runs ON entries(json_extract(body, '$.data.record.run')) WHERE json_extract(body, '$.kind') = 'delegation_admitted';
CREATE UNIQUE INDEX delegation_created_children ON entries(json_extract(body, '$.data.record.child')) WHERE json_extract(body, '$.kind') = 'delegation_admitted' AND json_extract(body, '$.data.record.spec.target.type') IN ('fresh', 'fork');
CREATE UNIQUE INDEX child_bindings ON entries(json_extract(body, '$.data.record.run')) WHERE json_extract(body, '$.kind') = 'child_session_of';
CREATE INDEX turn_ends ON entries(json_extract(body, '$.data.turn')) WHERE json_extract(body, '$.kind') = 'turn_ended';")?;
    Ok(())
}

fn database_path(path: &Path) -> Result<PathBuf, SessionError> {
    let path = path.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SessionError::NotFound
        } else {
            SessionError::Io(error)
        }
    })?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(SessionError::NotFound);
    }
    // SQLite resolves symlinks, but cannot unify hard-linked journal/WAL names.
    // Reject those aliases rather than claiming lock normalization makes them safe.
    // https://www.sqlite.org/howtocorrupt.html#multiple_links_to_the_same_file
    if metadata.nlink() != 1 {
        return Err(SessionError::HardLinkedDatabase);
    }
    Ok(path)
}

fn lock(path: &Path) -> Result<SessionLock, SessionError> {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(PathBuf::from(sidecar))?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            SessionError::AlreadyOpen
        } else {
            SessionError::Io(error.into())
        }
    })?;
    Ok(SessionLock(file))
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("completion inbox belongs to another receiving Session")]
    CompletionTargetMismatch,
    #[error("context changed since this preview; inspect history again")]
    StaleContextPosition,
    #[error("context selection must use a committed, complete call/result boundary")]
    InvalidContextPoint,
    #[error("authority scope must be nonblank, at most 1024 bytes, without control characters")]
    InvalidAuthorityScope,
    #[error("continuation authority or executable declarations changed or are unavailable")]
    RecoveryAuthorityChanged,
    #[error("recovery checkpoint is stale, belongs to another Session, or the Turn has ended")]
    StaleRecoveryPoint,
    #[error("delegation infrastructure failed: {0}")]
    DelegationInfrastructure(String),
    /// Infrastructure failed after the parent's joined outcome was committed.
    #[error("{error}")]
    DelegationSettledFailure {
        receipt: Box<crate::DelegationReceipt>,
        #[source]
        error: Box<SessionError>,
    },
    #[error("child resume refused: {0}")]
    DelegationTargetUnavailable(String),
    #[error("session does not exist")]
    NotFound,
    #[error("session has an active operation")]
    OperationActive,
    #[error(transparent)]
    Operation(OperationError),
    #[error("session is already open for writing")]
    AlreadyOpen,
    #[error("hard-linked session databases are unsupported; use Session clone or fork")]
    HardLinkedDatabase,
    #[error("invalid session path")]
    InvalidPath,
    #[error("invalid working directory")]
    InvalidWorkingDirectory,
    #[error("invalid or inconsistent session history")]
    InvalidHistory,
    #[error("invalid session database")]
    InvalidDatabase,
    #[error("unsupported session format version {0}")]
    UnsupportedFormat(u32),
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("user shell command cancelled before start")]
    UserShellCancelled,
    #[error("user input must contain text or valid images")]
    InvalidUserInput,
    #[error(
        "request scope and ID must be nonblank, at most 256 bytes each, without control characters"
    )]
    InvalidRequestKey,
    #[error("request identity was already accepted with different input")]
    RequestConflict,
    #[error("session name must be at most 120 bytes without control characters")]
    InvalidName,
    #[error("selected Turn boundary does not exist or has not ended")]
    InvalidForkPoint,
    #[error("session entry exceeds storage limit")]
    EntryTooLarge,
    #[error("turn identifier space exhausted")]
    TurnIdExhausted,
    #[error("session mutex was poisoned")]
    Poisoned,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl OperationFailure for SessionError {
    fn expected_cancellation(&self) -> bool {
        matches!(self, Self::UserShellCancelled)
    }
}

impl From<OperationError> for SessionError {
    fn from(error: OperationError) -> Self {
        match error {
            OperationError::Active => Self::OperationActive,
            other => Self::Operation(other),
        }
    }
}

#[cfg(test)]
mod tests;

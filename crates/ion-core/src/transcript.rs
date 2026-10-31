//! Typed coding transcript projection over durable Session facts and live agent events.
use std::collections::HashMap;

use ion_ai::{Content, Message, ResponseTermination};
use serde_json::Value;

use crate::{
    agent::AgentEvent,
    session::{SessionEntry, SessionView},
    tool_result::ToolResultProjection,
    tool_set::ToolActivity,
};

/// Coordinate in durable global Session history, before presentation filtering.
/// Facts without content parts (readiness and shell admission) use part zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TranscriptSource {
    pub entry: u64,
    pub part: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TranscriptActivityIdentity {
    Tool(crate::ToolOccurrence),
    Child {
        parent: crate::ToolOccurrence,
        child: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptPart {
    Text(String),
    Thinking(String),
    Image { mime_type: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMessage {
    /// None only for a removable streamed assistant response.
    pub source: Option<TranscriptSource>,
    pub turn: Option<u64>,
    pub steering: bool,
    pub parts: Vec<TranscriptPart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Queued,
    /// Live execution-start progress; never reconstructed as running on reopen.
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityResult {
    /// Only observed top-level output has a route delivery decision.
    pub projection: Option<ToolResultProjection>,
    pub value: Value,
    pub image_mime_types: Vec<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptActivity {
    pub identity: TranscriptActivityIdentity,
    pub call_id: String,
    pub name: String,
    pub activity: ToolActivity,
    pub arguments: Value,
    pub state: ActivityState,
    pub result: Option<ActivityResult>,
    pub children: Vec<TranscriptActivity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityGroup {
    pub source: TranscriptSource,
    pub turn: u64,
    pub activities: Vec<TranscriptActivity>,
    /// Whether subsequent silent calls can join this presentation group.
    /// Closed groups can still have pending outcomes; this is not immutability.
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserShellActivity {
    pub source: TranscriptSource,
    pub command: String,
    /// Absent observation displays as uncertain, but is not a stable settlement.
    pub outcome: Option<crate::UserShellOutcome>,
    pub exclude_from_context: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User(TranscriptMessage),
    Assistant(TranscriptMessage),
    ActivityGroup(ActivityGroup),
    UserShell(UserShellActivity),
    Readiness {
        source: TranscriptSource,
        turn: u64,
        notices: Vec<crate::CompletionNotice>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptProjection {
    pub items: Vec<TranscriptItem>,
}

impl TranscriptItem {
    pub fn source(&self) -> Option<TranscriptSource> {
        match self {
            Self::User(message) | Self::Assistant(message) => message.source,
            Self::ActivityGroup(group) => Some(group.source),
            Self::UserShell(shell) => Some(shell.source),
            Self::Readiness { source, .. } => Some(*source),
        }
    }

    /// Semantic finality, not physical publication. Inferred uncertainty alone
    /// cannot settle an activity; only a recorded outcome can do that.
    pub fn is_sealed(&self) -> bool {
        match self {
            Self::User(message) | Self::Assistant(message) => message.source.is_some(),
            Self::ActivityGroup(group) => {
                !group.open && group.activities.iter().all(activity_is_settled)
            }
            Self::UserShell(shell) => shell.outcome.is_some(),
            Self::Readiness { .. } => true,
        }
    }
}

fn activity_is_settled(activity: &TranscriptActivity) -> bool {
    activity.result.is_some() && activity.children.iter().all(activity_is_settled)
}

impl TranscriptProjection {
    /// Largest contiguous prefix whose items cannot change within this source.
    pub fn sealed_prefix(&self) -> usize {
        self.items
            .iter()
            .take_while(|item| item.is_sealed())
            .count()
    }

    pub fn from_session(view: &SessionView) -> Self {
        let mut builder = TranscriptBuilder::default();
        let mut shells = HashMap::new();
        let selected: std::collections::BTreeSet<_> = view.context_path.iter().copied().collect();
        for (entry_index, entry) in view.entries.iter().enumerate() {
            if !selected.contains(&(entry_index as u64 + 1)) {
                continue;
            }
            let entry_coordinate = entry_index as u64 + 1;
            let source = TranscriptSource {
                entry: entry_coordinate,
                part: 0,
            };
            match entry {
                SessionEntry::TurnStarted {
                    turn,
                    input,
                    completion,
                    ..
                } => {
                    if completion.is_empty() {
                        builder.push_user(entry_coordinate, Some(*turn), false, input);
                    } else {
                        builder.close_group();
                        builder.projection.items.push(TranscriptItem::Readiness {
                            source,
                            turn: *turn,
                            notices: completion.clone(),
                        });
                    }
                }
                SessionEntry::Completion { turn, notices } => {
                    builder.close_group();
                    builder.projection.items.push(TranscriptItem::Readiness {
                        source,
                        turn: *turn,
                        notices: notices.clone(),
                    });
                }
                SessionEntry::Steering { turn, input } => {
                    builder.push_user(entry_coordinate, Some(*turn), true, input);
                }
                SessionEntry::Assistant {
                    turn,
                    message,
                    tool_activities,
                    termination,
                    ..
                } => {
                    builder.push_assistant(
                        entry_coordinate,
                        *turn,
                        &message.content,
                        tool_activities,
                        termination,
                    );
                }
                SessionEntry::ChildToolAdmitted { intent, .. } => builder.push_child(intent),
                SessionEntry::ChildToolResult {
                    parent,
                    child,
                    outcome,
                    ..
                } => builder.push_child_result(*parent, *child, outcome),
                SessionEntry::ToolResult { turn, result } => {
                    builder.push_outcome(*turn, &result.call_id, &result.outcome);
                }
                SessionEntry::UserShellAdmitted {
                    command,
                    exclude_from_context,
                } => {
                    builder.close_group();
                    shells.insert(entry_coordinate, builder.projection.items.len());
                    builder
                        .projection
                        .items
                        .push(TranscriptItem::UserShell(UserShellActivity {
                            source,
                            command: command.clone(),
                            outcome: None,
                            exclude_from_context: *exclude_from_context,
                        }));
                }
                SessionEntry::UserShellSettled {
                    admission_entry,
                    outcome,
                } => {
                    let index = shells[admission_entry];
                    let TranscriptItem::UserShell(shell) = &mut builder.projection.items[index]
                    else {
                        unreachable!("shell admission index points to its activity");
                    };
                    shell.outcome = Some(outcome.clone());
                }
                SessionEntry::TurnEnded { .. } => builder.close_group(),
                SessionEntry::ContextSelected { .. }
                | SessionEntry::ModelSelected { .. }
                | SessionEntry::ReasoningSelected { .. }
                | SessionEntry::EffectiveModelChanged { .. }
                | SessionEntry::ProviderReplayRebased { .. }
                | SessionEntry::ModelContextChanged { .. }
                | SessionEntry::TurnResumed { .. }
                | SessionEntry::CacheWarm { .. }
                | SessionEntry::Compacted { .. }
                | SessionEntry::DelegationAdmitted { .. }
                | SessionEntry::DelegationTransferred { .. }
                | SessionEntry::DelegationSettled { .. }
                | SessionEntry::ChildSessionOf { .. } => {}
            }
        }
        for item in &mut builder.projection.items {
            if let TranscriptItem::ActivityGroup(group) = item {
                for activity in &mut group.activities {
                    if activity.state == ActivityState::Queued {
                        activity.state = ActivityState::Unknown;
                    }
                    for child in &mut activity.children {
                        if child.state == ActivityState::Queued {
                            child.state = ActivityState::Unknown;
                        }
                    }
                }
            }
        }
        builder.projection
    }
}

/// Shared ordering and grouping of committed facts. Streaming text never enters
/// this call index; only the live projection owns a removable provisional tail.
#[derive(Debug, Default)]
struct TranscriptBuilder {
    projection: TranscriptProjection,
    active_group: Option<(u64, usize)>,
    calls: HashMap<(u64, String), (usize, usize)>,
    occurrences: HashMap<crate::ToolOccurrence, (usize, usize)>,
}

impl TranscriptBuilder {
    fn push_user(&mut self, entry: u64, turn: Option<u64>, steering: bool, input: &Message) {
        self.close_group();
        let mut source = None;
        let parts = input
            .content
            .iter()
            .enumerate()
            .filter_map(|(part, content)| {
                let visible = visible_part(content, false)?;
                source.get_or_insert(TranscriptSource { entry, part });
                Some(visible)
            })
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            self.projection
                .items
                .push(TranscriptItem::User(TranscriptMessage {
                    source,
                    turn,
                    steering,
                    parts,
                }));
        }
    }

    fn push_assistant(
        &mut self,
        entry: u64,
        turn: u64,
        content: &[Content],
        tool_activities: &[crate::StoredToolActivity],
        termination: &ResponseTermination,
    ) {
        let mut ordinal = 0;
        for (part_index, part) in content.iter().enumerate() {
            let source = TranscriptSource {
                entry,
                part: part_index,
            };
            let visible = visible_part(part, true);
            if let Some(part) = visible {
                self.close_group();
                self.projection
                    .items
                    .push(TranscriptItem::Assistant(TranscriptMessage {
                        source: Some(source),
                        turn: Some(turn),
                        steering: false,
                        parts: vec![part],
                    }));
            } else if let Content::ToolCall(call) = part {
                let group_index = self.ensure_group(turn, source);
                let occurrence = crate::ToolOccurrence {
                    assistant_entry: entry,
                    ordinal,
                };
                ordinal += 1;
                let stored = tool_activities.get(occurrence.ordinal).expect(
                    "Session validates ordered activity metadata before committing an assistant",
                );
                let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[group_index]
                else {
                    unreachable!("ensure_group returns an activity group");
                };
                let activity_index = group.activities.len();
                group.activities.push(TranscriptActivity {
                    identity: TranscriptActivityIdentity::Tool(occurrence),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    activity: stored.activity.clone(),
                    arguments: call.arguments.clone(),
                    state: if matches!(termination, ResponseTermination::Completed) {
                        ActivityState::Queued
                    } else {
                        ActivityState::Rejected
                    },
                    result: None,
                    children: Vec::new(),
                });
                self.calls
                    .insert((turn, call.id.clone()), (group_index, activity_index));
                self.occurrences
                    .insert(occurrence, (group_index, activity_index));
            }
        }
    }

    fn activity_mut(&mut self, turn: u64, call_id: &str) -> &mut TranscriptActivity {
        let &(group_index, activity_index) = self
            .calls
            .get(&(turn, call_id.to_owned()))
            .expect("Session commits the assistant call before its result");
        let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[group_index] else {
            unreachable!("call index points to an activity group");
        };
        &mut group.activities[activity_index]
    }

    fn push_outcome(&mut self, turn: u64, call_id: &str, outcome: &crate::ToolOutcome) {
        let output = outcome.inspection_output();
        let state = match outcome {
            crate::ToolOutcome::Observed { .. } => result_state(output.is_error, &output.value),
            crate::ToolOutcome::NotDispatched { .. } => ActivityState::Rejected,
            crate::ToolOutcome::Unknown => ActivityState::Unknown,
        };
        let activity = self.activity_mut(turn, call_id);
        activity.state = state;
        activity.result = Some(activity_result(
            output.value.clone(),
            &output.images,
            output.is_error,
            outcome.projection(),
        ));
    }

    fn parent_mut(&mut self, parent: crate::ToolOccurrence) -> &mut TranscriptActivity {
        let &(group, activity) = self
            .occurrences
            .get(&parent)
            .expect("committed parent precedes child facts");
        let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[group] else {
            unreachable!()
        };
        &mut group.activities[activity]
    }

    fn push_child(&mut self, intent: &crate::ChildIntent) {
        let parent = self.parent_mut(intent.parent);
        assert_eq!(parent.children.len(), intent.child);
        parent.children.push(TranscriptActivity {
            identity: TranscriptActivityIdentity::Child {
                parent: intent.parent,
                child: intent.child,
            },
            call_id: intent.call.id.clone(),
            name: intent.call.name.clone(),
            activity: intent.activity.clone(),
            arguments: intent.call.arguments.clone(),
            state: ActivityState::Queued,
            result: None,
            children: Vec::new(),
        });
    }

    fn push_child_result(
        &mut self,
        parent: crate::ToolOccurrence,
        child: usize,
        outcome: &crate::ChildOutcome,
    ) {
        let activity = &mut self.parent_mut(parent).children[child];
        let output = outcome.clone().inspection_output();
        activity.state = match outcome {
            crate::ChildOutcome::Observed { .. } => result_state(output.is_error, &output.value),
            crate::ChildOutcome::NotDispatched { .. } => ActivityState::Rejected,
            crate::ChildOutcome::Unknown => ActivityState::Unknown,
        };
        activity.result = Some(activity_result(
            output.value,
            &output.images,
            output.is_error,
            None,
        ));
    }

    fn ensure_group(&mut self, turn: u64, source: TranscriptSource) -> usize {
        if let Some((active_turn, index)) = self.active_group
            && active_turn == turn
        {
            return index;
        }
        self.close_group();
        let index = self.projection.items.len();
        self.projection
            .items
            .push(TranscriptItem::ActivityGroup(ActivityGroup {
                source,
                turn,
                activities: Vec::new(),
                open: true,
            }));
        self.active_group = Some((turn, index));
        index
    }

    fn close_group(&mut self) {
        if let Some((_, index)) = self.active_group.take()
            && let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[index]
        {
            group.open = false;
        }
    }
}

fn visible_part(content: &Content, assistant: bool) -> Option<TranscriptPart> {
    match content {
        Content::Thinking(text) if !text.is_empty() => Some(TranscriptPart::Thinking(text.clone())),
        Content::Text(text) if !(if assistant { text.trim() } else { text }).is_empty() => {
            Some(TranscriptPart::Text(text.clone()))
        }
        Content::Image(image) => Some(TranscriptPart::Image {
            mime_type: image.mime_type().as_str().to_owned(),
        }),
        _ => None,
    }
}

fn result_state(is_error: bool, value: &Value) -> ActivityState {
    if !is_error {
        return ActivityState::Completed;
    }
    if value
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ActivityState::Cancelled;
    }
    if value
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ActivityState::TimedOut;
    }
    ActivityState::Failed
}

fn activity_result(
    value: Value,
    images: &[ion_ai::ImageContent],
    is_error: bool,
    projection: Option<ToolResultProjection>,
) -> ActivityResult {
    ActivityResult {
        projection,
        value,
        image_mime_types: images
            .iter()
            .map(|image| image.mime_type().as_str().to_owned())
            .collect(),
        is_error,
    }
}

/// One active Turn's committed projection plus a removable streamed response.
/// Feed all events in order, starting with TurnAccepted. Commit events replace
/// provisional response; restart never removes committed assistant or steering.
#[derive(Debug, Default)]
pub struct LiveTranscript {
    revision: u64,
    turn: Option<u64>,
    builder: TranscriptBuilder,
    current_response: Option<usize>,
    notices: Vec<String>,
}

impl LiveTranscript {
    pub fn projection(&self) -> &TranscriptProjection {
        &self.builder.projection
    }

    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Observation revision within this instance, for presentation invalidation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Index of the uncommitted assistant tail in this live projection.
    pub fn provisional_item_index(&self) -> Option<usize> {
        self.current_response
    }

    /// Resolve an original streamed part to its committed content coordinate.
    /// Ordered matching preserves duplicate parts and skips non-visible content;
    /// a changed or discarded stream has no corresponding source point.
    pub fn committed_part_source(
        &self,
        event: &AgentEvent,
        part: usize,
    ) -> Option<TranscriptSource> {
        let AgentEvent::AssistantCommitted {
            entry,
            turn,
            content,
            ..
        } = event
        else {
            return None;
        };
        if self.turn != Some(*turn) {
            return None;
        }
        let TranscriptItem::Assistant(message) =
            &self.builder.projection.items[self.current_response?]
        else {
            unreachable!("provisional response is an assistant message")
        };
        message.parts.get(part)?;
        let mut committed = content
            .iter()
            .enumerate()
            .filter_map(|(index, content)| visible_part(content, true).map(|part| (index, part)));
        let mut source = None;
        for streamed in &message.parts[..=part] {
            let (part, _) = committed.by_ref().find(|(_, value)| value == streamed)?;
            source = Some(TranscriptSource {
                entry: *entry,
                part,
            });
        }
        source
    }

    pub fn observe(&mut self, event: &AgentEvent) {
        self.revision += 1;
        match event {
            AgentEvent::TurnAccepted { turn, entry, input } => {
                self.turn = Some(*turn);
                self.builder.push_user(*entry, Some(*turn), false, input);
            }
            AgentEvent::CompletionCommitted {
                turn,
                entry,
                notices,
                ..
            } => {
                self.turn = Some(*turn);
                self.builder.close_group();
                self.builder
                    .projection
                    .items
                    .push(TranscriptItem::Readiness {
                        source: TranscriptSource {
                            entry: *entry,
                            part: 0,
                        },
                        turn: *turn,
                        notices: notices.clone(),
                    });
            }
            AgentEvent::TurnResumed { point } => {
                self.turn = Some(point.turn);
                self.note("Interrupted Turn continued; reconcile committed history".into());
            }
            AgentEvent::ModelOutputObserved => {}
            AgentEvent::TextDelta(text) => self.push_text(text),
            AgentEvent::ThinkingDelta { block, text } => self.push_thinking(*block, text),
            AgentEvent::AssistantCommitted {
                turn,
                entry,
                content,
                tool_activities,
                termination,
            } => {
                self.restart_partial_response();
                self.builder
                    .push_assistant(*entry, *turn, content, tool_activities, termination);
            }
            AgentEvent::SteeringCommitted { turn, entry, input } => {
                self.builder.push_user(*entry, Some(*turn), true, input);
            }
            AgentEvent::ProviderRetry {
                attempt,
                max_retries,
                delay_ms,
            } => self.note(format!(
                "Provider retry {attempt}/{max_retries} in {delay_ms}ms"
            )),
            AgentEvent::ContextCompactionRetry {
                next_through_entry, ..
            } => self.note(format!(
                "Summary request too large; retrying through entry {next_through_entry}"
            )),
            AgentEvent::ContextCompacted { through_entry } => {
                self.note(format!("Context summarized through entry {through_entry}"));
            }
            AgentEvent::ProviderReplayRebased => {
                self.note("Provider reasoning context reset".into());
            }
            AgentEvent::ProviderReplayNotice {
                action,
                reason,
                count,
            } => self.note(format!(
                "Provider reasoning {action}: {count} block(s), {reason}"
            )),
            AgentEvent::ResponseRestarted => {
                self.restart_partial_response();
                self.note("Incomplete response discarded; retrying".into());
            }
            AgentEvent::ToolCatalogWarning(message) => {
                self.note(format!("Tool catalog: {message}"));
            }
            // Call content and metadata are already projected by the commit.
            // Start is execution progress, not another authoritative call.
            AgentEvent::ToolStarted { call_id, .. } => {
                let turn = self.turn.expect("tool progress follows TurnAccepted");
                let activity = self.builder.activity_mut(turn, call_id);
                assert_eq!(activity.state, ActivityState::Queued);
                activity.state = ActivityState::Running;
            }
            AgentEvent::ToolFinished {
                call_id, outcome, ..
            } => {
                let turn = self.turn.expect("tool progress follows TurnAccepted");
                self.builder.push_outcome(turn, call_id, outcome);
            }
            AgentEvent::ChildToolAdmitted { intent, .. } => self.builder.push_child(intent),
            AgentEvent::ChildToolStarted { parent, child } => {
                let activity = &mut self.builder.parent_mut(*parent).children[*child];
                assert_eq!(activity.state, ActivityState::Queued);
                activity.state = ActivityState::Running;
            }
            AgentEvent::ChildToolFinished {
                parent,
                child,
                outcome,
            } => self.builder.push_child_result(*parent, *child, outcome),
            AgentEvent::TurnEnded { .. } => self.builder.close_group(),
            AgentEvent::InterruptedCalls(count) => self.note(format!(
                "{count} previous tool call(s) had unknown effects; inspect before retrying"
            )),
            AgentEvent::Final(_) => self.builder.close_group(),
        }
    }

    fn partial_response(&mut self) -> &mut TranscriptMessage {
        if self.current_response.is_none() {
            let index = self.builder.projection.items.len();
            self.builder
                .projection
                .items
                .push(TranscriptItem::Assistant(TranscriptMessage {
                    source: None,
                    turn: self.turn,
                    steering: false,
                    parts: Vec::new(),
                }));
            self.current_response = Some(index);
        }
        let index = self
            .current_response
            .expect("partial response was inserted");
        let TranscriptItem::Assistant(message) = &mut self.builder.projection.items[index] else {
            unreachable!("provisional response is an assistant message");
        };
        message
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let message = self.partial_response();
        if let Some(TranscriptPart::Text(current)) = message
            .parts
            .iter_mut()
            .find(|part| matches!(part, TranscriptPart::Text(_)))
        {
            current.push_str(text);
        } else {
            message.parts.push(TranscriptPart::Text(text.to_owned()));
        }
    }

    fn push_thinking(&mut self, block: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let message = self.partial_response();
        let count = message
            .parts
            .iter()
            .filter(|part| matches!(part, TranscriptPart::Thinking(_)))
            .count();
        assert!(
            block <= count,
            "human thinking block indices are consecutive"
        );
        if block == count {
            message
                .parts
                .push(TranscriptPart::Thinking(text.to_owned()));
        } else if let Some(TranscriptPart::Thinking(current)) = message
            .parts
            .iter_mut()
            .filter(|part| matches!(part, TranscriptPart::Thinking(_)))
            .nth(block)
        {
            current.push_str(text);
        }
    }

    fn restart_partial_response(&mut self) {
        if let Some(index) = self.current_response.take() {
            assert_eq!(
                index + 1,
                self.builder.projection.items.len(),
                "provisional response is always the last transcript item"
            );
            self.builder.projection.items.pop();
        }
    }

    fn note(&mut self, message: String) {
        self.notices.push(message);
        if self.notices.len() > 16 {
            self.notices.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_result::ToolOutput;
    use ion_ai::{
        ModelExecution, ModelRef, ModelRoute, ModelRouteReason, Role, ToolCall, ToolResult, Usage,
    };

    use crate::{ToolActivityKind, TurnEndReason};

    fn assistant(turn: u64, content: Vec<Content>) -> SessionEntry {
        let tool_activities = content
            .iter()
            .filter_map(|part| match part {
                Content::ToolCall(call) => {
                    let kind = match call.name.as_str() {
                        "read" => ToolActivityKind::Read,
                        "edit" => ToolActivityKind::Edit,
                        "exec" => ToolActivityKind::Command,
                        _ => ToolActivityKind::External,
                    };
                    let subject = match call.name.as_str() {
                        "exec" => call.arguments.get("command"),
                        _ => call.arguments.get("path"),
                    }
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                    Some(crate::StoredToolActivity {
                        call_id: call.id.clone(),
                        activity: ToolActivity { kind, subject },
                    })
                }
                _ => None,
            })
            .collect();
        SessionEntry::Assistant {
            turn,
            message: Message {
                role: Role::Assistant,
                content,
                provider_replay: None,
            },
            tool_activities,
            execution: ModelExecution {
                route: ModelRoute::direct(
                    ModelRef {
                        provider: "test".into(),
                        model: "model".into(),
                    },
                    ModelRouteReason::UserRequest,
                ),
                returned_model: None,
            },
            usage: Usage::unknown(),
            termination: ResponseTermination::Completed,
        }
    }

    fn call(id: &str, name: &str, arguments: Value) -> Content {
        Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
            raw_arguments: None,
        })
    }

    #[test]
    fn historical_projection_groups_silent_tool_steps_and_splits_on_prose() {
        let model = ModelRef {
            provider: "test".into(),
            model: "model".into(),
        };
        let view = SessionView {
            id: Default::default(),
            cwd: Some("/tmp".into()),
            name: None,
            entries: vec![
                SessionEntry::TurnStarted {
                    turn: 1,
                    input: Message::user_input("inspect".into(), std::iter::empty()),
                    model,
                    completion: Vec::new(),
                    request: None,
                    recovery: None,
                },
                assistant(
                    1,
                    vec![call("a", "read", serde_json::json!({"path":"a.rs"}))],
                ),
                SessionEntry::ToolResult {
                    turn: 1,
                    result: crate::ToolSettlement::observed(
                        ToolResult {
                            call_id: "a".into(),
                            name: "read".into(),
                            result: serde_json::json!({"path":"a.rs"}),
                            images: Vec::new(),
                            is_error: false,
                        },
                        crate::ToolResultProjection::Observed,
                    ),
                },
                assistant(
                    1,
                    vec![call("b", "edit", serde_json::json!({"path":"b.rs"}))],
                ),
                SessionEntry::ToolResult {
                    turn: 1,
                    result: crate::ToolSettlement::observed(
                        ToolResult {
                            call_id: "b".into(),
                            name: "edit".into(),
                            result: serde_json::json!({"path":"b.rs","replacements":1}),
                            images: Vec::new(),
                            is_error: false,
                        },
                        crate::ToolResultProjection::Observed,
                    ),
                },
                assistant(
                    1,
                    vec![
                        Content::Text("I found the issue.".into()),
                        call("c", "read", serde_json::json!({"path":"c.rs"})),
                    ],
                ),
                SessionEntry::ToolResult {
                    turn: 1,
                    result: crate::ToolSettlement::observed(
                        ToolResult {
                            call_id: "c".into(),
                            name: "read".into(),
                            result: serde_json::json!({"path":"c.rs"}),
                            images: Vec::new(),
                            is_error: false,
                        },
                        crate::ToolResultProjection::Observed,
                    ),
                },
                assistant(1, vec![Content::Text("Done.".into())]),
                SessionEntry::TurnEnded {
                    turn: 1,
                    reason: TurnEndReason::Completed,
                },
            ],
            context_path: (1..=9).collect(),
            messages: Vec::new(),
            unfinished_turn: None,
            unfinished_user_shell: None,
            last_end: None,
            last_model: None,
            reasoning: ion_ai::Reasoning::ProviderDefault,
            last_effective_model: None,
            last_context: None,
            compacted_through: None,
            last_execution: None,
            last_usage: None,
        };

        let projected = TranscriptProjection::from_session(&view);
        let groups = projected
            .items
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::ActivityGroup(group) => Some(group),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].activities.len(), 2);
        assert_eq!(
            groups[0].activities[0].activity.kind,
            ToolActivityKind::Read
        );
        assert_eq!(
            groups[0].activities[1].activity.kind,
            ToolActivityKind::Edit
        );
        assert_eq!(groups[1].activities.len(), 1);
        assert_eq!(groups[0].source, TranscriptSource { entry: 2, part: 0 });
        assert_eq!(groups[1].source, TranscriptSource { entry: 6, part: 1 });
        assert_eq!(projected.sealed_prefix(), projected.items.len());
        assert!(projected.items.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::Assistant(TranscriptMessage { parts, .. })
                    if parts == &vec![TranscriptPart::Text("I found the issue.".into())]
            )
        }));
    }

    #[test]
    fn passive_uncertainty_is_not_sealed_but_recorded_unknown_is() {
        let root =
            std::env::temp_dir().join(format!("ion-transcript-seal-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = crate::Session::create(&path, Some(&root)).unwrap();
        let (turn, _) = session
            .begin_turn(
                "inspect".into(),
                ModelRef {
                    provider: "test".into(),
                    model: "model".into(),
                },
            )
            .unwrap();
        let record = |content| {
            let SessionEntry::Assistant { message, .. } = assistant(turn, content) else {
                unreachable!()
            };
            session
                .record_assistant(turn, message, Usage::unknown(), false)
                .unwrap();
        };
        record(vec![call("same", "read", serde_json::json!({}))]);
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: "same".into(),
                    name: "read".into(),
                    result: Value::Null,
                    images: vec![],
                    is_error: false,
                },
            )
            .unwrap();
        let growing = TranscriptProjection::from_session(&session.view().unwrap());
        assert_eq!(
            growing.sealed_prefix(),
            1,
            "observed calls can still acquire silent siblings"
        );
        assert!(matches!(
            &growing.items[1],
            TranscriptItem::ActivityGroup(ActivityGroup { open: true, .. })
        ));

        // Reused provider IDs cannot replace the first committed occurrence.
        record(vec![
            Content::Text("  ".into()),
            call("same", "read", serde_json::json!({})),
            Content::Text("pending work".into()),
        ]);
        let parent = session.tool_occurrence(turn, "same").unwrap();
        session
            .record_child_intent(
                turn,
                crate::ChildIntent {
                    parent,
                    child: 0,
                    call: ToolCall {
                        id: "child".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                        raw_arguments: None,
                    },
                    definition: ion_ai::ToolSpec {
                        name: "read".into(),
                        description: String::new(),
                        input_schema: serde_json::json!({"type":"object"}),
                    },
                    activity: ToolActivity::external("read"),
                },
            )
            .unwrap();
        let pending = TranscriptProjection::from_session(&session.view().unwrap());
        let TranscriptItem::ActivityGroup(group) = &pending.items[1] else {
            panic!()
        };
        assert!(!group.open);
        assert_eq!(group.source, growing.items[1].source().unwrap());
        assert_ne!(group.activities[0].identity, group.activities[1].identity);
        assert_eq!(
            group.activities[1].identity,
            TranscriptActivityIdentity::Tool(parent)
        );
        assert_eq!(group.activities[1].state, ActivityState::Unknown);
        assert!(group.activities[1].result.is_none());
        assert_eq!(
            group.activities[1].children[0].identity,
            TranscriptActivityIdentity::Child { parent, child: 0 }
        );
        assert!(group.activities[1].children[0].result.is_none());
        assert_eq!(
            pending.sealed_prefix(),
            1,
            "committed prose closes but cannot settle a group"
        );
        assert!(
            pending.items[2].is_sealed(),
            "later stable items cannot leap over an unresolved group"
        );
        drop(session);
        let session = crate::Session::open(&path).unwrap();
        assert_eq!(
            TranscriptProjection::from_session(&session.view().unwrap()),
            pending
        );
        session
            .end_turn(turn, TurnEndReason::Cancelled, &Default::default())
            .unwrap();
        let settled = TranscriptProjection::from_session(&session.view().unwrap());
        assert_eq!(settled.sealed_prefix(), settled.items.len());
        let TranscriptItem::ActivityGroup(group) = &settled.items[1] else {
            panic!()
        };
        assert_eq!(group.activities[1].state, ActivityState::Unknown);
        assert!(group.activities[1].result.is_some());
        assert!(group.activities[1].children[0].result.is_some());
        assert_eq!(settled.items[1].source(), pending.items[1].source());
        drop(session);
        let reopened = crate::Session::open(&path).unwrap();
        assert_eq!(
            TranscriptProjection::from_session(&reopened.view().unwrap()),
            settled
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn committed(entry: u64, turn: u64, content: Vec<Content>) -> AgentEvent {
        let SessionEntry::Assistant {
            message,
            tool_activities,
            termination,
            ..
        } = assistant(turn, content)
        else {
            unreachable!()
        };
        AgentEvent::AssistantCommitted {
            turn,
            entry,
            content: message.content,
            tool_activities,
            termination,
        }
    }

    #[test]
    fn execution_start_changes_only_the_committed_call() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            turn: 1,
            entry: 1,
            input: Message::user_input("request".into(), []),
        });
        live.observe(&committed(
            2,
            1,
            vec![
                call("first", "read", serde_json::json!({"path":"a"})),
                call("second", "read", serde_json::json!({"path":"b"})),
            ],
        ));
        live.observe(&AgentEvent::ToolStarted {
            call_id: "first".into(),
            name: "read".into(),
            // Start cannot overwrite committed arguments or metadata.
            arguments: serde_json::json!({"path":"not-a"}),
            activity: ToolActivity::external("not-read"),
        });
        let TranscriptItem::ActivityGroup(group) = &live.projection().items[1] else {
            panic!()
        };
        assert_eq!(group.activities[0].state, ActivityState::Running);
        assert_eq!(group.activities[0].arguments["path"], "a");
        assert_eq!(group.activities[1].state, ActivityState::Queued);
        live.observe(&AgentEvent::ToolFinished {
            call_id: "first".into(),
            name: "read".into(),
            activity: ToolActivity::external("read"),
            outcome: crate::ToolOutcome::Observed {
                projection: crate::ToolResultProjection::Observed,
                output: ToolOutput {
                    value: serde_json::json!({"content":"observed"}),
                    images: vec![],
                    is_error: false,
                },
            },
        });
        let TranscriptItem::ActivityGroup(group) = &live.projection().items[1] else {
            panic!()
        };
        assert_eq!(group.activities[0].state, ActivityState::Completed);
        assert_eq!(group.activities[1].state, ActivityState::Queued);
    }

    #[test]
    fn provisional_prose_never_seals_a_growing_group_even_before_restart() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            entry: 20,
            turn: 7,
            input: Message::user_input("request".into(), []),
        });
        live.observe(&committed(
            21,
            7,
            vec![call(
                "read-1",
                "read",
                serde_json::json!({"path":"src/lib.rs"}),
            )],
        ));
        live.observe(&AgentEvent::ToolFinished {
            call_id: "read-1".into(),
            name: "read".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("src/lib.rs".into()),
            },
            outcome: crate::ToolOutcome::Observed {
                projection: crate::ToolResultProjection::Observed,
                output: ToolOutput {
                    value: serde_json::json!({"path":"src/lib.rs"}),
                    images: Vec::new(),
                    is_error: false,
                },
            },
        });
        let source = live.projection().items[1].source();
        assert_eq!(source, Some(TranscriptSource { entry: 21, part: 0 }));
        assert_eq!(live.projection().sealed_prefix(), 1);
        live.observe(&AgentEvent::TextDelta("discard me".into()));
        assert!(!live.projection().items[1].is_sealed());
        assert!(!live.projection().items[2].is_sealed());
        assert_eq!(live.projection().items[2].source(), None);
        assert_eq!(live.projection().sealed_prefix(), 1);
        live.observe(&AgentEvent::ResponseRestarted);
        assert_eq!(live.projection().sealed_prefix(), 1);
        live.observe(&committed(
            23,
            7,
            vec![call(
                "read-2",
                "read",
                serde_json::json!({"path":"src/main.rs"}),
            )],
        ));
        // Progress must not duplicate the already committed call.
        live.observe(&AgentEvent::ToolStarted {
            call_id: "read-2".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path":"src/main.rs"}),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("src/main.rs".into()),
            },
        });
        assert_eq!(live.projection().items.len(), 2);
        let TranscriptItem::ActivityGroup(group) = &live.projection().items[1] else {
            panic!("expected one activity group");
        };
        assert!(group.open);
        assert_eq!(group.activities.len(), 2);
        assert_eq!(group.activities[0].state, ActivityState::Completed);
        assert_eq!(Some(group.source), source);
        assert_ne!(group.activities[0].identity, group.activities[1].identity);
        assert!(!live.projection().items[1].is_sealed());
    }

    #[test]
    fn streamed_parts_resolve_ordered_duplicates_to_actual_committed_coordinates() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            turn: 1,
            entry: 1,
            input: Message::user_input("request".into(), []),
        });
        for block in 0..2 {
            live.observe(&AgentEvent::ThinkingDelta {
                block,
                text: "same".into(),
            });
        }
        live.observe(&AgentEvent::TextDelta("same".into()));
        let event = committed(
            4,
            1,
            vec![
                Content::Text(" ".into()),
                Content::Thinking("same".into()),
                call("read", "read", serde_json::json!({})),
                Content::Thinking("same".into()),
                Content::Text("same".into()),
            ],
        );
        for (streamed, part) in [1, 3, 4].into_iter().enumerate() {
            assert_eq!(
                live.committed_part_source(&event, streamed),
                Some(TranscriptSource { entry: 4, part })
            );
        }
        assert_eq!(live.committed_part_source(&event, 3), None);
        assert_eq!(
            live.committed_part_source(&committed(4, 2, vec![Content::Text("same".into())]), 0),
            None
        );
        assert_eq!(
            live.committed_part_source(&committed(4, 1, vec![Content::Text("changed".into())]), 0),
            None
        );
        live.observe(&AgentEvent::ResponseRestarted);
        assert_eq!(live.committed_part_source(&event, 0), None);
    }

    #[test]
    fn thinking_and_prose_share_response_restart_and_commit_custody() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            turn: 1,
            entry: 1,
            input: Message::user_input("request".into(), []),
        });
        live.observe(&committed(2, 1, vec![Content::Text("kept".into())]));
        for (block, text) in [(0, "first "), (0, "thought"), (1, "summary")] {
            live.observe(&AgentEvent::ThinkingDelta {
                block,
                text: text.into(),
            });
        }
        live.observe(&AgentEvent::TextDelta("provisional".into()));
        let index = live.provisional_item_index().unwrap();
        let TranscriptItem::Assistant(message) = &live.projection().items[index] else {
            panic!()
        };
        assert_eq!(
            message.parts,
            vec![
                TranscriptPart::Thinking("first thought".into()),
                TranscriptPart::Thinking("summary".into()),
                TranscriptPart::Text("provisional".into())
            ]
        );
        live.observe(&AgentEvent::ResponseRestarted);
        assert!(live.provisional_item_index().is_none());
        assert_eq!(live.projection().items.len(), 2);
        live.observe(&AgentEvent::ThinkingDelta {
            block: 0,
            text: "new thought".into(),
        });
        live.observe(&committed(
            3,
            1,
            vec![
                Content::Thinking("recorded thought".into()),
                Content::Text("answer".into()),
            ],
        ));
        assert!(live.provisional_item_index().is_none());
        assert_eq!(live.projection().items.len(), 4);
        let TranscriptItem::Assistant(message) = &live.projection().items[2] else {
            panic!()
        };
        assert_eq!(
            message.parts,
            vec![TranscriptPart::Thinking("recorded thought".into())]
        );
    }

    #[test]
    fn live_committed_text_splits_the_next_tool_group() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            entry: 10,
            turn: 3,
            input: Message::user_input("request".into(), []),
        });
        live.observe(&committed(
            11,
            3,
            vec![call("one", "read", serde_json::json!({"path":"one.rs"}))],
        ));
        live.observe(&AgentEvent::ToolFinished {
            call_id: "one".into(),
            name: "read".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("one.rs".into()),
            },
            outcome: crate::ToolOutcome::Observed {
                projection: crate::ToolResultProjection::Observed,
                output: ToolOutput {
                    value: serde_json::json!({}),
                    images: Vec::new(),
                    is_error: false,
                },
            },
        });
        live.observe(&AgentEvent::TextDelta("provisional".into()));
        live.observe(&committed(
            13,
            3,
            vec![
                Content::Text("Now editing.".into()),
                call("two", "edit", serde_json::json!({"path":"two.rs"})),
            ],
        ));
        assert_eq!(live.projection().items.len(), 4);
        assert!(matches!(
            &live.projection().items[1],
            TranscriptItem::ActivityGroup(ActivityGroup { open: false, .. })
        ));
        assert!(matches!(
            &live.projection().items[2],
            TranscriptItem::Assistant(TranscriptMessage { parts, .. })
                if parts == &vec![TranscriptPart::Text("Now editing.".into())]
        ));
        assert!(matches!(
            &live.projection().items[3],
            TranscriptItem::ActivityGroup(ActivityGroup { open: true, .. })
        ));
    }
}

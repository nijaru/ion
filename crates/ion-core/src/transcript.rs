//! Typed coding transcript projection over durable Session facts and live agent events.
use std::collections::HashMap;

use ion_ai::{Content, Message, ResponseTermination};
use serde_json::Value;

use crate::{
    agent::AgentEvent,
    session::{SessionEntry, SessionView},
    tool_result::{ToolOutput, ToolResultProjection},
    tool_set::ToolActivity,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptPart {
    Text(String),
    Image { mime_type: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMessage {
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
    pub projection: ToolResultProjection,
    pub value: Value,
    pub image_mime_types: Vec<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptActivity {
    pub call_id: String,
    pub name: String,
    pub activity: ToolActivity,
    pub arguments: Value,
    pub state: ActivityState,
    pub result: Option<ActivityResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityGroup {
    pub turn: u64,
    pub activities: Vec<TranscriptActivity>,
    /// Whether subsequent silent calls can join this presentation group.
    /// Closed groups can still have pending outcomes; this is not immutability.
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserShellActivity {
    pub command: String,
    pub output: Value,
    pub is_error: bool,
    pub exclude_from_context: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User(TranscriptMessage),
    Assistant(TranscriptMessage),
    ActivityGroup(ActivityGroup),
    UserShell(UserShellActivity),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptProjection {
    pub items: Vec<TranscriptItem>,
}

impl TranscriptProjection {
    pub fn from_session(view: &SessionView) -> Self {
        let mut builder = TranscriptBuilder::default();
        for entry in &view.entries {
            match entry {
                SessionEntry::TurnStarted { turn, input, .. } => {
                    builder.push_user(Some(*turn), false, input);
                }
                SessionEntry::Steering { turn, input } => {
                    builder.push_user(Some(*turn), true, input);
                }
                SessionEntry::Assistant {
                    turn,
                    message,
                    tool_activities,
                    termination,
                    ..
                } => builder.push_assistant(
                    *turn,
                    &message.content,
                    tool_activities,
                    termination,
                    false,
                ),
                SessionEntry::ToolResult {
                    turn,
                    result,
                    projection,
                } => {
                    builder.push_result(
                        *turn,
                        &result.call_id,
                        ActivityResult {
                            projection: *projection,
                            value: result.result.clone(),
                            image_mime_types: result
                                .images
                                .iter()
                                .map(|image| image.mime_type().as_str().to_owned())
                                .collect(),
                            is_error: result.is_error,
                        },
                    );
                }
                SessionEntry::UserShell {
                    command,
                    output,
                    is_error,
                    exclude_from_context,
                } => {
                    builder.close_group();
                    builder
                        .projection
                        .items
                        .push(TranscriptItem::UserShell(UserShellActivity {
                            command: command.clone(),
                            output: output.clone(),
                            is_error: *is_error,
                            exclude_from_context: *exclude_from_context,
                        }));
                }
                SessionEntry::TurnEnded { .. } => builder.close_group(),
                SessionEntry::ModelSelected { .. }
                | SessionEntry::EffectiveModelChanged { .. }
                | SessionEntry::ProviderReplayRebased { .. }
                | SessionEntry::ModelContextChanged { .. }
                | SessionEntry::CacheWarm { .. }
                | SessionEntry::Compacted { .. } => {}
            }
        }
        for item in &mut builder.projection.items {
            if let TranscriptItem::ActivityGroup(group) = item {
                group.open = false;
                for activity in &mut group.activities {
                    if activity.state == ActivityState::Queued {
                        activity.state = ActivityState::Unknown;
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
}

impl TranscriptBuilder {
    fn push_user(&mut self, turn: Option<u64>, steering: bool, input: &Message) {
        self.close_group();
        let parts = visible_parts(input);
        if !parts.is_empty() {
            self.projection
                .items
                .push(TranscriptItem::User(TranscriptMessage {
                    turn,
                    steering,
                    parts,
                }));
        }
    }

    fn push_assistant(
        &mut self,
        turn: u64,
        content: &[Content],
        tool_activities: &[crate::StoredToolActivity],
        termination: &ResponseTermination,
        open: bool,
    ) {
        for part in content {
            let visible = match part {
                Content::Text(text) if !text.trim().is_empty() => {
                    Some(TranscriptPart::Text(text.clone()))
                }
                Content::Image(image) => Some(TranscriptPart::Image {
                    mime_type: image.mime_type().as_str().to_owned(),
                }),
                _ => None,
            };
            if let Some(part) = visible {
                self.close_group();
                self.projection
                    .items
                    .push(TranscriptItem::Assistant(TranscriptMessage {
                        turn: Some(turn),
                        steering: false,
                        parts: vec![part],
                    }));
            } else if let Content::ToolCall(call) = part {
                let group_index = self.ensure_group(turn, open);
                let stored = tool_activities
                    .iter()
                    .find(|stored| stored.call_id == call.id)
                    .expect("Session validates activity metadata before committing an assistant");
                let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[group_index]
                else {
                    unreachable!("ensure_group returns an activity group");
                };
                let activity_index = group.activities.len();
                group.activities.push(TranscriptActivity {
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
                });
                self.calls
                    .insert((turn, call.id.clone()), (group_index, activity_index));
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

    fn push_result(&mut self, turn: u64, call_id: &str, result: ActivityResult) {
        let activity = self.activity_mut(turn, call_id);
        if activity.state != ActivityState::Rejected {
            activity.state = result_state(result.is_error, &result.value);
        }
        activity.result = Some(result);
    }

    fn ensure_group(&mut self, turn: u64, open: bool) -> usize {
        if let Some((active_turn, index)) = self.active_group
            && active_turn == turn
        {
            if let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[index] {
                group.open |= open;
            }
            return index;
        }
        self.close_group();
        let index = self.projection.items.len();
        self.projection
            .items
            .push(TranscriptItem::ActivityGroup(ActivityGroup {
                turn,
                activities: Vec::new(),
                open,
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

fn visible_parts(message: &Message) -> Vec<TranscriptPart> {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text(text) if !text.is_empty() => Some(TranscriptPart::Text(text.clone())),
            Content::Image(image) => Some(TranscriptPart::Image {
                mime_type: image.mime_type().as_str().to_owned(),
            }),
            Content::Text(_) | Content::ToolCall(_) | Content::ToolResult(_) => None,
        })
        .collect()
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

fn live_result(output: ToolOutput, projection: ToolResultProjection) -> ActivityResult {
    ActivityResult {
        projection,
        value: output.value,
        image_mime_types: output
            .images
            .iter()
            .map(|image| image.mime_type().as_str().to_owned())
            .collect(),
        is_error: output.is_error,
    }
}

/// One active Turn's committed projection plus a removable streamed response.
/// Feed all events in order, starting with TurnAccepted. Commit events replace
/// provisional text; restart never removes committed assistant or steering.
#[derive(Debug, Default)]
pub struct LiveTranscript {
    revision: u64,
    turn: Option<u64>,
    builder: TranscriptBuilder,
    current_text: Option<usize>,
    group_closed_for_partial_text: Option<(u64, usize)>,
    notices: Vec<String>,
}

impl LiveTranscript {
    pub fn with_user_input(input: &Message) -> Self {
        let mut live = Self::default();
        live.builder.push_user(None, false, input);
        live
    }

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
        self.current_text
    }

    pub fn observe(&mut self, event: AgentEvent) {
        self.revision += 1;
        match event {
            AgentEvent::TurnAccepted { turn } => {
                self.turn = Some(turn);
                for item in &mut self.builder.projection.items {
                    if let TranscriptItem::User(message) = item
                        && message.turn.is_none()
                    {
                        message.turn = Some(turn);
                    }
                }
            }
            AgentEvent::TextDelta(text) => self.push_text(text),
            AgentEvent::AssistantCommitted {
                turn,
                content,
                tool_activities,
                termination,
            } => {
                self.restart_partial_response();
                self.builder
                    .push_assistant(turn, &content, &tool_activities, &termination, true);
            }
            AgentEvent::SteeringCommitted { turn, input } => {
                self.builder.push_user(Some(turn), true, &input);
            }
            AgentEvent::ProviderRetry {
                attempt,
                max_retries,
                delay_ms,
            } => self.note(format!(
                "Provider retry {attempt}/{max_retries} in {delay_ms}ms"
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
                let activity = self.builder.activity_mut(turn, &call_id);
                assert_eq!(activity.state, ActivityState::Queued);
                activity.state = ActivityState::Running;
            }
            AgentEvent::ToolFinished {
                call_id,
                output,
                projection,
                ..
            } => {
                let turn = self.turn.expect("tool progress follows TurnAccepted");
                self.builder
                    .push_result(turn, &call_id, live_result(output, projection));
            }
            AgentEvent::ToolRejected {
                call_id, output, ..
            } => {
                let turn = self.turn.expect("tool progress follows TurnAccepted");
                self.builder.push_result(
                    turn,
                    &call_id,
                    live_result(output, ToolResultProjection::Observed),
                );
            }
            AgentEvent::InterruptedCalls(count) => self.note(format!(
                "{count} previous tool call(s) had unknown effects; inspect before retrying"
            )),
            AgentEvent::Final(_) => self.builder.close_group(),
        }
    }

    fn push_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.current_text.is_none() {
            self.group_closed_for_partial_text = self.builder.active_group;
            self.builder.close_group();
            let index = self.builder.projection.items.len();
            self.builder
                .projection
                .items
                .push(TranscriptItem::Assistant(TranscriptMessage {
                    turn: self.turn,
                    steering: false,
                    parts: vec![TranscriptPart::Text(String::new())],
                }));
            self.current_text = Some(index);
        }
        if let Some(index) = self.current_text
            && let TranscriptItem::Assistant(message) = &mut self.builder.projection.items[index]
            && let Some(TranscriptPart::Text(current)) = message.parts.first_mut()
        {
            current.push_str(&text);
        }
    }

    fn restart_partial_response(&mut self) {
        if let Some(index) = self.current_text.take() {
            assert_eq!(
                index + 1,
                self.builder.projection.items.len(),
                "provisional text is always the last transcript item"
            );
            self.builder.projection.items.pop();
        }
        if let Some((turn, index)) = self.group_closed_for_partial_text.take() {
            if let TranscriptItem::ActivityGroup(group) = &mut self.builder.projection.items[index]
            {
                group.open = true;
            }
            self.builder.active_group = Some((turn, index));
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
            cwd: "/tmp".into(),
            name: None,
            entries: vec![
                SessionEntry::TurnStarted {
                    turn: 1,
                    input: Message::user_input("inspect".into(), std::iter::empty()),
                    model,
                },
                assistant(
                    1,
                    vec![call("a", "read", serde_json::json!({"path":"a.rs"}))],
                ),
                SessionEntry::ToolResult {
                    projection: crate::ToolResultProjection::Observed,
                    turn: 1,
                    result: ToolResult {
                        call_id: "a".into(),
                        name: "read".into(),
                        result: serde_json::json!({"path":"a.rs"}),
                        images: Vec::new(),
                        is_error: false,
                    },
                },
                assistant(
                    1,
                    vec![call("b", "edit", serde_json::json!({"path":"b.rs"}))],
                ),
                SessionEntry::ToolResult {
                    projection: crate::ToolResultProjection::Observed,
                    turn: 1,
                    result: ToolResult {
                        call_id: "b".into(),
                        name: "edit".into(),
                        result: serde_json::json!({"path":"b.rs","replacements":1}),
                        images: Vec::new(),
                        is_error: false,
                    },
                },
                assistant(
                    1,
                    vec![
                        Content::Text("I found the issue.".into()),
                        call("c", "read", serde_json::json!({"path":"c.rs"})),
                    ],
                ),
                SessionEntry::ToolResult {
                    projection: crate::ToolResultProjection::Observed,
                    turn: 1,
                    result: ToolResult {
                        call_id: "c".into(),
                        name: "read".into(),
                        result: serde_json::json!({"path":"c.rs"}),
                        images: Vec::new(),
                        is_error: false,
                    },
                },
                assistant(1, vec![Content::Text("Done.".into())]),
                SessionEntry::TurnEnded {
                    turn: 1,
                    reason: TurnEndReason::Completed,
                },
            ],
            messages: Vec::new(),
            unfinished_turn: None,
            last_end: None,
            last_model: None,
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
        assert!(projected.items.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::Assistant(TranscriptMessage { parts, .. })
                    if parts == &vec![TranscriptPart::Text("I found the issue.".into())]
            )
        }));
    }

    fn committed(turn: u64, content: Vec<Content>) -> AgentEvent {
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
            content: message.content,
            tool_activities,
            termination,
        }
    }

    #[test]
    fn execution_start_changes_only_the_committed_call() {
        let mut live = LiveTranscript::default();
        live.observe(AgentEvent::TurnAccepted { turn: 1 });
        live.observe(committed(
            1,
            vec![
                call("first", "read", serde_json::json!({"path":"a"})),
                call("second", "read", serde_json::json!({"path":"b"})),
            ],
        ));
        live.observe(AgentEvent::ToolStarted {
            call_id: "first".into(),
            name: "read".into(),
            // Start cannot overwrite committed arguments or metadata.
            arguments: serde_json::json!({"path":"not-a"}),
            activity: ToolActivity::external("not-read"),
        });
        let TranscriptItem::ActivityGroup(group) = &live.projection().items[0] else {
            panic!()
        };
        assert_eq!(group.activities[0].state, ActivityState::Running);
        assert_eq!(group.activities[0].arguments["path"], "a");
        assert_eq!(group.activities[1].state, ActivityState::Queued);
        live.observe(AgentEvent::ToolFinished {
            projection: crate::ToolResultProjection::Observed,
            call_id: "first".into(),
            name: "read".into(),
            activity: ToolActivity::external("read"),
            output: ToolOutput {
                value: serde_json::json!({"content":"observed"}),
                images: vec![],
                is_error: false,
            },
        });
        let TranscriptItem::ActivityGroup(group) = &live.projection().items[0] else {
            panic!()
        };
        assert_eq!(group.activities[0].state, ActivityState::Completed);
        assert_eq!(group.activities[1].state, ActivityState::Queued);
    }

    #[test]
    fn live_projection_reopens_a_group_when_partial_text_is_restarted() {
        let mut live = LiveTranscript::default();
        live.observe(AgentEvent::TurnAccepted { turn: 7 });
        live.observe(committed(
            7,
            vec![call(
                "read-1",
                "read",
                serde_json::json!({"path":"src/lib.rs"}),
            )],
        ));
        live.observe(AgentEvent::ToolFinished {
            projection: crate::ToolResultProjection::Observed,
            call_id: "read-1".into(),
            name: "read".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("src/lib.rs".into()),
            },
            output: ToolOutput {
                value: serde_json::json!({"path":"src/lib.rs"}),
                images: Vec::new(),
                is_error: false,
            },
        });
        live.observe(AgentEvent::TextDelta("discard me".into()));
        live.observe(AgentEvent::ResponseRestarted);
        live.observe(committed(
            7,
            vec![call(
                "read-2",
                "read",
                serde_json::json!({"path":"src/main.rs"}),
            )],
        ));
        // Progress must not duplicate the already committed call.
        live.observe(AgentEvent::ToolStarted {
            call_id: "read-2".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path":"src/main.rs"}),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("src/main.rs".into()),
            },
        });
        assert_eq!(live.projection().items.len(), 1);
        let TranscriptItem::ActivityGroup(group) = &live.projection().items[0] else {
            panic!("expected one activity group");
        };
        assert!(group.open);
        assert_eq!(group.activities.len(), 2);
        assert_eq!(group.activities[0].state, ActivityState::Completed);
    }

    #[test]
    fn live_committed_text_splits_the_next_tool_group() {
        let mut live = LiveTranscript::default();
        live.observe(AgentEvent::TurnAccepted { turn: 3 });
        live.observe(committed(
            3,
            vec![call("one", "read", serde_json::json!({"path":"one.rs"}))],
        ));
        live.observe(AgentEvent::ToolFinished {
            projection: crate::ToolResultProjection::Observed,
            call_id: "one".into(),
            name: "read".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("one.rs".into()),
            },
            output: ToolOutput {
                value: serde_json::json!({}),
                images: Vec::new(),
                is_error: false,
            },
        });
        live.observe(AgentEvent::TextDelta("provisional".into()));
        live.observe(committed(
            3,
            vec![
                Content::Text("Now editing.".into()),
                call("two", "edit", serde_json::json!({"path":"two.rs"})),
            ],
        ));
        assert_eq!(live.projection().items.len(), 3);
        assert!(matches!(
            &live.projection().items[0],
            TranscriptItem::ActivityGroup(ActivityGroup { open: false, .. })
        ));
        assert!(matches!(
            &live.projection().items[1],
            TranscriptItem::Assistant(TranscriptMessage { parts, .. })
                if parts == &vec![TranscriptPart::Text("Now editing.".into())]
        ));
        assert!(matches!(
            &live.projection().items[2],
            TranscriptItem::ActivityGroup(ActivityGroup { open: true, .. })
        ));
    }
}

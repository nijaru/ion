//! Typed coding transcript projection over durable Session facts and live agent events.
use std::collections::HashMap;

use ion_ai::{Content, Message, ResponseTermination};
use serde_json::Value;

use crate::{
    agent::AgentEvent,
    session::{SessionEntry, SessionView},
    tool_set::{ToolActivity, ToolOutput},
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
pub enum ActivityOutcome {
    Pending,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityResult {
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
    pub outcome: ActivityOutcome,
    pub result: Option<ActivityResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityGroup {
    pub turn: u64,
    pub activities: Vec<TranscriptActivity>,
    /// Live groups stay mutable until visible assistant narrative closes them.
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
        let mut items = Vec::new();
        let mut active_group: Option<(u64, usize)> = None;
        let mut calls: HashMap<(u64, String), (usize, usize)> = HashMap::new();

        for entry in &view.entries {
            match entry {
                SessionEntry::TurnStarted { turn, input, .. } => {
                    active_group = None;
                    push_message(&mut items, *turn, false, input, true);
                }
                SessionEntry::Steering { turn, input } => {
                    active_group = None;
                    push_message(&mut items, *turn, true, input, true);
                }
                SessionEntry::Assistant {
                    turn,
                    message,
                    tool_activities,
                    termination,
                    ..
                } => {
                    for content in &message.content {
                        match content {
                            Content::Text(text) if !text.trim().is_empty() => {
                                active_group = None;
                                items.push(TranscriptItem::Assistant(TranscriptMessage {
                                    turn: Some(*turn),
                                    steering: false,
                                    parts: vec![TranscriptPart::Text(text.clone())],
                                }));
                            }
                            Content::Image(image) => {
                                active_group = None;
                                items.push(TranscriptItem::Assistant(TranscriptMessage {
                                    turn: Some(*turn),
                                    steering: false,
                                    parts: vec![TranscriptPart::Image {
                                        mime_type: image.mime_type().as_str().to_owned(),
                                    }],
                                }));
                            }
                            Content::ToolCall(call) => {
                                let group_index =
                                    ensure_group(&mut items, &mut active_group, *turn, false);
                                let activity = tool_activities
                                    .iter()
                                    .find(|stored| stored.call_id == call.id)
                                    .map_or_else(
                                        || ToolActivity::external(&call.name),
                                        |stored| stored.activity.clone(),
                                    );
                                let activity_index = match &mut items[group_index] {
                                    TranscriptItem::ActivityGroup(group) => {
                                        let index = group.activities.len();
                                        group.activities.push(TranscriptActivity {
                                            call_id: call.id.clone(),
                                            name: call.name.clone(),
                                            activity,
                                            arguments: call.arguments.clone(),
                                            outcome: if matches!(
                                                termination,
                                                ResponseTermination::Completed
                                            ) {
                                                ActivityOutcome::Pending
                                            } else {
                                                ActivityOutcome::Rejected
                                            },
                                            result: None,
                                        });
                                        index
                                    }
                                    _ => unreachable!("ensure_group returns an activity group"),
                                };
                                calls.insert(
                                    (*turn, call.id.clone()),
                                    (group_index, activity_index),
                                );
                            }
                            Content::Text(_) | Content::ToolResult(_) => {}
                        }
                    }
                }
                SessionEntry::ToolResult { turn, result } => {
                    if let Some(&(group_index, activity_index)) =
                        calls.get(&(*turn, result.call_id.clone()))
                        && let TranscriptItem::ActivityGroup(group) = &mut items[group_index]
                        && let Some(activity) = group.activities.get_mut(activity_index)
                    {
                        if activity.outcome != ActivityOutcome::Rejected {
                            activity.outcome = result_outcome(result.is_error, &result.result);
                        }
                        activity.result = Some(ActivityResult {
                            value: result.result.clone(),
                            image_mime_types: result
                                .images
                                .iter()
                                .map(|image| image.mime_type().as_str().to_owned())
                                .collect(),
                            is_error: result.is_error,
                        });
                    }
                }
                SessionEntry::UserShell {
                    command,
                    output,
                    is_error,
                    exclude_from_context,
                } => {
                    active_group = None;
                    items.push(TranscriptItem::UserShell(UserShellActivity {
                        command: command.clone(),
                        output: output.clone(),
                        is_error: *is_error,
                        exclude_from_context: *exclude_from_context,
                    }));
                }
                SessionEntry::TurnEnded { .. } => active_group = None,
                SessionEntry::ModelSelected { .. }
                | SessionEntry::ProviderReplayRebased { .. }
                | SessionEntry::Compacted { .. } => {}
            }
        }

        for item in &mut items {
            if let TranscriptItem::ActivityGroup(group) = item {
                group.open = false;
                for activity in &mut group.activities {
                    if activity.outcome == ActivityOutcome::Pending {
                        activity.outcome = ActivityOutcome::Unknown;
                    }
                }
            }
        }
        Self { items }
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

fn push_message(
    items: &mut Vec<TranscriptItem>,
    turn: u64,
    steering: bool,
    message: &Message,
    user: bool,
) {
    let parts = visible_parts(message);
    if parts.is_empty() {
        return;
    }
    let message = TranscriptMessage {
        turn: Some(turn),
        steering,
        parts,
    };
    items.push(if user {
        TranscriptItem::User(message)
    } else {
        TranscriptItem::Assistant(message)
    });
}

fn ensure_group(
    items: &mut Vec<TranscriptItem>,
    active_group: &mut Option<(u64, usize)>,
    turn: u64,
    open: bool,
) -> usize {
    if let Some((active_turn, index)) = *active_group
        && active_turn == turn
    {
        if let TranscriptItem::ActivityGroup(group) = &mut items[index] {
            group.open |= open;
        }
        return index;
    }
    let index = items.len();
    items.push(TranscriptItem::ActivityGroup(ActivityGroup {
        turn,
        activities: Vec::new(),
        open,
    }));
    *active_group = Some((turn, index));
    index
}

fn result_outcome(is_error: bool, value: &Value) -> ActivityOutcome {
    if !is_error {
        return ActivityOutcome::Completed;
    }
    if value
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ActivityOutcome::Cancelled;
    }
    if value
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ActivityOutcome::TimedOut;
    }
    ActivityOutcome::Failed
}

fn live_result(output: ToolOutput) -> ActivityResult {
    ActivityResult {
        value: output.value,
        image_mime_types: output
            .images
            .iter()
            .map(|image| image.mime_type().as_str().to_owned())
            .collect(),
        is_error: output.is_error,
    }
}

/// Mutable projection for one active Turn. It keeps a silent multi-step tool
/// phase in one open group and closes that group when visible narrative begins.
#[derive(Debug, Default)]
pub struct LiveTranscript {
    turn: Option<u64>,
    projection: TranscriptProjection,
    active_group: Option<usize>,
    current_text: Option<usize>,
    group_closed_for_partial_text: Option<usize>,
    calls: HashMap<String, (usize, usize)>,
    notices: Vec<String>,
}

impl LiveTranscript {
    pub fn with_user_input(input: &Message) -> Self {
        let parts = visible_parts(input);
        let projection = if parts.is_empty() {
            TranscriptProjection::default()
        } else {
            TranscriptProjection {
                items: vec![TranscriptItem::User(TranscriptMessage {
                    turn: None,
                    steering: false,
                    parts,
                })],
            }
        };
        Self {
            projection,
            ..Self::default()
        }
    }

    pub fn projection(&self) -> &TranscriptProjection {
        &self.projection
    }

    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    pub fn observe(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnAccepted { turn } => {
                self.turn = Some(turn);
                for item in &mut self.projection.items {
                    match item {
                        TranscriptItem::User(message) | TranscriptItem::Assistant(message)
                            if message.turn.is_none() =>
                        {
                            message.turn = Some(turn);
                        }
                        TranscriptItem::ActivityGroup(group) if group.turn == 0 => {
                            group.turn = turn;
                        }
                        _ => {}
                    }
                }
            }
            AgentEvent::TextDelta(text) => self.push_text(text),
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
            AgentEvent::ToolStarted {
                call_id,
                name,
                arguments,
                activity,
            } => {
                self.commit_partial_text_boundary();
                let group_index = self.ensure_live_group();
                let activity_index = match &mut self.projection.items[group_index] {
                    TranscriptItem::ActivityGroup(group) => {
                        let index = group.activities.len();
                        group.activities.push(TranscriptActivity {
                            call_id: call_id.clone(),
                            name,
                            activity,
                            arguments,
                            outcome: ActivityOutcome::Pending,
                            result: None,
                        });
                        index
                    }
                    _ => unreachable!("live group index is an activity group"),
                };
                self.calls.insert(call_id, (group_index, activity_index));
            }
            AgentEvent::ToolFinished {
                call_id,
                name,
                activity,
                output,
            } => {
                self.commit_partial_text_boundary();
                let outcome = result_outcome(output.is_error, &output.value);
                let result = live_result(output);
                self.finish_or_insert(call_id, name, activity, outcome, result);
            }
            AgentEvent::ToolRejected {
                call_id,
                name,
                activity,
                output,
            } => {
                self.commit_partial_text_boundary();
                let result = live_result(output);
                self.finish_or_insert(call_id, name, activity, ActivityOutcome::Rejected, result);
            }
            AgentEvent::InterruptedCalls(count) => self.note(format!(
                "{count} previous tool call(s) had unknown effects; inspect before retrying"
            )),
            AgentEvent::Final(_) => {
                self.close_group();
                self.current_text = None;
                self.group_closed_for_partial_text = None;
            }
        }
    }

    fn push_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.current_text.is_none() {
            if let Some(group_index) = self.active_group.take() {
                if let TranscriptItem::ActivityGroup(group) =
                    &mut self.projection.items[group_index]
                {
                    group.open = false;
                }
                self.group_closed_for_partial_text = Some(group_index);
            }
            let index = self.projection.items.len();
            self.projection
                .items
                .push(TranscriptItem::Assistant(TranscriptMessage {
                    turn: self.turn,
                    steering: false,
                    parts: vec![TranscriptPart::Text(String::new())],
                }));
            self.current_text = Some(index);
        }
        if let Some(index) = self.current_text
            && let TranscriptItem::Assistant(message) = &mut self.projection.items[index]
            && let Some(TranscriptPart::Text(current)) = message.parts.first_mut()
        {
            current.push_str(&text);
        }
    }

    fn commit_partial_text_boundary(&mut self) {
        self.current_text = None;
        self.group_closed_for_partial_text = None;
    }

    fn restart_partial_response(&mut self) {
        if let Some(index) = self.current_text.take()
            && index + 1 == self.projection.items.len()
        {
            self.projection.items.pop();
        }
        if let Some(group_index) = self.group_closed_for_partial_text.take() {
            if let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[group_index] {
                group.open = true;
            }
            self.active_group = Some(group_index);
        }
    }

    fn ensure_live_group(&mut self) -> usize {
        if let Some(index) = self.active_group {
            return index;
        }
        let index = self.projection.items.len();
        self.projection
            .items
            .push(TranscriptItem::ActivityGroup(ActivityGroup {
                turn: self.turn.unwrap_or(0),
                activities: Vec::new(),
                open: true,
            }));
        self.active_group = Some(index);
        index
    }

    fn finish_or_insert(
        &mut self,
        call_id: String,
        name: String,
        activity: ToolActivity,
        outcome: ActivityOutcome,
        result: ActivityResult,
    ) {
        if let Some(&(group_index, activity_index)) = self.calls.get(&call_id)
            && let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[group_index]
            && let Some(item) = group.activities.get_mut(activity_index)
        {
            item.outcome = outcome;
            item.result = Some(result);
            return;
        }

        let group_index = self.ensure_live_group();
        let activity_index = match &mut self.projection.items[group_index] {
            TranscriptItem::ActivityGroup(group) => {
                let index = group.activities.len();
                group.activities.push(TranscriptActivity {
                    call_id: call_id.clone(),
                    name,
                    activity,
                    arguments: Value::Null,
                    outcome,
                    result: Some(result),
                });
                index
            }
            _ => unreachable!("live group index is an activity group"),
        };
        self.calls.insert(call_id, (group_index, activity_index));
    }

    fn close_group(&mut self) {
        if let Some(index) = self.active_group.take()
            && let TranscriptItem::ActivityGroup(group) = &mut self.projection.items[index]
        {
            group.open = false;
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
    use std::sync::Arc;

    use ion_ai::{BoxFuture, ModelRef, Role, ToolCall, ToolResult, ToolSpec, Usage};
    use tokio_util::sync::CancellationToken;

    use crate::{
        CodingToolHost as ToolHost, ToolActivityKind, ToolDefinition, ToolPresentation, ToolSet,
        TurnEndReason,
    };

    struct TestTools;

    impl ToolHost for TestTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            ["read", "edit", "exec"]
                .into_iter()
                .map(|name| {
                    let kind = match name {
                        "read" => ToolActivityKind::Read,
                        "edit" => ToolActivityKind::Edit,
                        _ => ToolActivityKind::Command,
                    };
                    let argument = if name == "exec" { "command" } else { "path" };
                    ToolDefinition {
                        spec: ToolSpec {
                            name: name.into(),
                            description: name.into(),
                            input_schema: serde_json::json!({"type":"object"}),
                        },
                        presentation: ToolPresentation::argument(kind, argument),
                    }
                })
                .collect()
        }

        fn execute<'a>(
            &'a self,
            _call: &'a ToolCall,
            _stop: CancellationToken,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async {
                ToolOutput {
                    value: Value::Null,
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    fn catalog() -> ToolCatalog {
        ToolSet::new([Arc::new(TestTools) as Arc<dyn ToolHost>]).snapshot()
    }

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
            compacted_through: None,
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

    #[test]
    fn live_projection_reopens_a_group_when_partial_text_is_restarted() {
        let mut live = LiveTranscript::default();
        live.observe(AgentEvent::TurnAccepted { turn: 7 });
        let read = ToolActivity {
            kind: ToolActivityKind::Read,
            subject: Some("src/lib.rs".into()),
        };
        live.observe(AgentEvent::ToolStarted {
            call_id: "read-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path":"src/lib.rs"}),
            activity: read.clone(),
        });
        live.observe(AgentEvent::ToolFinished {
            call_id: "read-1".into(),
            name: "read".into(),
            activity: read,
            output: ToolOutput {
                value: serde_json::json!({"path":"src/lib.rs"}),
                images: Vec::new(),
                is_error: false,
            },
        });
        live.observe(AgentEvent::TextDelta("discard me".into()));
        live.observe(AgentEvent::ResponseRestarted);
        live.observe(AgentEvent::ToolStarted {
            call_id: "read-2".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path":"src/main.rs"}),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("src/main.rs".into()),
            },
        });

        assert_eq!(live.projection.items.len(), 1);
        let TranscriptItem::ActivityGroup(group) = &live.projection.items[0] else {
            panic!("expected one activity group");
        };
        assert!(group.open);
        assert_eq!(group.activities.len(), 2);
    }

    #[test]
    fn live_visible_text_splits_the_next_tool_group() {
        let mut live = LiveTranscript::default();
        live.observe(AgentEvent::TurnAccepted { turn: 3 });
        live.observe(AgentEvent::ToolStarted {
            call_id: "one".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path":"one.rs"}),
            activity: ToolActivity {
                kind: ToolActivityKind::Read,
                subject: Some("one.rs".into()),
            },
        });
        live.observe(AgentEvent::ToolFinished {
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
        live.observe(AgentEvent::TextDelta("Now editing.".into()));
        live.observe(AgentEvent::ToolStarted {
            call_id: "two".into(),
            name: "edit".into(),
            arguments: serde_json::json!({"path":"two.rs"}),
            activity: ToolActivity {
                kind: ToolActivityKind::Edit,
                subject: Some("two.rs".into()),
            },
        });

        assert_eq!(live.projection.items.len(), 3);
        assert!(matches!(
            &live.projection.items[0],
            TranscriptItem::ActivityGroup(ActivityGroup { open: false, .. })
        ));
        assert!(matches!(
            &live.projection.items[1],
            TranscriptItem::Assistant(_)
        ));
        assert!(matches!(
            &live.projection.items[2],
            TranscriptItem::ActivityGroup(ActivityGroup { open: true, .. })
        ));
    }
}

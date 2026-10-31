//! Bounded observation of the actual child control; no execution or writer lease.
use std::{
    sync::{Arc, Mutex, MutexGuard},
    task::Waker,
    time::{SystemTime, UNIX_EPOCH},
};

use ion_ai::Content;
use ion_core::{AgentEvent, DelegationPartialOutput, RequestKey};
use serde::Serialize;
use tokio::time::Instant;

/// Content-free observed activity, not a heartbeat or proof of a live effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildActivity {
    Model,
    Thinking,
    Text,
    Tool,
}

/// Ephemeral, possibly provisional observation. Only the owner's `joined` flag
/// attests full joining; silence, elapsed time and text do not establish success.
#[derive(Clone, Debug, Serialize)]
pub struct ChildActivitySnapshot {
    pub child_session: uuid::Uuid,
    pub request: RequestKey,
    pub elapsed_ms: u64,
    /// UTC milliseconds when activity was actually observed, not read or polled.
    pub last_activity_at_ms: Option<u64>,
    pub activity: Option<ChildActivity>,
    /// Top-level tool name only, at most 256 UTF-8 bytes. No arguments/results.
    pub current_tool: Option<String>,
    pub stopping: bool,
}

/// Activity metadata plus bounded, explicitly provisional visible text.
#[derive(Clone, Debug, Serialize)]
pub struct ChildProgressSnapshot {
    #[serde(flatten)]
    pub status: ChildActivitySnapshot,
    /// Latest visible text only: provisional child-authored data, never an answer.
    pub provisional_output: Option<DelegationPartialOutput>,
}

/// Read-only handle to the same bounded response used for joined failure reports.
/// Retaining it cannot retain a Session writer, task or cancellation authority.
#[derive(Clone, Default)]
pub struct ChildProgress(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    binding: Option<(uuid::Uuid, RequestKey, Instant)>,
    elapsed_ms: Option<u64>,
    last_activity_at_ms: Option<u64>,
    activity: Option<ChildActivity>,
    current_tool: Option<String>,
    stopping: bool,
    visible: VisibleResponse,
    changed: Option<Waker>,
}

impl ChildProgress {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn update(&self, update: impl FnOnce(&mut State)) {
        let changed = {
            let mut state = self.lock();
            update(&mut state);
            state.changed.clone()
        };
        if let Some(changed) = changed {
            changed.wake();
        }
    }

    pub(crate) fn observe_changes(&self, changed: Waker) {
        self.lock().changed = Some(changed);
    }

    pub(super) fn start(&self, child: uuid::Uuid, request: RequestKey, started: Instant) {
        self.update(|state| {
            assert!(state.binding.is_none(), "a prepared child runs only once");
            state.binding = Some((child, request, started));
        });
    }

    pub(super) fn freeze(&self, elapsed_ms: u64) {
        self.update(|state| {
            state.elapsed_ms.get_or_insert(elapsed_ms);
        });
    }

    pub(super) fn stop(&self, elapsed_ms: u64) {
        self.update(|state| {
            state.elapsed_ms.get_or_insert(elapsed_ms);
            state.stopping = true;
            state.activity = None;
            state.current_tool = None;
        });
    }

    pub(super) fn settle(&self, answer: Option<&str>) {
        self.update(|state| {
            state.activity = None;
            state.current_tool = None;
            if let Some(answer) = answer {
                state.visible.commit(answer);
            }
        });
    }

    pub(super) fn gap(&self) {
        self.update(|state| {
            state.visible = VisibleResponse {
                gap: true,
                suppress: true,
                ..Default::default()
            };
            state.activity = None;
            state.current_tool = None;
        });
    }

    pub(super) fn observe(&self, event: &AgentEvent) {
        let activity = match event {
            AgentEvent::TurnAccepted { .. }
            | AgentEvent::TurnResumed { .. }
            | AgentEvent::ModelOutputObserved
            | AgentEvent::ResponseRestarted
            | AgentEvent::ProviderRetry { .. }
            | AgentEvent::ContextCompactionRetry { .. }
            | AgentEvent::ContextCompacted { .. }
            | AgentEvent::ProviderReplayRebased => Some(ChildActivity::Model),
            AgentEvent::ThinkingDelta { .. } => Some(ChildActivity::Thinking),
            AgentEvent::TextDelta(_) => Some(ChildActivity::Text),
            AgentEvent::ToolStarted { .. }
            | AgentEvent::ChildToolStarted { .. }
            | AgentEvent::ChildToolFinished { .. } => Some(ChildActivity::Tool),
            AgentEvent::AssistantCommitted { .. }
            | AgentEvent::ToolFinished { .. }
            | AgentEvent::TurnEnded { .. }
            | AgentEvent::Final(_) => None,
            _ => return,
        };
        self.update(|state| {
            state.visible.observe(event);
            state.last_activity_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_millis()).ok());
            if !state.stopping && state.elapsed_ms.is_none() {
                state.activity = activity;
                match event {
                    AgentEvent::ToolStarted { name, .. } => {
                        state.current_tool =
                            Some(name[..name.floor_char_boundary(256.min(name.len()))].into());
                    }
                    AgentEvent::ToolFinished { .. } | AgentEvent::TurnEnded { .. } => {
                        state.current_tool = None;
                    }
                    _ => {}
                }
            }
        });
    }

    pub(super) fn output(&self) -> Option<DelegationPartialOutput> {
        self.lock().visible.output()
    }

    /// Content-free metadata without copying provisional or committed output.
    pub fn activity(&self) -> Option<ChildActivitySnapshot> {
        self.lock().activity_snapshot()
    }

    /// Null until the actual callback starts. This is not durable recovery data.
    pub fn snapshot(&self) -> Option<ChildProgressSnapshot> {
        let state = self.lock();
        Some(ChildProgressSnapshot {
            status: state.activity_snapshot()?,
            provisional_output: state.visible.output(),
        })
    }
}

impl State {
    fn activity_snapshot(&self) -> Option<ChildActivitySnapshot> {
        let (child_session, request, started) = self.binding.as_ref()?;
        Some(ChildActivitySnapshot {
            child_session: *child_session,
            request: request.clone(),
            elapsed_ms: self.elapsed_ms.unwrap_or_else(|| elapsed(*started)),
            last_activity_at_ms: self.last_activity_at_ms,
            activity: self.activity,
            current_tool: self.current_tool.clone(),
            stopping: self.stopping,
        })
    }
}

fn elapsed(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).expect("monotonic runtime fits milliseconds")
}

#[derive(Default)]
struct VisibleResponse {
    committed: Option<DelegationPartialOutput>,
    streaming: Option<DelegationPartialOutput>,
    suppress: bool,
    gap: bool,
}
impl VisibleResponse {
    fn commit(&mut self, text: &str) {
        let mut output = DelegationPartialOutput::default();
        output.push_text(text);
        self.committed = Some(output);
        self.streaming = None;
        self.suppress = false;
        self.gap = false;
    }

    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) if !self.suppress => {
                self.gap = false;
                self.streaming.get_or_insert_default().push_text(text);
            }
            AgentEvent::AssistantCommitted { content, .. } => {
                let mut output = DelegationPartialOutput::default();
                for (index, text) in content
                    .iter()
                    .filter_map(|part| match part {
                        Content::Text(text) => Some(text),
                        _ => None,
                    })
                    .enumerate()
                {
                    if index != 0 {
                        output.push_text("\n");
                    }
                    output.push_text(text);
                }
                self.committed = Some(output);
                self.streaming = None;
                self.suppress = false;
                self.gap = false;
            }
            AgentEvent::ResponseRestarted => {
                self.streaming = None;
                self.suppress = false;
            }
            _ => {}
        }
    }

    fn output(&self) -> Option<DelegationPartialOutput> {
        self.streaming
            .as_ref()
            .filter(|output| !output.text.is_empty())
            .or(self.committed.as_ref())
            .cloned()
            .filter(|output| !output.text.is_empty())
            .or_else(|| {
                self.gap.then_some(DelegationPartialOutput {
                    observation_gap: true,
                    ..Default::default()
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn activity_is_observed_not_polled_and_runtime_freezes_before_cleanup() {
        let progress = ChildProgress::default();
        assert!(progress.snapshot().is_none());
        let child = uuid::Uuid::new_v4();
        let request = RequestKey::new("child".into(), "observation".into()).unwrap();
        progress.start(child, request.clone(), Instant::now());
        progress.observe(&AgentEvent::ThinkingDelta {
            block: 0,
            text: "PRIVATE_THOUGHT".into(),
        });
        let thinking = progress.snapshot().unwrap();
        assert!(thinking.provisional_output.is_none());
        let thinking = thinking.status;
        assert_eq!(thinking.child_session, child);
        assert_eq!(thinking.request, request);
        assert_eq!(thinking.activity, Some(ChildActivity::Thinking));
        assert!(thinking.last_activity_at_ms.is_some());
        tokio::time::advance(Duration::from_secs(10)).await;
        let silent = progress.activity().unwrap();
        assert_eq!(silent.elapsed_ms, 10_000);
        assert_eq!(silent.last_activity_at_ms, thinking.last_activity_at_ms);
        assert_eq!(silent.activity, thinking.activity);
        progress.observe(&AgentEvent::ToolStarted {
            call_id: "opaque".into(),
            name: "🚀".repeat(100),
            arguments: serde_json::json!({"secret":"PRIVATE_ARGUMENT"}),
            activity: ion_core::ToolActivity::external("PRIVATE_SUBJECT"),
        });
        let tool = progress.snapshot().unwrap();
        assert_eq!(tool.status.activity, Some(ChildActivity::Tool));
        assert_eq!(
            tool.status.current_tool.as_deref(),
            Some("🚀".repeat(64).as_str())
        );
        assert!(tool.provisional_output.is_none());
        let encoded = serde_json::to_string(&tool).unwrap();
        for hidden in ["PRIVATE_THOUGHT", "PRIVATE_ARGUMENT", "PRIVATE_SUBJECT"] {
            assert!(!encoded.contains(hidden));
        }
        progress.stop(10_000);
        tokio::time::advance(Duration::from_secs(60)).await;
        let stopped = progress.activity().unwrap();
        assert!(stopped.stopping);
        assert_eq!(stopped.elapsed_ms, 10_000);
        assert_eq!(stopped.last_activity_at_ms, tool.status.last_activity_at_ms);
        assert!(stopped.activity.is_none() && stopped.current_tool.is_none());
        progress.settle(Some("last visible text"));
        assert_eq!(progress.activity().unwrap().elapsed_ms, 10_000);
        assert_eq!(progress.output().unwrap().text, "last visible text");
        assert!(
            !serde_json::to_string(&progress.activity().unwrap())
                .unwrap()
                .contains("last visible text")
        );
        assert!(
            serde_json::to_string(&progress.snapshot().unwrap())
                .unwrap()
                .contains("last visible text")
        );
    }
}

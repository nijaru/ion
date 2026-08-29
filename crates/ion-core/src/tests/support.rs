pub(super) use std::future::Future;
pub(super) use std::sync::{Arc, Mutex};
pub(super) use std::time::{Duration, Instant};

pub(super) use serde_json::json;
pub(super) use tokio::sync::mpsc;
pub(super) use tokio::time::{sleep, timeout};
pub(super) use tokio_util::sync::CancellationToken;

pub(super) use crate::context::{
    CapabilitySnapshot, ContextMessage, ContextPlan, load_trusted_resources,
};
pub(super) use crate::error::{CommandError, RuntimeError};
pub(super) use crate::ids::{EffectId, InboxId, OperationId};
pub(super) use crate::operation::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition,
};
pub(super) use crate::policy::{AllowlistPolicy, PolicyEngine};
pub(super) use crate::provider::{
    EngineSignal, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider,
};
pub(super) use crate::runtime::{
    EffectBoundary, EffectGate, OperationStatus, Runtime, RuntimeEvent, SaturatedHandle,
    SessionHandle,
};
pub(super) use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EntryRecord, InboxRecord, SessionRecord,
    SessionStore,
};
pub(super) use crate::tool::{
    RecoveryClass, Tool, ToolCall, ToolCatalog, ToolOutcome, ToolRegistry, ToolResult, ToolSpec,
};

pub(super) const STEP: Duration = Duration::from_millis(50);

/// Model snapshot used by transition tests.
pub(super) fn step_model() -> crate::provider::ModelConfig {
    crate::provider::ModelConfig {
        model_ref: "test-model".to_owned(),
        context_window: None,
        capabilities: crate::provider::ModelCapabilities::default(),
    }
}

pub(super) async fn collect_until_terminal(
    events: &mut crate::EventSubscription,
) -> Result<Vec<RuntimeEvent>, RuntimeError> {
    let mut out = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event recv timed out")?;
        let done = matches!(
            event,
            RuntimeEvent::OperationFinished { .. }
                | RuntimeEvent::OperationCancelled { .. }
                | RuntimeEvent::OperationFailed { .. }
                | RuntimeEvent::OperationIndeterminate { .. }
                | RuntimeEvent::OperationApprovalRequired { .. }
                | RuntimeEvent::SessionClosed { .. }
        );
        out.push(event);
        if done {
            return Ok(out);
        }
    }
}

/// Wait for the next ApprovalPending event, skipping non-park events.
pub(super) async fn wait_for_park(events: &mut crate::EventSubscription) -> RuntimeEvent {
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event recv timed out")
            .expect("event stream closed");
        match &event {
            RuntimeEvent::ApprovalPending { .. } => return event,
            RuntimeEvent::SessionClosed { .. } => panic!("stream closed before parking"),
            _ => {}
        }
    }
}

pub(super) fn kinds(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RuntimeEvent::OperationStarted { .. } => "operation_started",
            RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
            RuntimeEvent::ThinkingDelta { .. } => "thinking_delta",
            RuntimeEvent::ToolStarted { .. } => "tool_started",
            RuntimeEvent::ToolProgress { .. } => "tool_progress",
            RuntimeEvent::ToolSettled { .. } => "tool_settled",
            RuntimeEvent::UsageUpdate { .. } => "usage_update",
            RuntimeEvent::OperationFinished { .. } => "operation_finished",
            RuntimeEvent::OperationFailed { .. } => "operation_failed",
            RuntimeEvent::OperationIndeterminate { .. } => "operation_indeterminate",
            RuntimeEvent::OperationCancelled { .. } => "operation_cancelled",
            RuntimeEvent::OperationApprovalRequired { .. } => "operation_approval_required",
            RuntimeEvent::ApprovalPending { .. } => "approval_pending",
            RuntimeEvent::SessionClosed { .. } => "session_closed",
        })
        .collect()
}

pub(super) fn texts(events: &[RuntimeEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::AssistantTextDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Tests that exercise mechanics rather than policy run with every
/// core tool granted; the policy-gate tests construct their own.
pub(super) fn permissive_policy() -> Arc<dyn PolicyEngine> {
    Arc::new(AllowlistPolicy::new([
        "read", "write", "edit", "bash", "search", "find",
    ]))
}

/// Runtime over an in-memory store; file-backed stores are exercised by
/// the dedicated store tests below.
pub(super) fn start_runtime(provider: impl crate::Provider, tools: ToolRegistry) -> Runtime {
    let store = SessionStore::open_in_memory().expect("in-memory store");
    start_runtime_with_store(provider, tools, store)
}

pub(super) fn start_runtime_with_store(
    provider: impl crate::Provider,
    tools: ToolRegistry,
    store: SessionStore,
) -> Runtime {
    Runtime::start_with_policy(provider, tools, store, permissive_policy())
}

pub(super) fn tool_runtime() -> Runtime {
    start_runtime(ScriptedProvider::echo(), ToolRegistry::default())
}

pub(super) fn machine_with_tools(
    prompt: &str,
    tools: Vec<ToolSpec>,
) -> (OperationMachine, Applied) {
    OperationMachine::accept(OperationId::from_uuid(uuid::Uuid::nil()), prompt, tools)
}

pub(super) fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: "d".to_owned(),
        input_schema: json!({"type": "object"}),
    }
}

pub(super) fn call(id: u64, name: &str) -> crate::tool::ToolCall {
    crate::tool::ToolCall {
        operation_id: OperationId::from_uuid(uuid::Uuid::nil()),
        call_id: id,
        name: name.to_owned(),
        arguments: json!({}),
    }
}

/// A provider that records every model-step request it receives, so
/// tests can assert what the runtime projected into each step.
#[derive(Clone, Default)]
pub(super) struct SharedLogProvider {
    pub(super) log: Arc<Mutex<Vec<ProviderRequest>>>,
    pub(super) settle_delay: Duration,
}

impl SharedLogProvider {
    pub(super) fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for SharedLogProvider {
    fn run(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        let step = request.step;
        let delay = self.settle_delay;
        async move {
            self.log.lock().expect("log poisoned").push(request);
            if out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    step,
                    text: "working".to_owned(),
                })
                .await
                .is_err()
            {
                return;
            }
            sleep(delay).await;
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

pub(super) fn temp_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ion-store-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("sessions.db")
}

pub(super) fn entry_kinds<'a>(
    entries: impl IntoIterator<Item = &'a crate::SessionEntry>,
) -> Vec<&'static str> {
    entries
        .into_iter()
        .map(|entry| match entry {
            crate::SessionEntry::UserMessage { .. } => "user_message",
            crate::SessionEntry::AgentMessage { .. } => "agent_message",
            crate::SessionEntry::AssistantMessage { .. } => "assistant_message",
            crate::SessionEntry::ToolCall { .. } => "tool_call",
            crate::SessionEntry::ToolResult { .. } => "tool_result",
            crate::SessionEntry::Compaction { .. } => "compaction",
        })
        .collect()
}

pub(super) async fn wait_for_state(
    session: &SessionHandle,
    predicate: impl Fn(&OperationState) -> bool,
) {
    for _ in 0..50 {
        let snapshot = session.snapshot().await.expect("snapshot");
        if matches!(
            snapshot.operation,
            OperationStatus::Active { ref state, .. } if predicate(state)
        ) {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("operation never reached the expected state");
}

#[derive(Clone)]
pub(super) struct CompactionProbe {
    pub(super) log: Arc<Mutex<Vec<ProviderRequest>>>,
    pub(super) context_window: Option<u64>,
}

impl CompactionProbe {
    pub(super) fn with_window(tokens: u64) -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
            context_window: Some(tokens),
        }
    }
}

impl CompactionProbe {
    pub(super) fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for CompactionProbe {
    async fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    fn run(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        let step = request.step;
        let is_compaction = request.plan.messages.iter().any(|message| {
            matches!(message, crate::context::ContextMessage::User { content }
                if content.contains("Summarize the conversation"))
        });
        self.log.lock().expect("log poisoned").push(request);
        async move {
            if step == 1 {
                let _ = out
                    .send(EngineSignal::UsageUpdate {
                        operation_id,
                        step,
                        usage: crate::provider::TokenUsage {
                            input: 200_000,
                            output: 10,
                            cache_read: 0,
                            cache_write: 0,
                        },
                    })
                    .await;
            }
            let text = if is_compaction {
                "compact-summary: user asked X; answered; next: Y"
            } else {
                "answer"
            };
            let _ = out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    step,
                    text: text.to_owned(),
                })
                .await;
            // Hold step 1 open so the test's steer lands mid-flight,
            // after the delta the test waits for.
            if step == 1 {
                sleep(Duration::from_millis(300)).await;
            }
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

//! Compaction tests.

use super::support::*;

// ---- Readable compaction (DESIGN.md §14.7, §32 Step 4 slice 4) ----

#[test]
fn projector_uses_compaction_baseline_plus_suffix() {
    let entries = vec![
        SessionEntry::UserMessage {
            text: "early".to_owned(),
        },
        SessionEntry::AssistantMessage {
            text: "old answer".to_owned(),
        },
        SessionEntry::Compaction {
            covers_through_seq: 2,
            summary: "the user asked X; it was answered".to_owned(),
        },
        SessionEntry::UserMessage {
            text: "latest".to_owned(),
        },
    ];
    let plan = crate::context::project(&entries, 1);
    // The summary replaces everything through its coverage boundary.
    assert_eq!(plan.messages.len(), 2);
    assert!(
        plan.messages[0].prompt_text().contains("the user asked X"),
        "{:?}",
        plan.messages[0]
    );
    assert!(plan.messages[1].prompt_text().contains("latest"));
    assert!(!plan.messages[0].prompt_text().contains("early"));
}

#[test]
fn compaction_transitions_are_total() {
    // Invalid from Accepted.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    assert!(
        machine
            .apply(Transition::StartCompaction {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new()
                },
            })
            .is_err()
    );

    // NeedAssistant -> CompactionPending -> Completed appends the
    // readable entry and returns to NeedAssistant.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            model: step_model(),
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
    // Queue a steer so ProviderCompleted lands in NeedContinuation.
    machine
        .apply(Transition::ApplyInbox {
            item: InboxItem {
                kind: InboxKind::Steer,
                text: "more".to_owned(),
            },
        })
        .expect("queue steer");
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: Vec::new(),
        })
        .expect("complete with queued steer");
    machine
        .drain_steers()
        .expect("drain steers at the reasoning boundary");
    let applied = machine
        .apply(Transition::StartCompaction {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start compaction from NeedAssistant");
    assert!(matches!(applied.state, OperationState::CompactionPending));
    assert!(matches!(
        applied.intents[0],
        EffectIntent::Compaction { .. }
    ));

    let applied = machine
        .apply(Transition::CompactionCompleted {
            summary: "handoff".to_owned(),
            covers_through_seq: 3,
        })
        .expect("complete compaction");
    assert_eq!(applied.state, OperationState::NeedAssistant);
    assert_eq!(
        applied.entries,
        vec![SessionEntry::Compaction {
            covers_through_seq: 3,
            summary: "handoff".to_owned(),
        }]
    );

    // Failure without a cancellation request continues the operation.
    machine
        .apply(Transition::StartCompaction {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("compact again");
    let applied = machine
        .apply(Transition::CompactionFailed)
        .expect("abandon compaction");
    assert_eq!(applied.state, OperationState::NeedAssistant);
    let _ = applied;
}

#[tokio::test]
async fn large_context_compacts_at_the_continuation_boundary() {
    // 200k usage against a 128k window trips the safety net
    // (context_tokens > window - 16k reserve, 14.7.3).
    let probe = CompactionProbe::with_window(128_000);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    // Steer while step 1 streams so the operation continues; compaction
    // before an idle operation buys nothing.
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    session.steer("and also this").await.expect("steer");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Step 1: plain history. Step 2: compaction (history + instruction).
    // Step 3: summary baseline + the steered prompt only.
    let requests = probe.requests();
    assert_eq!(requests.len(), 3, "compaction must run between the steps");
    assert!(
        requests[1]
            .plan
            .messages
            .iter()
            .any(|m| m.prompt_text().contains("Summarize the conversation")),
        "the compaction step must carry the summarize instruction"
    );
    let third = &requests[2].plan.messages;
    assert!(
        third.len() <= 3
            && third
                .iter()
                .any(|m| m.prompt_text().contains("compact-summary")),
        "the steered step must project the summary baseline + suffix: {third:?}"
    );

    // Canonical history is untouched: the compaction entry joins the
    // early entries, it does not replace them.
    let loaded = store.load(session_id).await.expect("load");
    let kinds: Vec<_> = loaded
        .entries
        .iter()
        .map(|record| entry_kind_name(&record.entry))
        .collect();
    assert!(kinds.contains(&"compaction"), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|k| **k == "user_message").count(),
        2,
        "the prompt and the steer stay durable: {kinds:?}"
    );
}

fn entry_kind_name(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::UserMessage { .. } => "user_message",
        SessionEntry::ModelChanged { .. } => "model_changed",
        SessionEntry::AssistantMessage { .. } => "assistant_message",
        SessionEntry::ToolCall { .. } => "tool_call",
        SessionEntry::ToolResult { .. } => "tool_result",
        SessionEntry::Compaction { .. } => "compaction",
    }
}

// ---- Overflow recovery: one compaction, one retry (DESIGN.md §14.7.4) ----

#[derive(Clone)]
struct OverflowProbe {
    log: Arc<Mutex<Vec<ProviderRequest>>>,
    always_overflow: bool,
}

impl OverflowProbe {
    fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for OverflowProbe {
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
            if is_compaction {
                let _ = out
                    .send(EngineSignal::TextDelta {
                        operation_id,
                        step,
                        text: "overflow-summary: goal kept; next step kept".to_owned(),
                    })
                    .await;
                let _ = out
                    .send(EngineSignal::Completed { operation_id, step })
                    .await;
                return;
            }
            if step == 1 || self.always_overflow {
                let _ = out
                    .send(EngineSignal::Failed {
                        operation_id,
                        step,
                        message: "provider error: This model's maximum context length is \
                                  exceeded"
                            .to_owned(),
                    })
                    .await;
                return;
            }
            let _ = out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    step,
                    text: "retry answer".to_owned(),
                })
                .await;
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

#[tokio::test]
async fn context_overflow_compacts_once_and_retries() {
    let probe = OverflowProbe {
        log: Arc::new(Mutex::new(Vec::new())),
        always_overflow: false,
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "the retried step must complete the operation: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Step 1: plain. Step 2: the overflow compaction. Step 3: the retry
    // over the summary baseline.
    let requests = probe.requests();
    assert_eq!(requests.len(), 3, "compact once, retry once: {requests:?}");
    assert!(
        requests[2]
            .plan
            .messages
            .iter()
            .any(|m| m.prompt_text().contains("overflow-summary")),
        "the retry must project the summary baseline"
    );
    let loaded = store.load(session_id).await.expect("load");
    assert!(
        loaded.entries.iter().any(|record| matches!(
            &record.entry,
            crate::session::SessionEntry::Compaction { .. }
        )),
        "the overflow compaction must be durable"
    );
}

#[tokio::test]
async fn repeated_overflow_fails_visibly() {
    let probe = OverflowProbe {
        log: Arc::new(Mutex::new(Vec::new())),
        always_overflow: true,
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFailed { .. })),
        "a second overflow must fail visibly: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Exactly one compaction happened; the retry's failure was terminal.
    let requests = probe.requests();
    assert_eq!(requests.len(), 3, "no compaction loop: {requests:?}");
    let summarize_steps = requests
        .iter()
        .filter(|request| {
            request
                .plan
                .messages
                .iter()
                .any(|m| m.prompt_text().contains("Summarize the conversation"))
        })
        .count();
    assert_eq!(summarize_steps, 1, "only one summarize step may run");
}

use super::*;
use ion_ai::{BoxFuture, Message, ModelRef, ModelStream, Role, ToolCall};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct InterruptedStream {
    prefix: Mutex<Option<Vec<ModelStreamEvent>>>,
    requests: AtomicUsize,
}
impl ModelService for InterruptedStream {
    fn stream<'a>(&'a self, _: ModelRequest) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        // A service may enqueue work when called, before its future is polled.
        self.requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let prefix = self.prefix.lock().unwrap().take();
            let events = if let Some(prefix) = prefix {
                prefix
                    .into_iter()
                    .map(Ok)
                    .chain([Err(ProviderError {
                        kind: ProviderErrorKind::Overloaded,
                        message: "busy".into(),
                        retry_after_ms: Some(0),
                    })])
                    .collect::<Vec<_>>()
            } else {
                vec![Ok(ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None,
                    },
                    usage: Usage::known(100, 1),
                    termination: ion_ai::ResponseTermination::Completed,
                    returned_model: None,
                }))]
            };
            Ok(Box::pin(futures_util::stream::iter(events)) as ModelStream)
        })
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        route: ModelRoute::direct(
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
            ModelRouteReason::UserRequest,
        ),
        provider_session_id: None,
        instructions: None,
        messages: Vec::new(),
        tools: Vec::new(),
        context_timeline: None,
        prompt_cache: ion_ai::PromptCacheIntent::Default,
        controls: crate::AgentLimits::default().controls(ion_ai::Reasoning::ProviderDefault, false),
    }
}

#[tokio::test]
async fn cancellation_prevents_provider_construction_including_cache_warm() {
    for warming in [false, true] {
        let service = Arc::new(InterruptedStream {
            prefix: Mutex::new(None),
            requests: AtomicUsize::new(0),
        });
        let model: Arc<dyn ModelService> = service.clone();
        let stop = CancellationToken::new();
        stop.cancel();
        let cancelled = if warming {
            !refresh_prompt_cache(&model, request(), &stop, &mut None).await
        } else {
            matches!(
                generate_with_retry(&model, &request(), &stop, &mut |_| {}, |_| Ok(()))
                    .await
                    .outcome,
                Err(AgentError::Cancelled)
            )
        };
        assert_eq!(
            service.requests.load(Ordering::SeqCst),
            0,
            "warming={warming}"
        );
        assert!(cancelled, "warming={warming}");
    }
}

#[tokio::test]
async fn cancellation_from_retry_observer_settles_the_agent_without_redispatch() {
    let root = std::env::temp_dir().join(format!("ion-retry-stop-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session =
        Arc::new(crate::Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let service = Arc::new(InterruptedStream {
        prefix: Mutex::new(Some(vec![ModelStreamEvent::Usage(Usage::known(55, 0))])),
        requests: AtomicUsize::new(0),
    });
    let agent = crate::Agent::with_tool_set(
        service.clone(),
        Arc::new(crate::ToolSet::new([])),
        request().route.effective,
    );
    let stop = CancellationToken::new();
    let observer_stop = stop.clone();
    let result = agent
        .submit(
            session.clone(),
            "start".into(),
            "neutral".into(),
            stop,
            move |event| {
                if matches!(event, AgentEvent::ProviderRetry { .. }) {
                    observer_stop.cancel();
                }
            },
        )
        .await;
    assert!(matches!(result, Err(AgentError::Cancelled)));
    assert_eq!(service.requests.load(Ordering::SeqCst), 1);
    let view = session.view().unwrap();
    assert!(matches!(
        view.last_end,
        Some((1, crate::TurnEndReason::Cancelled))
    ));
    assert!(view.unfinished_turn.is_none());
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].role, Role::User);
    assert_eq!(view.usage().uncommitted.input_tokens.reported, 55);
    assert_eq!(view.usage().uncommitted.observations, 1);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn streamed_usage_survives_retry_or_failure_without_becoming_context() {
    for partial_output in [false, true] {
        let root = std::env::temp_dir().join(format!("ion-usage-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(crate::Session::create(&path, Some(&root)).unwrap());
        let mut prefix = vec![
            ModelStreamEvent::Usage(Usage::known(100, 0)),
            ModelStreamEvent::Usage(Usage {
                input_tokens: Some(120),
                output_tokens: None,
                cache_read_input_tokens: Some(0),
                cache_write_input_tokens: None,
            }),
        ];
        if partial_output {
            prefix.push(ModelStreamEvent::TextDelta("partial".into()));
        }
        let service = Arc::new(InterruptedStream {
            prefix: Mutex::new(Some(prefix)),
            requests: AtomicUsize::new(0),
        });
        let agent = crate::Agent::with_tool_set(
            service.clone(),
            Arc::new(crate::ToolSet::new([])),
            request().route.effective,
        );
        let result = agent
            .submit(
                session.clone(),
                "start".into(),
                "neutral".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await;
        assert_eq!(result.is_err(), partial_output);
        assert_eq!(
            service.requests.load(Ordering::SeqCst),
            if partial_output { 1 } else { 2 }
        );
        let view = session.view().unwrap();
        let usage = view.usage();
        assert_eq!(
            usage.total().input_tokens.reported,
            if partial_output { 120 } else { 220 }
        );
        assert_eq!(
            usage.total().output_tokens.reported,
            if partial_output { 0 } else { 1 }
        );
        assert_eq!(
            usage.total().observations,
            if partial_output { 1 } else { 2 }
        );
        assert_eq!(view.messages.len(), if partial_output { 1 } else { 2 });
        assert_eq!(usage.uncommitted.observations, 1);
        assert_eq!(
            view.entries
                .iter()
                .filter(|entry| matches!(entry, crate::SessionEntry::UncommittedUsage { .. }))
                .count(),
            1
        );
        assert_eq!(usage.assistant.observations, usize::from(!partial_output));
        assert_eq!(usage.models[0].effective, request().route.effective);
        drop(session);
        assert_eq!(crate::Session::inspect(&path).unwrap().usage(), usage);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn completion_usage_retains_missing_fields_and_honors_reported_zero() {
    for input in [None, Some(0)] {
        let root = std::env::temp_dir().join(format!("ion-usage-final-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(crate::Session::create(&path, Some(&root)).unwrap());
        let service = Arc::new(ion_ai::ScriptedModelService::new([ion_ai::Script::Stream(
            vec![
                ModelStreamEvent::Usage(Usage::known_with_cache(100, 2, 0, 0)),
                ModelStreamEvent::Usage(Usage {
                    input_tokens: Some(120),
                    ..Usage::unknown()
                }),
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None,
                    },
                    usage: Usage {
                        input_tokens: input,
                        output_tokens: Some(7),
                        ..Usage::unknown()
                    },
                    termination: ResponseTermination::Completed,
                    returned_model: Some("served".into()),
                }),
            ],
        )]));
        let agent = crate::Agent::with_tool_set(
            service,
            Arc::new(crate::ToolSet::new([])),
            request().route.effective,
        );
        agent
            .submit(
                session.clone(),
                "start".into(),
                "neutral".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let view = session.view().unwrap();
        let usage = view.usage();
        assert_eq!(usage.assistant.observations, 1);
        assert_eq!(usage.uncommitted.observations, 0);
        assert_eq!(
            usage.total().input_tokens.reported,
            u128::from(input.unwrap_or(120))
        );
        assert_eq!(usage.total().output_tokens.reported, 7);
        assert_eq!(usage.total().cache_read_input_tokens.unreported, 0);
        assert_eq!(usage.models[0].returned_model.as_deref(), Some("served"));
        assert_eq!(
            view.last_usage,
            Some(Usage::known_with_cache(input.unwrap_or(120), 7, 0, 0))
        );
        assert!(
            !view
                .entries
                .iter()
                .any(|entry| matches!(entry, crate::SessionEntry::UncommittedUsage { .. }))
        );
        drop(session);
        assert_eq!(crate::Session::inspect(&path).unwrap().usage(), usage);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn boundary_input_rejection_retains_response_usage_and_original_input() {
    let root = std::env::temp_dir().join(format!("ion-usage-input-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session =
        Arc::new(crate::Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let service = Arc::new(InterruptedStream {
        prefix: Mutex::new(None),
        requests: AtomicUsize::new(0),
    });
    let agent = crate::Agent::with_tool_set(
        service.clone(),
        Arc::new(crate::ToolSet::new([])),
        request().route.effective,
    )
    .with_limits(crate::AgentLimits {
        max_request_bytes: 2048,
        ..crate::AgentLimits::default()
    });
    let inbox = Arc::new(crate::SteeringInbox::new(
        crate::AgentLimits::default(),
        crate::InputBudget::default(),
    ));
    let pending = inbox.clone();
    let prompt = "x".repeat(3000);
    let queued = prompt.clone();
    let result = agent
        .submit_with_steering(
            session.clone(),
            "start".into(),
            "neutral".into(),
            CancellationToken::new(),
            inbox.clone(),
            move |event| {
                if matches!(event, AgentEvent::TextDelta(_)) {
                    pending.push(queued.clone()).unwrap();
                }
            },
        )
        .await;
    assert!(
        matches!(result, Err(AgentError::InputTooLarge { .. })),
        "{result:?}"
    );
    let view = session.view().unwrap();
    assert_eq!(view.usage().uncommitted.reported_tokens(), 101);
    assert_eq!(view.usage().uncommitted.observations, 1);
    assert_eq!(view.usage().assistant.observations, 0);
    assert_eq!(view.messages.len(), 1);
    assert!(view.unfinished_turn.is_none());
    assert_eq!(service.requests.load(Ordering::SeqCst), 1);
    let returned = inbox.take_uncommitted();
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].message(), &Message::user_input(prompt, []));
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn usage_storage_failure_blocks_retry_without_publishing_or_closing() {
    let root = std::env::temp_dir().join(format!("ion-usage-store-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("session.sqlite");
    let session = Arc::new(crate::Session::create(&path, Some(&root)).unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_usage BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'uncommitted_usage' BEGIN SELECT RAISE(ABORT, 'accounting unavailable'); END;").unwrap();
    let service = Arc::new(InterruptedStream {
        prefix: Mutex::new(Some(vec![ModelStreamEvent::Usage(Usage::known(100, 0))])),
        requests: AtomicUsize::new(0),
    });
    let agent = crate::Agent::with_tool_set(
        service.clone(),
        Arc::new(crate::ToolSet::new([])),
        request().route.effective,
    );
    let result = agent
        .submit(
            session.clone(),
            "start".into(),
            "neutral".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(AgentError::Session(_))));
    assert_eq!(service.requests.load(Ordering::SeqCst), 1);
    let view = session.view().unwrap();
    assert_eq!(view.unfinished_turn, Some(1));
    assert!(view.last_end.is_none());
    assert_eq!(view.usage().total().observations, 0);
    assert_eq!(view.messages.len(), 1);
    drop(connection);
    drop(session);
    assert_eq!(
        crate::Session::inspect(&path).unwrap().unfinished_turn,
        Some(1)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn only_generated_output_bars_retry_after_a_stream_error() {
    let metadata = vec![
        ModelStreamEvent::Usage(Usage::known(100, 0)),
        ModelStreamEvent::ProviderReplayNotice {
            action: "dropped".into(),
            reason: "input transformation".into(),
            count: 1,
        },
        ModelStreamEvent::TextDelta(String::new()),
        ModelStreamEvent::ThinkingDelta {
            block: 0,
            text: String::new(),
        },
    ];
    for (prefix, retry) in [
        (metadata, true),
        (vec![ModelStreamEvent::TextDelta("partial".into())], false),
        (
            vec![ModelStreamEvent::ThinkingDelta {
                block: 0,
                text: "thought".into(),
            }],
            false,
        ),
        (vec![ModelStreamEvent::OutputObserved], false),
        (
            vec![ModelStreamEvent::ToolCall(ToolCall {
                id: "call".into(),
                name: "tool".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })],
            false,
        ),
    ] {
        let service = Arc::new(InterruptedStream {
            prefix: Mutex::new(Some(prefix)),
            requests: AtomicUsize::new(0),
        });
        let model: Arc<dyn ModelService> = service.clone();
        let request = request();
        let mut output = false;
        let mut retries = 0;
        let mut notices = 0;
        let result = generate_with_retry(
            &model,
            &request,
            &CancellationToken::new(),
            &mut |event| match event {
                AgentEvent::ProviderRetry { .. } => {
                    assert!(!output, "must not retry after output was published");
                    retries += 1;
                }
                AgentEvent::TextDelta(_)
                | AgentEvent::ThinkingDelta { .. }
                | AgentEvent::ModelOutputObserved => output = true,
                AgentEvent::ProviderReplayNotice { .. } => notices += 1,
                _ => {}
            },
            |_| Ok(()),
        )
        .await;
        if retry {
            assert_eq!(result.route.reason, ModelRouteReason::Retry);
            assert_eq!(result.output, OutputObservation::Observed);
            assert!(result.outcome.is_ok());
            assert_eq!(retries, 1);
            assert_eq!(notices, 1, "input-transformation notices remain visible");
            assert_eq!(service.requests.load(Ordering::SeqCst), 2);
        } else {
            assert_eq!(result.output, OutputObservation::Observed);
            assert_eq!(result.route, request.route);
            assert!(matches!(
                result.outcome,
                Err(AgentError::Provider(ProviderError {
                    kind: ProviderErrorKind::Overloaded,
                    ..
                }))
            ));
            assert_eq!(retries, 0);
            assert_eq!(service.requests.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn completion_only_attempt_evidence_does_not_depend_on_observer_events() {
    for (content, expected) in [
        (Vec::new(), OutputObservation::Unobserved),
        (
            vec![Content::Text(String::new())],
            OutputObservation::Unobserved,
        ),
        (
            vec![Content::Thinking(String::new())],
            OutputObservation::Unobserved,
        ),
        (
            vec![Content::Thinking("private output".into())],
            OutputObservation::Observed,
        ),
        (
            vec![Content::Text("partial".into())],
            OutputObservation::Observed,
        ),
        (
            vec![Content::ToolCall(ToolCall {
                id: "call".into(),
                name: "tool".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })],
            OutputObservation::Observed,
        ),
    ] {
        let service: Arc<dyn ModelService> =
            Arc::new(ion_ai::ScriptedModelService::new([ion_ai::Script::Stream(
                vec![ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content,
                        provider_replay: None,
                    },
                    usage: Usage::known(100, 0),
                    termination: ResponseTermination::Incomplete(IncompleteReason::ContextLength),
                    returned_model: None,
                })],
            )]));
        let request = request();
        let before = tokio::time::Instant::now();
        // Summaries deliberately suppress provisional presentation, but still
        // must not replay any output, including completion-only thinking/calls.
        let attempt = generate_with_retry(
            &service,
            &request,
            &CancellationToken::new(),
            &mut |_| {},
            |_| Ok(()),
        )
        .await;
        assert_eq!(attempt.output, expected);
        assert_eq!(attempt.route, request.route);
        assert!(attempt.context_length_exceeded());
        assert!(before <= attempt.timing.start);
        assert!(attempt.timing.start <= attempt.timing.end);
        assert!(attempt.timing.end <= tokio::time::Instant::now());
    }
}

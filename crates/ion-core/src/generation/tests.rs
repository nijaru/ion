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
            refresh_prompt_cache(&model, request(), &stop)
                .await
                .is_none()
        } else {
            matches!(
                generate_with_retry(&model, &request(), &stop, &mut |_| {}).await,
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
        Arc::new(crate::CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let service = Arc::new(InterruptedStream {
        prefix: Mutex::new(Some(Vec::new())),
        requests: AtomicUsize::new(0),
    });
    let agent = crate::CodingAgent::with_tool_set(
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
    drop(session);
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
        let result =
            generate_with_retry(&model, &request, &CancellationToken::new(), &mut |event| {
                match event {
                    AgentEvent::ProviderRetry { .. } => {
                        assert!(!output, "must not retry after output was published");
                        retries += 1;
                    }
                    AgentEvent::TextDelta(_)
                    | AgentEvent::ThinkingDelta { .. }
                    | AgentEvent::ModelOutputObserved => output = true,
                    AgentEvent::ProviderReplayNotice { .. } => notices += 1,
                    _ => {}
                }
            })
            .await;
        if retry {
            assert_eq!(result.unwrap().route.reason, ModelRouteReason::Retry);
            assert_eq!(retries, 1);
            assert_eq!(notices, 1, "input-transformation notices remain visible");
            assert_eq!(service.requests.load(Ordering::SeqCst), 2);
        } else {
            assert!(matches!(
                result,
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

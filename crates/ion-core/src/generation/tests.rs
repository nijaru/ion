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
        Box::pin(async move {
            self.requests.fetch_add(1, Ordering::SeqCst);
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
        let request = ModelRequest {
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
            controls: crate::AgentLimits::default()
                .controls(ion_ai::Reasoning::ProviderDefault, false),
        };
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

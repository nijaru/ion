//! Provider capacity, not just local estimates, bounds a summary prefix.
use super::*;
use futures_util::stream;
use ion_ai::{BoxFuture, ModelStream, ModelStreamEvent, ToolChoice, Usage};

#[derive(Clone, Copy, Debug)]
enum Rejection {
    Open,
    Completed,
    Observed,
}

struct CapacityService {
    rejection: Rejection,
    max_records: usize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ModelService for CapacityService {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            let summary = request.route.reason == ModelRouteReason::Auxiliary;
            let rejected = summary && request.messages[0].content.len() > self.max_records;
            self.requests.lock().unwrap().push(request);
            let capacity = || ProviderError {
                kind: ProviderErrorKind::ContextLength,
                message: "fixture summary capacity exceeded".into(),
                retry_after_ms: None,
            };
            if rejected {
                match self.rejection {
                    Rejection::Open => return Err(capacity()),
                    Rejection::Observed => {
                        let events = [Ok(ModelStreamEvent::OutputObserved), Err(capacity())];
                        return Ok(Box::pin(stream::iter(events)) as ModelStream);
                    }
                    Rejection::Completed => {}
                }
            }
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: if rejected {
                        Vec::new()
                    } else {
                        vec![Content::Text(if summary {
                            "Settled history retained.".into()
                        } else {
                            "continued".into()
                        })]
                    },
                    provider_replay: None,
                },
                usage: Usage::unknown(),
                termination: if rejected {
                    ResponseTermination::Incomplete(IncompleteReason::ContextLength)
                } else {
                    ResponseTermination::Completed
                },
                returned_model: None,
            };
            Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Completed(response))])) as ModelStream)
        })
    }
}

fn model() -> ModelRef {
    ModelRef {
        provider: "test".into(),
        model: "test".into(),
    }
}

fn history() -> (std::path::PathBuf, Arc<Session>) {
    let root = std::env::temp_dir().join(format!("ion-summary-capacity-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    for index in 0..6 {
        let (turn, _) = session
            .begin_turn(format!("task {index}: {}", "a".repeat(300)), model())
            .unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("b".repeat(300))],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
    }
    (root, session)
}

#[tokio::test]
async fn automatic_compaction_shrinks_provider_rejected_summary_prefixes() {
    for rejection in [Rejection::Open, Rejection::Completed] {
        let (root, session) = history();
        let before = session.view().unwrap().entries;
        let service = Arc::new(CapacityService {
            rejection,
            max_records: 3,
            requests: Mutex::new(Vec::new()),
        });
        let agent =
            Agent::with_tool_set(service.clone(), Arc::new(ToolSet::new(Vec::new())), model())
                .with_limits(AgentLimits {
                    max_request_bytes: 3_200,
                    ..AgentLimits::default()
                });
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = events.clone();
        let result = agent
            .submit(
                session.clone(),
                "continue".into(),
                "test".into(),
                CancellationToken::new(),
                move |event| observed_events.lock().unwrap().push(event),
            )
            .await;
        let after = session.view().unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
        let requests = service.requests.lock().unwrap();
        let summaries = requests
            .iter()
            .filter(|request| request.route.reason == ModelRouteReason::Auxiliary)
            .collect::<Vec<_>>();
        assert!(summaries[0].messages[0].content.len() > 3);
        assert_eq!(result.unwrap(), "continued", "{rejection:?}");
        assert!(after.entries.starts_with(&before));
        assert!(after.compacted_through.is_some());
        assert_eq!(requests.len(), summaries.len() + 1);
        for pair in summaries.windows(2) {
            if pair[0].messages[0].content.len() > 3 {
                assert!(pair[1].messages[0].content.len() < pair[0].messages[0].content.len());
            }
        }
        for request in summaries {
            assert_eq!(request.route.effective, model());
            assert!(request.tools.is_empty());
            assert_eq!(request.controls.tool_choice, ToolChoice::None);
        }
        let retries = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ContextCompactionRetry {
                    through_entry,
                    next_through_entry,
                } => Some((*through_entry, *next_through_entry)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!retries.is_empty());
        assert!(retries.iter().all(|(from, to)| to < from));
    }
}

#[tokio::test]
async fn summary_recovery_stops_without_replacing_context_on_refusal_output_or_cancellation() {
    for (rejection, cancel) in [
        (Rejection::Open, false),
        (Rejection::Observed, false),
        (Rejection::Open, true),
    ] {
        let (root, session) = history();
        let before = session.view().unwrap().entries;
        let service = Arc::new(CapacityService {
            rejection,
            max_records: 0,
            requests: Mutex::new(Vec::new()),
        });
        let agent =
            Agent::with_tool_set(service.clone(), Arc::new(ToolSet::new(Vec::new())), model());
        let stop = CancellationToken::new();
        let observed_stop = stop.clone();
        let result = agent
            .compact(session.clone(), stop, move |event| {
                if cancel && matches!(event, AgentEvent::ContextCompactionRetry { .. }) {
                    observed_stop.cancel();
                }
            })
            .await;
        let after = session.view().unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(after.entries, before);
        assert!(after.compacted_through.is_none());
        let requests = service.requests.lock().unwrap();
        if cancel {
            assert!(matches!(result, Err(AgentError::Cancelled)));
            assert_eq!(requests.len(), 1);
        } else {
            assert!(matches!(
                result,
                Err(AgentError::Provider(ProviderError {
                    kind: ProviderErrorKind::ContextLength,
                    ..
                }))
            ));
            if matches!(rejection, Rejection::Observed) {
                assert_eq!(
                    requests.len(),
                    1,
                    "generated output bars smaller-prefix retry"
                );
            } else {
                assert!(requests.len() > 1);
                assert!(requests.windows(2).all(|pair| {
                    pair[1].messages[0].content.len() < pair[0].messages[0].content.len()
                }));
                // The smallest settled prefix is one complete user/assistant exchange.
                assert_eq!(requests.last().unwrap().messages[0].content.len(), 2);
            }
        }
    }
}

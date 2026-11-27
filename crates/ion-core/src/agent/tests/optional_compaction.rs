//! Failure policy at the optional output-room caller, not the strict summarizer.
use super::*;

type Events = Arc<Mutex<Vec<AgentEvent>>>;

fn observer() -> (Events, impl FnMut(AgentEvent) + Send + 'static) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    (events, move |event| captured.lock().unwrap().push(event))
}

fn skip_reasons(events: &Events) -> Vec<String> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ContextCompactionSkipped { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

struct Fixture {
    root: std::path::PathBuf,
    session: Arc<Session>,
    scripts: Arc<ScriptedModelService>,
    agent: Agent,
}

impl Fixture {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        let root =
            std::env::temp_dir().join(format!("ion-optional-policy-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let (turn, _) = session.begin_turn("earlier task".into(), model()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("h".repeat(9600))],
                    provider_replay: None,
                },
                Usage::known(10, 20),
                false,
            )
            .unwrap();
        let scripts = Arc::new(ScriptedModelService::new(scripts));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                max_output_tokens: 2500,
                context_window_tokens: Some(5000),
                ..AgentLimits::default()
            });
        Self {
            root,
            session,
            scripts,
            agent,
        }
    }

    async fn run(
        &self,
        observe: impl FnMut(AgentEvent) + Send + 'static,
    ) -> Result<String, AgentError> {
        self.agent
            .submit(
                self.session.clone(),
                "finish".into(),
                "test".into(),
                CancellationToken::new(),
                observe,
            )
            .await
    }

    async fn finish(self) {
        self.session.shutdown().await.unwrap();
        let Self {
            root,
            session,
            scripts,
            agent,
        } = self;
        drop((session, scripts, agent));
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn incomplete() -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text("UNCOMMITTED_SUMMARY".into())],
            provider_replay: None,
        },
        usage: Usage::known(500, 100),
        termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
        returned_model: None,
    })])
}

fn provider_failure(kind: ProviderErrorKind) -> Script {
    Script::OpenError(ProviderError {
        kind,
        message: "PRIVATE_PROVIDER_PAYLOAD\u{1b}[31m".into(),
        retry_after_ms: Some(0),
    })
}

#[tokio::test]
async fn provider_and_stream_failures_are_advisory_only_for_the_summary() {
    for (failure, reason) in [
        (
            provider_failure(ProviderErrorKind::Safety),
            "provider: Safety",
        ),
        (
            provider_failure(ProviderErrorKind::Authentication),
            "provider: Authentication",
        ),
        (
            provider_failure(ProviderErrorKind::Cancelled),
            "provider: Cancelled",
        ),
        (
            provider_failure(ProviderErrorKind::ReplayContextChanged),
            "summary provider replay unavailable",
        ),
        (
            Script::Stream(vec![ModelStreamEvent::Usage(Usage::known(500, 0))]),
            "incomplete summary response",
        ),
    ] {
        // Open errors have no observed usage; the incomplete stream does.
        let usage_observations = usize::from(matches!(&failure, Script::Stream(_)));
        let fixture = Fixture::new([failure, response(vec![Content::Text("done".into())])]);
        let (events, observe) = observer();
        assert_eq!(fixture.run(observe).await.unwrap(), "done");
        assert_eq!(skip_reasons(&events), [reason]);
        let mut live = crate::LiveTranscript::default();
        for event in events.lock().unwrap().iter() {
            live.observe(event);
        }
        assert_eq!(
            live.notices(),
            [format!(
                "Auto-compaction skipped: {reason}; existing context retained"
            )]
        );
        assert!(!events.lock().unwrap().iter().any(|event| matches!(
            event,
            AgentEvent::ProviderReplayRebased | AgentEvent::ContextCompacted { .. }
        )));
        let requests = fixture.scripts.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].tools.is_empty());
        assert!(!requests[1].tools.is_empty());
        assert!(
            requests[1].controls.max_output_tokens > 0
                && requests[1].controls.max_output_tokens < 2500
        );
        let view = fixture.session.view().unwrap();
        assert_eq!(view.usage().uncommitted.observations, usage_observations);
        assert!(view.compacted_through.is_none());
        assert_eq!(view.last_end.unwrap().1, TurnEndReason::Completed);
        fixture.finish().await;
    }

    // A shared route failure is still fatal on the actual coding request.
    let fixture = Fixture::new([
        provider_failure(ProviderErrorKind::Authentication),
        provider_failure(ProviderErrorKind::Authentication),
    ]);
    let (events, observe) = observer();
    let result = fixture.run(observe).await;
    assert!(matches!(
        result,
        Err(AgentError::Provider(ProviderError {
            kind: ProviderErrorKind::Authentication,
            ..
        }))
    ));
    assert_eq!(skip_reasons(&events).len(), 1);
    assert_eq!(fixture.scripts.requests().len(), 2);
    assert!(matches!(
        fixture.session.view().unwrap().last_end.unwrap().1,
        TurnEndReason::Failed(_)
    ));
    fixture.finish().await;
}

#[tokio::test]
async fn optional_failure_does_not_multiply_generation_retries_or_accounting() {
    // An opened stream may report input usage before a server error. Usage
    // alone does not establish readable output or forbid bounded retry.
    struct UsageBeforeError(Arc<ScriptedModelService>);
    impl ModelService for UsageBeforeError {
        fn validate_access(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
            self.0.validate_access()
        }

        fn stream<'a>(
            &'a self,
            request: &'a ModelRequest,
        ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ProviderError>> {
            Box::pin(async move {
                match self.0.stream(request).await {
                    Ok(stream) => Ok(stream),
                    Err(error) => Ok(Box::pin(futures_util::stream::iter([
                        Ok(ModelStreamEvent::Usage(Usage::known(500, 0))),
                        Err(error),
                    ])) as ion_ai::ModelStream),
                }
            })
        }
    }
    let mut fixture = Fixture::new([
        provider_failure(ProviderErrorKind::Server),
        provider_failure(ProviderErrorKind::Server),
        provider_failure(ProviderErrorKind::Server),
        response(vec![Content::Text("done".into())]),
    ]);
    fixture.agent = Agent::new(
        Arc::new(UsageBeforeError(fixture.scripts.clone())),
        Arc::new(TestTools::new(&fixture.root)),
        model(),
    )
    .with_limits(fixture.agent.limits());
    let (events, observe) = observer();
    assert_eq!(fixture.run(observe).await.unwrap(), "done");
    let requests = fixture.scripts.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[..3].iter().all(|request| request.tools.is_empty()));
    assert_eq!(requests[0].route.reason, ModelRouteReason::Auxiliary);
    assert_eq!(requests[1].route.reason, ModelRouteReason::Retry);
    assert_eq!(requests[2].route.reason, ModelRouteReason::Retry);
    assert_eq!(requests[3].route.reason, ModelRouteReason::UserRequest);
    assert_eq!(skip_reasons(&events).len(), 1);
    assert_eq!(
        fixture
            .session
            .view()
            .unwrap()
            .usage()
            .uncommitted
            .observations,
        3
    );
    assert_eq!(
        fixture
            .session
            .view()
            .unwrap()
            .usage()
            .uncommitted
            .input_tokens
            .reported,
        1500
    );
    fixture.finish().await;
}

#[tokio::test]
async fn rejected_summary_rebuild_admits_steering_and_honors_stop_or_observer_failure() {
    for action in ["steer", "stop", "panic"] {
        let fixture = Fixture::new([incomplete(), response(vec![Content::Text("done".into())])]);
        let stop = CancellationToken::new();
        let steering = Arc::new(SteeringInbox::new(
            fixture.agent.limits(),
            crate::InputBudget::default(),
        ));
        let (events, mut observe) = observer();
        let observed_stop = stop.clone();
        let observed_steering = steering.clone();
        let result = fixture
            .agent
            .submit_with_steering(
                fixture.session.clone(),
                "finish".into(),
                "test".into(),
                stop,
                steering.clone(),
                move |event| {
                    let skipped = matches!(event, AgentEvent::ContextCompactionSkipped { .. });
                    observe(event);
                    if skipped {
                        match action {
                            "steer" => {
                                observed_steering
                                    .push("NEW_GUIDANCE: inspect only".into())
                                    .unwrap();
                            }
                            "stop" => observed_stop.cancel(),
                            "panic" => panic!("observer failed on optional-compaction advisory"),
                            _ => unreachable!(),
                        }
                    }
                },
            )
            .await;
        assert_eq!(skip_reasons(&events).len(), 1);
        let requests = fixture.scripts.requests();
        let view = fixture.session.view().unwrap();
        assert!(view.compacted_through.is_none());
        assert_eq!(view.usage().uncommitted.output_tokens.reported, 100);
        if action == "steer" {
            assert_eq!(result.unwrap(), "done");
            assert_eq!(requests.len(), 2);
            assert_eq!(
                requests[1].messages.last(),
                Some(&user_text("NEW_GUIDANCE: inspect only".into()))
            );
            assert!(steering.take_uncommitted().is_empty());
        } else {
            assert!(match action {
                "stop" => matches!(result, Err(AgentError::Cancelled)),
                "panic" => matches!(result, Err(AgentError::ObserverPanicked)),
                _ => false,
            });
            assert_eq!(
                requests.len(),
                1,
                "fallback must not bypass the intake stop gate"
            );
            assert_eq!(view.last_end.unwrap().1, TurnEndReason::Cancelled);
        }
        fixture.finish().await;
    }
}

#[tokio::test]
async fn optional_summary_tool_calls_never_gain_execution_authority() {
    let fixture = Fixture::new([
        response(vec![Content::ToolCall(ToolCall {
            id: "auxiliary-call".into(),
            name: "write".into(),
            arguments: serde_json::json!({"path":"forbidden.txt", "content":"must not execute"}),
            raw_arguments: None,
        })]),
        response(vec![Content::Text("done".into())]),
    ]);
    let (events, observe) = observer();
    assert_eq!(fixture.run(observe).await.unwrap(), "done");
    assert_eq!(
        skip_reasons(&events),
        ["invalid summary: response contains non-text content"]
    );
    assert!(!fixture.root.join("forbidden.txt").exists());
    let view = fixture.session.view().unwrap();
    assert!(
        !view
            .entries
            .iter()
            .any(|entry| matches!(entry, SessionEntry::ToolResult { .. }))
    );
    assert_eq!(view.usage().uncommitted.observations, 1);
    assert!(view.compacted_through.is_none());
    assert!(
        !serde_json::to_string(&fixture.scripts.requests()[1].messages)
            .unwrap()
            .contains("auxiliary-call")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn optional_failure_cannot_bypass_failed_usage_storage() {
    let fixture = Fixture::new([
        incomplete(),
        response(vec![Content::Text("must not issue".into())]),
    ]);
    let connection = rusqlite::Connection::open(fixture.root.join("session.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_usage BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'uncommitted_usage' BEGIN SELECT RAISE(ABORT, 'accounting unavailable'); END;").unwrap();
    let (events, observe) = observer();
    let result = fixture.run(observe).await;
    assert!(matches!(result, Err(AgentError::Session(_))));
    assert_eq!(fixture.scripts.requests().len(), 1);
    assert!(skip_reasons(&events).is_empty());
    let view = fixture.session.view().unwrap();
    assert_eq!(view.unfinished_turn, Some(2));
    assert_eq!(view.usage().uncommitted.observations, 0);
    assert!(
        !view
            .entries
            .iter()
            .any(|entry| matches!(entry, SessionEntry::TurnEnded { turn: 2, .. }))
    );
    drop(connection);
    fixture.finish().await;
}

#[tokio::test]
async fn required_and_explicit_compaction_keep_strict_failure_semantics() {
    for explicit in [false, true] {
        let fixture = Fixture::new([
            incomplete(),
            response(vec![Content::Text("must not issue".into())]),
        ]);
        let (events, observe) = observer();
        let result = if explicit {
            fixture
                .agent
                .compact(
                    None,
                    fixture.session.clone(),
                    CancellationToken::new(),
                    observe,
                )
                .await
                .map(|_| ())
        } else {
            // Main instructions consume the remaining room; the auxiliary request
            // uses its own smaller instructions and still fits. This is required,
            // not optional, compaction despite the same settled history.
            fixture
                .agent
                .submit(
                    fixture.session.clone(),
                    "finish".into(),
                    "i".repeat(6000),
                    CancellationToken::new(),
                    observe,
                )
                .await
                .map(|_| ())
        };
        assert!(
            matches!(
                result,
                Err(AgentError::InvalidSummary("model output limit reached"))
            ),
            "{result:?}"
        );
        assert!(skip_reasons(&events).is_empty());
        assert_eq!(fixture.scripts.requests().len(), 1);
        let view = fixture.session.view().unwrap();
        assert!(view.compacted_through.is_none());
        assert_eq!(view.usage().uncommitted.output_tokens.reported, 100);
        if !explicit {
            assert!(matches!(view.last_end.unwrap().1, TurnEndReason::Failed(_)));
        }
        fixture.finish().await;
    }
}

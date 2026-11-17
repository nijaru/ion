//! Registered input contracts are an effect gate, not executor error reporting.
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    Agent, RequestKey, Session, SessionEntry, SubmissionRequest, ToolDecision, ToolDefinition,
    ToolExecutor, ToolInvocation, ToolOutcome, ToolOutput, ToolPolicy, ToolPolicyRequest,
    ToolRegistration, ToolSet, ToolSource,
};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-input-schema-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Effect {
    marker: PathBuf,
    schema: Value,
}
impl ToolSource for Effect {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "append".into(),
                description: "Append one durable effect".into(),
                input_schema: self.schema.clone(),
            })
            .with_result_schema(json!(false)),
            self,
        )]
    }
}
impl ToolExecutor for Effect {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
        // Deliberately unconditional, like an embedding executor that trusts its
        // registered schema. Even this synchronous effect must not be reached.
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.marker)
            .unwrap();
        writeln!(file, "{}", invocation.call().arguments).unwrap();
        file.sync_all().unwrap();
        Box::pin(async { ToolOutput::success(json!({"appended":true})) })
    }
}
#[derive(Default)]
struct Policy(AtomicUsize);
impl ToolPolicy for Policy {
    fn decide<'a>(
        &'a self,
        _: ToolPolicyRequest<'a>,
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolDecision, String>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(ToolDecision::Allow) })
    }
}
fn response(content: Vec<Content>) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}
fn call(id: usize, arguments: Value) -> ToolCall {
    ToolCall {
        id: format!("call-{id}"),
        name: "append".into(),
        arguments,
        raw_arguments: None,
    }
}
fn schema() -> Value {
    json!({
        "$defs": {"count": {"type":"integer", "minimum":1}},
        "type":"object", "required":["count", "mode"], "additionalProperties":false,
        "properties": {
            "count":{"$ref":"#/$defs/count"}, "mode":{"enum":["append"]},
            "label":{"type":"string", "default":"must not be inserted"}
        }
    })
}

#[tokio::test]
async fn direct_and_keyed_agent_calls_reject_before_policy_effect_and_reopen_without_replay() {
    for keyed in [false, true] {
        let root = Scratch::new();
        let path = root.0.join("session.sqlite");
        let marker = root.0.join("executor.jsonl");
        let session = Arc::new(Session::create(&path, None).unwrap());
        let invalid = [
            json!({"mode":"append"}),                          // required
            json!({"count":"1", "mode":"append"}),             // no coercion
            json!({"count":1, "mode":"erase"}),                // enum
            json!({"count":1, "mode":"append", "extra":true}), // no removal
            json!({"count":0, "mode":"append"}),               // local reference
            json!({"count":null, "mode":"append"}),
            json!({"count":[1], "mode":"append"}),
        ];
        let valid = json!({"count":1, "mode":"append"});
        let model = Arc::new(ScriptedModelService::new([
            response(
                invalid
                    .iter()
                    .enumerate()
                    .map(|(i, args)| Content::ToolCall(call(i, args.clone())))
                    .collect(),
            ),
            response(vec![Content::Text("refused".into())]),
            response(vec![Content::ToolCall(call(100, valid.clone()))]),
            response(vec![Content::Text("corrected".into())]),
        ]));
        let source = Arc::new(Effect {
            marker: marker.clone(),
            schema: schema(),
        });
        let policy = Arc::new(Policy::default());
        let agent = Agent::with_tool_set(
            model.clone(),
            Arc::new(ToolSet::new([source as Arc<dyn ToolSource>]).with_policy(policy.clone())),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        );
        let key = RequestKey::new("schema-test".into(), "invalid".into()).unwrap();
        let input = Message::user_input("invalid".into(), []);
        if keyed {
            agent
                .submit_request::<_, ()>(
                    session.clone(),
                    SubmissionRequest {
                        key: key.clone(),
                        input: input.clone(),
                    },
                    String::new(),
                    CancellationToken::new(),
                    None,
                    |_| {},
                )
                .await
                .unwrap()
                .operation
                .unwrap()
                .await
                .unwrap();
        } else {
            agent
                .submit(
                    session.clone(),
                    "invalid".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
        }
        assert!(
            !marker.exists(),
            "invalid schema input performed an executor effect"
        );
        assert_eq!(
            policy.0.load(Ordering::SeqCst),
            0,
            "invalid input asked policy"
        );
        let view = session.view().unwrap();
        let results: Vec<_> = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::ToolResult { result, .. } => Some(&result.outcome),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), invalid.len());
        assert!(
            results
                .iter()
                .all(|outcome| matches!(outcome, ToolOutcome::NotDispatched { .. }))
        );
        let saved_calls: Vec<_> = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::Assistant { message, .. } => Some(message),
                _ => None,
            })
            .flat_map(|message| &message.content)
            .filter_map(|part| match part {
                Content::ToolCall(call) => Some(call.arguments.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(saved_calls, invalid, "durable intent was rewritten");
        agent
            .submit(
                session.clone(),
                "corrected".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(policy.0.load(Ordering::SeqCst), 1);
        let effects = fs::read_to_string(&marker).unwrap();
        assert_eq!(effects.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(effects.trim()).unwrap(),
            valid,
            "defaults changed typed executor input"
        );
        let corrected = session.view().unwrap();
        assert!(corrected.entries.iter().any(|entry| matches!(entry,
            SessionEntry::ToolResult {result, ..} if result.call_id == "call-100"
                && matches!(&result.outcome, ToolOutcome::Observed {output, ..} if !output.is_error))),
            "result-schema metadata incorrectly became output enforcement");
        let before = serde_json::to_value(corrected.entries).unwrap();
        session.shutdown().await.unwrap();
        drop(session);
        let reopened = Arc::new(Session::open(&path).unwrap());
        assert_eq!(
            serde_json::to_value(reopened.view().unwrap().entries).unwrap(),
            before
        );
        if keyed {
            let reconciled = agent
                .submit_request::<_, ()>(
                    reopened.clone(),
                    SubmissionRequest { key, input },
                    String::new(),
                    CancellationToken::new(),
                    None,
                    |_| {},
                )
                .await
                .unwrap();
            assert!(
                reconciled.operation.is_none(),
                "redelivery replayed refused work"
            );
        }
        assert_eq!(model.requests().len(), 4);
        assert_eq!(fs::read_to_string(&marker).unwrap(), effects);
        reopened.shutdown().await.unwrap();
    }
}

struct NeverStart;
impl ion_core::CodeRuntime for NeverStart {
    fn start(
        &self,
        _: String,
        _: String,
        _: ion_core::CodeLimits,
        _: CancellationToken,
    ) -> Result<ion_core::CodeTask, String> {
        panic!("invalid composition input started the runtime")
    }
}

#[tokio::test]
async fn invalid_intrinsic_arguments_never_ask_policy_or_enter_the_route() {
    let root = Scratch::new();
    let session = Arc::new(Session::create(root.0.join("session.sqlite"), None).unwrap());
    let policy = Arc::new(Policy::default());
    let calls = [
        ("history", json!({})),
        ("history", json!({"action":"list", "extra":true})),
        ("code_mode", json!({"code":"return 1", "extra":true})),
        ("code_mode", json!({"code":1})),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (name, arguments))| {
        Content::ToolCall(ToolCall {
            name: name.into(),
            ..call(id, arguments)
        })
    })
    .collect();
    let model = Arc::new(ScriptedModelService::new([
        response(calls),
        response(vec![Content::Text("refused".into())]),
    ]));
    let tools = ToolSet::new([])
        .with_history()
        .with_code_mode(Arc::new(NeverStart), ion_core::CodeLimits::default())
        .with_policy(policy.clone());
    Agent::with_tool_set(
        model,
        Arc::new(tools),
        ModelRef {
            provider: "test".into(),
            model: "test".into(),
        },
    )
    .submit(
        session.clone(),
        "invalid".into(),
        String::new(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(policy.0.load(Ordering::SeqCst), 0);
    let outcomes: Vec<_> = session
        .view()
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } => Some(result.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes.len(), 4);
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, ToolOutcome::NotDispatched { .. }))
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_registered_schema_fails_closed_even_for_low_level_execution() {
    let root = Scratch::new();
    let remote = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    remote.set_nonblocking(true).unwrap();
    for schema in [
        json!({"$ref": format!("http://{}/SCHEMA_CANARY", remote.local_addr().unwrap())}),
        json!({"$schema":"https://example.invalid/SCHEMA_CANARY"}),
        json!({"$ref":"#/$defs/missing"}),
        json!({"type":"SCHEMA_CANARY"}),
        json!({"pattern":"^(a+)+(?=b)$"}),
    ] {
        let marker = root.0.join("effect");
        let source = Arc::new(Effect {
            marker: marker.clone(),
            schema,
        });
        let catalog = ToolSet::new([source as Arc<dyn ToolSource>]).snapshot();
        let output = catalog
            .execute(ToolInvocation::untracked(
                &call(0, json!({})),
                CancellationToken::new(),
            ))
            .await;
        assert!(!marker.exists(), "uncheckable input contract executed");
        assert!(output.is_error);
        assert!(
            output
                .value
                .to_string()
                .contains("input schema unavailable"),
            "{}",
            output.value
        );
        assert!(!output.value.to_string().contains("SCHEMA_CANARY"));
    }
    assert!(
        matches!(remote.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

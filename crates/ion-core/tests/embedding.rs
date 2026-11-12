use futures_util::future::BoxFuture;
use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    Agent, Session, SessionEntry, ToolDefinition, ToolExecutor, ToolExposure, ToolInvocation,
    ToolOutput, ToolPresentation, ToolRegistration, ToolSource, TurnEndReason,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Orders(AtomicUsize);

impl ToolSource for Orders {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition {
                spec: ToolSpec {
                    name: "order_status".into(),
                    description: "Look up an order's current status".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {"order_id": {"type": "integer"}},
                        "required": ["order_id"],
                        "additionalProperties": false,
                    }),
                },
                result_schema: None,
                presentation: ToolPresentation::external(),
                exposure: ToolExposure::Direct,
            },
            self,
        )]
    }
}

impl ToolExecutor for Orders {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            assert!(invocation.origin().is_some());
            assert_eq!(invocation.call().arguments, json!({"order_id":42}));
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolOutput::success(json!({"order_id":42,"status":"shipped"}))
        })
    }
}

fn response(content: Content) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![content],
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn domain_tools_work_without_a_coding_workspace_and_reopen_passively() {
    let root = Scratch(std::env::temp_dir().join(format!("ion-domain-{}", uuid::Uuid::now_v7())));
    std::fs::create_dir(&root.0).unwrap();
    let path = root.0.join("orders.sqlite");
    let session = Arc::new(Session::create(&path, None).unwrap());
    let orders = Arc::new(Orders::default());
    let service = Arc::new(ScriptedModelService::new([
        response(Content::ToolCall(ToolCall {
            id: "lookup".into(),
            name: "order_status".into(),
            arguments: json!({"order_id":42}),
            raw_arguments: None,
        })),
        response(Content::Text("Order 42 has shipped.".into())),
        response(Content::Text(
            "It has shipped, as previously observed.".into(),
        )),
    ]));
    let agent = Agent::new(
        service.clone(),
        orders.clone(),
        ModelRef {
            provider: "test".into(),
            model: "orders".into(),
        },
    );
    for (prompt, expected) in [
        ("Check order 42", "Order 42 has shipped."),
        (
            "Restate the observed status",
            "It has shipped, as previously observed.",
        ),
    ] {
        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    prompt.into(),
                    "Use the order tools.".into(),
                    CancellationToken::new(),
                    |_| {}
                )
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(orders.0.load(Ordering::SeqCst), 1);
    let requests = service.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|spec| spec.name == "order_status")
    );
    for request in &requests[1..] {
        assert!(
            request.messages.iter().flat_map(|message| &message.content).any(|content| {
                matches!(content, Content::ToolResult(result)
                    if result.result == json!({"order_id":42,"status":"shipped"}) && !result.is_error)
            })
        );
    }
    let before = session.view().unwrap();
    assert!(before.cwd.is_none());
    assert_eq!(
        before
            .entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionEntry::TurnEnded {
                    reason: TurnEndReason::Completed,
                    ..
                }
            ))
            .count(),
        2
    );
    session.shutdown().await.unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    let after = reopened.view().unwrap();
    assert!(after.cwd.is_none());
    assert_eq!(
        serde_json::to_value(&after.entries).unwrap(),
        serde_json::to_value(&before.entries).unwrap()
    );
    assert_eq!(orders.0.load(Ordering::SeqCst), 1);
    assert_eq!(service.requests().len(), 3);
    reopened.shutdown().await.unwrap();
}

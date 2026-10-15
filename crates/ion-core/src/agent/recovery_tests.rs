use super::*;
use crate::{AuthorityScope, ToolDefinition, ToolExecutor, ToolRegistration};
use ion_ai::{BoxFuture, ModelStreamEvent, Script, ScriptedModelService, ToolCall, ToolSpec};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counter(AtomicUsize);
impl ToolExecutor for Counter {
    fn execute<'a>(
        &'a self,
        _: &'a ToolCall,
        _: CancellationToken,
    ) -> BoxFuture<'a, crate::CodingToolOutput> {
        Box::pin(async move {
            crate::CodingToolOutput {
                value: serde_json::json!(self.0.fetch_add(1, Ordering::SeqCst) + 1),
                images: vec![],
                is_error: false,
            }
        })
    }
}
struct Switching {
    snapshots: AtomicUsize,
    first: Arc<Counter>,
    replacement: Arc<Counter>,
}
impl ToolSource for Switching {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        let executor = if self.snapshots.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first.clone()
        } else {
            self.replacement.clone()
        };
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "count".into(),
                description: "count".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }),
            executor,
        )]
    }
}
fn response(content: Content) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![content],
            provider_replay: None,
        },
        usage: ion_ai::Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}

#[tokio::test]
async fn opted_in_turn_pins_actual_routes_not_only_equal_tool_names_and_schemas() {
    for scoped in [false, true] {
        let root = std::env::temp_dir().join(format!("ion-pinned-turn-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
        let service = Arc::new(ScriptedModelService::new([
            response(Content::ToolCall(ToolCall {
                id: "one".into(),
                name: "count".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })),
            response(Content::ToolCall(ToolCall {
                id: "two".into(),
                name: "count".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })),
            response(Content::Text("done".into())),
        ]));
        let first = Arc::new(Counter(AtomicUsize::new(0)));
        let replacement = Arc::new(Counter(AtomicUsize::new(0)));
        let source = Arc::new(Switching {
            snapshots: AtomicUsize::new(0),
            first: first.clone(),
            replacement: replacement.clone(),
        });
        let agent = Agent::new(
            service,
            source,
            ModelRef {
                provider: "fixture".into(),
                model: "fixture".into(),
            },
        );
        let agent = if scoped {
            agent.with_recovery_scope(AuthorityScope::new("counter/principal/routes-v1").unwrap())
        } else {
            agent
        };
        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    "two counts".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {}
                )
                .await
                .unwrap(),
            "done"
        );
        assert_eq!(
            (
                first.0.load(Ordering::SeqCst),
                replacement.0.load(Ordering::SeqCst)
            ),
            if scoped { (2, 0) } else { (1, 1) }
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

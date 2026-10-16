use std::sync::{Arc, Mutex};

use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    CodeLimits, CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, SessionEntry,
    ToolDefinition, ToolExecutor, ToolInvocation, ToolOrigin, ToolRegistration, ToolSet,
};
use ion_host::code_mode::QuickJs;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct Recorder {
    session: Arc<CodingSession>,
    entered: Mutex<Vec<Option<ToolOrigin>>>,
    completed: Mutex<Vec<ToolOrigin>>,
    release: Notify,
}
impl CodingToolSource for Recorder {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "observe".into(),
                description: "Observe invocation provenance.".into(),
                input_schema: json!({"type":"object"}),
            }),
            self,
        )]
    }
}
impl ToolExecutor for Recorder {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        let call = invocation.call();
        let origin = invocation.origin();
        let stop = invocation.stop().clone();
        // Deliberately synchronous: the durable intent must precede even the
        // executor's future constructor, not merely its first asynchronous effect.
        if let Some(origin) = origin {
            assert_eq!(origin.session(), self.session.id());
            let view = self.session.view().unwrap();
            assert_eq!(view.unfinished_turn, Some(origin.turn()));
            let SessionEntry::Assistant { turn, message, .. } = &view.entries[usize::try_from(
                origin.occurrence().assistant_entry.checked_sub(1).unwrap(),
            )
            .unwrap()] else {
                panic!("origin is not a committed assistant");
            };
            assert_eq!(*turn, origin.turn());
            let parent = message
                .content
                .iter()
                .filter_map(|part| match part {
                    Content::ToolCall(call) => Some(call),
                    _ => None,
                })
                .nth(origin.occurrence().ordinal)
                .unwrap();
            if let Some(child) = origin.composed_child() {
                assert_eq!(parent.name, "code_mode");
                assert!(view.entries.iter().any(|entry| matches!(entry,
                    SessionEntry::ChildToolAdmitted { turn, intent }
                    if *turn == origin.turn() && intent.parent == origin.occurrence()
                        && intent.child == child && &intent.call == call
                )));
                assert!(!view.entries.iter().any(|entry| matches!(entry,
                    SessionEntry::ChildToolResult { parent, child: index, .. }
                    if *parent == origin.occurrence() && *index == child
                )));
            } else {
                assert_eq!(parent, call);
            }
        }
        self.entered.lock().unwrap().push(origin);
        Box::pin(async move {
            if call.arguments["order"] == 0 {
                tokio::select! {
                    () = self.release.notified() => {},
                    () = stop.cancelled() => return CodingToolOutput {
                        value: json!("stopped"), images: Vec::new(), is_error: true,
                    },
                }
            }
            if let Some(origin) = origin {
                self.completed.lock().unwrap().push(origin);
            }
            if call.arguments["order"] == 1 {
                self.release.notify_one();
            }
            CodingToolOutput {
                value: Value::Bool(true),
                images: Vec::new(),
                is_error: false,
            }
        })
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
fn call(id: &str, name: &str, arguments: Value) -> Content {
    Content::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
        raw_arguments: None,
    })
}

#[tokio::test]
async fn provenance_comes_from_committed_occurrences_not_provider_ids_or_arguments() {
    let root = std::env::temp_dir().join(format!("ion-tool-origin-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(CodingSession::create(root.join("session.sqlite"), None).unwrap());
    let recorder = Arc::new(Recorder {
        session: session.clone(),
        entered: Mutex::new(Vec::new()),
        completed: Mutex::new(Vec::new()),
        release: Notify::new(),
    });
    let tools = Arc::new(
        ToolSet::new([recorder.clone() as Arc<dyn CodingToolSource>])
            .with_code_mode(Arc::new(QuickJs), CodeLimits::default()),
    );
    let spoof = json!({"session":"not-the-session","turn":999,"assistant_entry":999,"ordinal":999,"composed_child":999});
    let raw_call = ToolCall {
        id: "same".into(),
        name: "observe".into(),
        arguments: spoof.clone(),
        raw_arguments: None,
    };
    let catalog = tools.snapshot();
    let unpolled = catalog.execute_untracked(&raw_call, CancellationToken::new());
    assert!(recorder.entered.lock().unwrap().is_empty());
    drop(unpolled);
    assert!(recorder.entered.lock().unwrap().is_empty());
    assert!(session.view().unwrap().entries.is_empty());
    assert!(
        !catalog
            .execute_untracked(&raw_call, CancellationToken::new())
            .await
            .is_error
    );
    assert_eq!(*recorder.entered.lock().unwrap(), [None]);
    assert!(session.view().unwrap().entries.is_empty());

    let model = Arc::new(ScriptedModelService::new([
        response(vec![
            Content::Text("prefix".into()),
            call("same", "observe", spoof.clone()),
            Content::Text("between".into()),
            call("other", "observe", spoof.clone()),
        ]),
        response(vec![call("same", "observe", spoof.clone())]),
        response(vec![call(
            "same",
            "code_mode",
            json!({"code":
                "return await Promise.all([tools.observe({order:0,turn:999}),tools.observe({order:1,turn:999})]);"
            }),
        )]),
        response(vec![Content::Text("first done".into())]),
        response(vec![call("same", "observe", spoof)]),
        response(vec![Content::Text("second done".into())]),
    ]));
    let agent = CodingAgent::with_tool_set(
        model,
        tools,
        ModelRef {
            provider: "synthetic".into(),
            model: "provenance".into(),
        },
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        for input in ["first", "second"] {
            agent
                .submit(
                    session.clone(),
                    input.into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await?;
        }
        Ok::<_, ion_core::CodingAgentError>(())
    })
    .await;
    // Join cleanup even if qualification times out; do not abandon retained work.
    let cleanup = session.shutdown().await;
    result.unwrap().unwrap();
    cleanup.unwrap();
    let entered = recorder.entered.lock().unwrap();
    let origins = entered
        .iter()
        .skip(1)
        .map(|origin| origin.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(origins.len(), 6);
    assert_eq!(
        origins
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
    assert_eq!(origins[0].occurrence().ordinal, 0);
    assert_eq!(origins[1].occurrence().ordinal, 1);
    assert_eq!(
        origins[0].occurrence().assistant_entry,
        origins[1].occurrence().assistant_entry
    );
    assert_ne!(
        origins[0].occurrence().assistant_entry,
        origins[2].occurrence().assistant_entry
    );
    let view = session.view().unwrap();
    let children = [0, 1].map(|order| {
        view.entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::ChildToolAdmitted { intent, .. }
                    if intent.call.arguments["order"] == order =>
                {
                    Some(intent.child)
                }
                _ => None,
            })
            .unwrap()
    });
    for child in children {
        assert!(
            origins[3..5]
                .iter()
                .any(|origin| origin.composed_child() == Some(child))
        );
    }
    assert_eq!(origins[3].occurrence(), origins[4].occurrence());
    assert_eq!(origins[5].turn(), 2);
    assert!(origins[..5].iter().all(|origin| origin.turn() == 1));
    let completed = recorder.completed.lock().unwrap();
    // Scheduling need not enter constructors in admission order. The waiter
    // finishes after its sibling, but both retain their committed child index.
    assert_eq!(completed[3].composed_child(), Some(children[1]));
    assert_eq!(completed[4].composed_child(), Some(children[0]));
    drop(completed);
    drop(entered);
    drop(agent);
    drop(catalog);
    drop(recorder);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

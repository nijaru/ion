use std::{collections::BTreeSet, time::Duration};

use ion_ai::{Content, Message, ModelRef, Role, ToolResult, ToolSpec, Usage};
use tokio::sync::mpsc;

use super::*;
use crate::{ToolActivity, ToolSet, session::TurnEndReason};

struct Guest {
    await_reply: bool,
}
impl CodeRuntime for Guest {
    fn start(
        &self,
        query: String,
        _limits: CodeLimits,
        stop: CancellationToken,
    ) -> Result<CodeTask, String> {
        let (sender, requests) = mpsc::channel(1);
        let await_reply = self.await_reply;
        let runtime = tokio::runtime::Handle::current();
        // Like QuickJS, the guest retains a blocking-pool thread while awaiting
        // its bridge. Inspection must not need another thread from that pool.
        let result = tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                let (reply, receive) = oneshot::channel();
                sender
                    .send(CodeRequest {
                        kind: CodeRequestKind::Inspect { query_json: query },
                        reply,
                    })
                    .await
                    .unwrap();
                if await_reply {
                    tokio::select! {
                        () = stop.cancelled() => Err("guest stopped".into()),
                        result = receive => result.unwrap(),
                    }
                } else {
                    drop(receive);
                    Ok("null".into())
                }
            })
        });
        Ok(CodeTask { requests, result })
    }
}

fn assistant(call: ToolCall) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(call)],
        provider_replay: None,
    }
}

fn fixture() -> (std::path::PathBuf, Session, u64, ToolCall) {
    let root = std::env::temp_dir().join(format!("ion-inspection-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Session::create(root.join("session.sqlite"), &root).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("seed".into(), model.clone()).unwrap();
    let call = ToolCall {
        id: "seed".into(),
        name: "code_mode".into(),
        arguments: json!({"code":"seed"}),
        raw_arguments: None,
    };
    session
        .record_assistant(turn, assistant(call), Usage::unknown(), false)
        .unwrap();
    let parent = session.tool_occurrence(turn, "seed").unwrap();
    session
        .record_child_intent(
            turn,
            ChildIntent {
                parent,
                child: 0,
                call: ToolCall {
                    id: "child".into(),
                    name: "native".into(),
                    arguments: json!({}),
                    raw_arguments: None,
                },
                definition: ToolSpec {
                    name: "native".into(),
                    description: String::new(),
                    input_schema: json!({"type":"object"}),
                },
                activity: ToolActivity::external("native"),
            },
        )
        .unwrap();
    session
        .record_child_outcome(
            turn,
            parent,
            0,
            ChildOutcome::Observed {
                output: ToolOutput {
                    value: json!({"small":1}),
                    images: vec![],
                    is_error: false,
                },
            },
        )
        .unwrap();
    session
        .record_tool_result(
            turn,
            ToolResult {
                call_id: "seed".into(),
                name: "code_mode".into(),
                result: Value::Null,
                images: vec![],
                is_error: false,
            },
        )
        .unwrap();
    session
        .end_turn(turn, TurnEndReason::Completed, &BTreeSet::new())
        .unwrap();
    let (turn, _) = session.begin_turn("inspect".into(), model).unwrap();
    let call = ToolCall {
        id: "inspect".into(),
        name: "code_mode".into(),
        arguments: json!({"code":json!({"kind":"output","parent":parent,"child":0,"pointer":"/small","limit":4}).to_string()}),
        raw_arguments: None,
    };
    session
        .record_assistant(turn, assistant(call.clone()), Usage::unknown(), false)
        .unwrap();
    (root, session, turn, call)
}

#[test]
fn inspection_has_capacity_independent_of_the_waiting_guest() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (root, session, turn, call) = fixture();
        let guest: Arc<dyn CodeRuntime> = Arc::new(Guest { await_reply: true });
        let catalog = ToolSet::new([]).snapshot();
        let output = run(
            &guest,
            CodeLimits {
                execution_budget: Duration::from_secs(1),
                ..CodeLimits::default()
            },
            &session,
            turn,
            &catalog,
            &call,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(!output.is_error, "{output:?}");
        assert_eq!(output.value["result"]["json"], "1");
        assert_eq!(output.value["calls"], 0, "inspection admitted an effect");
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[tokio::test(flavor = "current_thread")]
async fn unawaited_inspection_fault_blocks_parent_success() {
    let (root, session, turn, call) = fixture();
    rusqlite::Connection::open(session.path())
        .unwrap()
        .execute(
            "UPDATE entries SET body='invalid JSON' WHERE json_extract(CAST(body AS TEXT),'$.kind')='child_tool_result'",
            [],
        )
        .unwrap();
    let guest: Arc<dyn CodeRuntime> = Arc::new(Guest { await_reply: false });
    let catalog = ToolSet::new([]).snapshot();
    let output = run(
        &guest,
        CodeLimits::default(),
        &session,
        turn,
        &catalog,
        &call,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .await;
    assert!(matches!(output, Err(AgentError::Session(_))), "{output:?}");
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

use std::{collections::BTreeSet, time::Duration};

use ion_ai::{Content, Message, ModelRef, Role, ToolResult, ToolSpec, Usage};
use tokio::sync::mpsc;

use super::*;
use crate::{ToolActivity, ToolSet, session::TurnEndReason};

#[test]
fn selected_images_are_invocation_scoped_and_charged_before_copying() {
    let image = ion_ai::ImageContent::from_bytes(&[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ])
    .unwrap();
    let images = BTreeMap::from([("current".into(), image.clone())]);
    let root = || CodeResult {
        result: Ok("{\"n\":3}".into()),
        output: vec![
            CodeOutput::Text {
                text: "selected".into(),
            },
            CodeOutput::Image {
                id: "current".into(),
            },
        ],
    };
    let result = selected_output(root(), &images, CodeLimits::default());
    assert_eq!(
        result.value,
        json!({"result":{"n":3},"output":[{"type":"text","text":"selected"},{"type":"image","index":0}]})
    );
    assert_eq!(result.images, [image]);
    let bytes = encoded_len(&(&result.value, &result.images)).unwrap();
    for (limit, fails) in [(bytes - 1, true), (bytes, false)] {
        let result = selected_output(
            root(),
            &images,
            CodeLimits {
                max_output_bytes: limit,
                ..CodeLimits::default()
            },
        );
        assert_eq!(result.is_error, fails);
        assert_eq!(result.images.len(), usize::from(!fails));
    }
    assert!(selected_output(root(), &BTreeMap::new(), CodeLimits::default()).is_error);
    let mut repeated = root();
    repeated.output.push(CodeOutput::Image {
        id: "current".into(),
    });
    assert!(
        selected_output(
            repeated,
            &images,
            CodeLimits {
                max_output_bytes: bytes,
                ..CodeLimits::default()
            }
        )
        .is_error
    );
}

struct Guest {
    await_reply: bool,
}
impl CodeRuntime for Guest {
    fn start(
        &self,
        query: String,
        _tools_json: String,
        _limits: CodeLimits,
        stop: CancellationToken,
    ) -> Result<CodeTask, String> {
        let (sender, requests) = mpsc::channel(1);
        let await_reply = self.await_reply;
        let runtime = tokio::runtime::Handle::current();
        // An embedding runtime can retain a blocking-pool thread while awaiting
        // inspection. Core's reader must not need another thread from that pool.
        let result = tokio::task::spawn_blocking(move || {
            let result = runtime.block_on(async move {
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
            });
            CodeResult {
                result,
                output: Vec::new(),
            }
        });
        Ok(CodeTask {
            requests,
            result: Box::pin(async move { result.await.map_err(|error| error.to_string()) }),
        })
    }
}

fn assistant(call: ToolCall) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(call)],
        provider_replay: None,
    }
}

fn fixture() -> (std::path::PathBuf, Arc<Session>, u64, ToolCall) {
    let root = std::env::temp_dir().join(format!("ion-inspection-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
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
            Reasoning::ProviderDefault,
            &[],
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

#[tokio::test]
async fn namespace_and_replies_share_one_delivery_budget() {
    struct Describer;
    impl CodeRuntime for Describer {
        fn start(
            &self,
            _: String,
            tools_json: String,
            _: CodeLimits,
            _: CancellationToken,
        ) -> Result<CodeTask, String> {
            assert_eq!(tools_json, "[]");
            let (sender, requests) = mpsc::channel(1);
            let result = tokio::spawn(async move {
                let (reply, receive) = oneshot::channel();
                sender
                    .send(CodeRequest {
                        kind: CodeRequestKind::DescribeTool {
                            name: "missing".into(),
                        },
                        reply,
                    })
                    .await
                    .unwrap();
                CodeResult {
                    result: receive.await.unwrap(),
                    output: Vec::new(),
                }
            });
            Ok(CodeTask {
                requests,
                result: Box::pin(async move { result.await.map_err(|error| error.to_string()) }),
            })
        }
    }
    // Empty namespace is two encoded bytes; the missing description is `null`
    // (four). A five-byte total must fail even though either value fits alone.
    for (budget, succeeds) in [(5, false), (6, true)] {
        let (root, session, turn, call) = fixture();
        let entries = session.view().unwrap().entries.len();
        let guest: Arc<dyn CodeRuntime> = Arc::new(Describer);
        let output = run(
            &guest,
            CodeLimits {
                max_host_bytes: budget,
                ..CodeLimits::default()
            },
            &session,
            turn,
            &ToolSet::new([]).snapshot(),
            &call,
            Reasoning::ProviderDefault,
            &[],
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(!output.is_error, succeeds, "{output:?}");
        if succeeds {
            assert_eq!(output.value["result"], Value::Null);
        } else {
            assert!(
                output.value["error"]
                    .as_str()
                    .unwrap()
                    .contains("host reply byte limit")
            );
        }
        assert_eq!(session.view().unwrap().entries.len(), entries);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unawaited_inspection_fault_blocks_parent_success() {
    let (root, session, turn, call) = fixture();
    rusqlite::Connection::open(session.path())
        .unwrap()
        // Syntactically valid JSON passes derived indexes but is not a SessionEntry.
        .execute(
            "UPDATE entries SET body=CAST('{}' AS BLOB) WHERE json_extract(CAST(body AS TEXT),'$.kind')='child_tool_result'",
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
        Reasoning::ProviderDefault,
        &[],
        &CancellationToken::new(),
        &mut |_| {},
    )
    .await;
    assert!(matches!(output, Err(AgentError::Session(_))), "{output:?}");
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

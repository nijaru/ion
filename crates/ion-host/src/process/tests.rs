//! Process authority, driver and retention tests.
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, Usage,
};
use ion_core::{
    CodingAgent, CodingAgentError, CodingAgentEvent, CodingSession, CodingToolSource, SessionEntry,
    ToolOutcome, ToolSet,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::*;

fn response(calls: Vec<Content>) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: calls,
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}
fn call(id: &str, arguments: Value) -> Content {
    Content::ToolCall(ToolCall {
        id: id.into(),
        name: "process".into(),
        arguments,
        raw_arguments: None,
    })
}
fn text(value: &str) -> Content {
    Content::Text(value.into())
}
fn turn(calls: Vec<Content>, answer: &str) -> Vec<Script> {
    vec![response(calls), response(vec![text(answer)])]
}
fn model(scripts: Vec<Script>) -> Arc<ScriptedModelService> {
    Arc::new(ScriptedModelService::new(scripts))
}
fn tool_set(tools: Arc<ProcessTools>) -> Arc<ToolSet> {
    Arc::new(ToolSet::new([tools as Arc<dyn CodingToolSource>]))
}
fn agent(service: Arc<ScriptedModelService>, tools: Arc<ProcessTools>) -> CodingAgent {
    CodingAgent::with_tool_set(
        service,
        tool_set(tools),
        ModelRef {
            provider: "test".into(),
            model: "process".into(),
        },
    )
}
async fn submit(
    agent: &CodingAgent,
    session: &Arc<CodingSession>,
    input: &str,
) -> anyhow::Result<()> {
    agent
        .submit(
            session.clone(),
            input.into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await?;
    Ok(())
}
fn process_outputs(session: &CodingSession) -> Vec<Value> {
    session
        .view()
        .unwrap()
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } if result.name == "process" => {
                match &result.outcome {
                    ToolOutcome::Observed { output, .. } => Some(output.value.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}
fn started_id(output: &Value) -> Uuid {
    Uuid::parse_str(
        output["id"]
            .as_str()
            .expect("start returned an owner handle"),
    )
    .expect("uuid handle")
}
async fn until(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "effect did not appear at {path:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn core_workflow_survives_turns_and_exposes_actual_pipe_input_and_output() {
    let root = std::env::temp_dir().join(format!("ion-process-workflow-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = "printf 'READY\\n'; printf effect > marker; while IFS= read -r line; do printf 'GOT:%s\\n' \"$line\"; done; printf 'ERR\\n' >&2; exit 3";
    let first = agent(
        model(turn(
            vec![call("start", json!({"action":"start","command":command}))],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&first, &session, "first").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    until(&root.join("marker")).await;

    // A fresh binding over the same owner and Session controls the old handle.
    let second = agent(
        model(turn(
            vec![
                call("write", json!({"action":"write","id":id,"data":"hello\n"})),
                call("eof", json!({"action":"eof","id":id})),
                call("wait", json!({"action":"wait","id":id,"timeout_ms":10000})),
                call(
                    "out",
                    json!({"action":"read","id":id,"offset":0,"encoding":"utf8"}),
                ),
                call("err", json!({"action":"read","id":id,"stream":"stderr"})),
                call("full", json!({"action":"start","command":"printf second"})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "done",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&second, &session, "second").await.unwrap();
    let outputs = process_outputs(&session);
    // Join even on a regression failure so a failed EOF assertion cannot leave
    // the owned test command running after its observation deadline.
    owner.join_shutdown().await.unwrap();
    let write = &outputs[1];
    assert_eq!(write["bytes_written"], 6, "{write}");
    assert_eq!(write["cancelled"], false, "{write}");
    assert_eq!(outputs[2]["cancelled"], false, "{outputs:?}");
    let wait = &outputs[3];
    assert_eq!(wait["joined"], true, "{wait}");
    assert_eq!(wait["timed_out"], false, "{wait}");
    assert_eq!(wait["exit"]["exit_code"], 3, "{wait}");
    let stdout = &outputs[4];
    assert_eq!(stdout["complete"], true, "{stdout}");
    assert_eq!(stdout["gap_bytes"], 0, "{stdout}");
    assert!(
        stdout["text"]
            .as_str()
            .unwrap()
            .contains("READY\nGOT:hello\n"),
        "{stdout}"
    );
    assert_eq!(outputs[5]["text"], "ERR\n", "{}", outputs[5]);
    assert!(
        outputs[6]["error"].as_str().unwrap().contains("capacity"),
        "{}",
        outputs[6]
    );
    assert_eq!(outputs[7]["acknowledged"], true, "{}", outputs[7]);
    assert!(root.join("marker").exists());
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_wait_does_not_kill_and_close_really_joins() {
    let root = std::env::temp_dir().join(format!("ion-process-cancel-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let first = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","command":"printf ready > ready; exec sleep 30"}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&first, &session, "first").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    until(&root.join("ready")).await;

    let stop = CancellationToken::new();
    let trigger = stop.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let mut started_tx = Some(started_tx);
    let waiting = agent(
        model(vec![
            response(vec![call(
                "wait",
                json!({"action":"wait","id":id,"timeout_ms":30000}),
            )]),
            response(vec![text("unreachable after cancellation")]),
        ]),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    let result = waiting
        .submit(
            session.clone(),
            "wait".into(),
            String::new(),
            stop,
            move |event| {
                if let CodingAgentEvent::ToolStarted { name, .. } = event
                    && name == "process"
                    && let Some(sender) = started_tx.take()
                {
                    trigger.cancel();
                    let _ = sender.send(());
                }
            },
        )
        .await;
    started_rx.await.unwrap();
    assert!(
        matches!(result, Err(CodingAgentError::Cancelled)),
        "{result:?}"
    );
    let cancelled = process_outputs(&session).pop().unwrap();
    assert_eq!(cancelled["cancelled"], true, "{cancelled}");

    // The wait cancellation did not stop the process. A later explicit close
    // does, and its joined=true means the driver and readers actually settled.
    let closer = agent(
        model(turn(
            vec![
                call("close", json!({"action":"close","id":id})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "closed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&closer, &session, "close").await.unwrap();
    let outputs = process_outputs(&session);
    let close = outputs
        .iter()
        .find(|output| output["joined"] == true && output["exit"].is_object())
        .expect("close snapshot");
    assert_eq!(close["cancelled"], false, "{close}");
    assert_eq!(close["exit"]["signal"], 15, "{close}");
    assert_eq!(outputs.last().unwrap()["acknowledged"], true);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn absent_and_foreign_authority_reject_before_effects() {
    let root = std::env::temp_dir().join(format!("ion-process-authority-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let tools = Arc::new(ProcessTools::new(&root, owner.clone()).unwrap());
    let catalog = tool_set(tools).snapshot();
    let start = ToolCall {
        id: "untracked".into(),
        name: "process".into(),
        arguments: json!({"action":"start","command":"printf effect > marker"}),
        raw_arguments: None,
    };
    let output = catalog
        .execute_untracked(&start, CancellationToken::new())
        .await;
    assert!(output.is_error);
    assert!(!root.join("marker").exists());

    let session_a = Arc::new(CodingSession::create(root.join("a.sqlite"), Some(&root)).unwrap());
    let first = agent(
        model(turn(
            vec![
                call(
                    "invalid",
                    json!({"action":"start","command":"printf nope > invalid-marker","unknown":true}),
                ),
                call(
                    "start",
                    json!({"action":"start","command":"printf started > marker; exec sleep 30"}),
                ),
            ],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&first, &session_a, "first").await.unwrap();
    assert!(
        !root.join("invalid-marker").exists(),
        "invalid start had an effect"
    );
    let id = started_id(&process_outputs(&session_a)[1]);
    until(&root.join("marker")).await;

    let session_b = Arc::new(CodingSession::create(root.join("b.sqlite"), Some(&root)).unwrap());
    let foreign = agent(
        model(turn(
            vec![call("close", json!({"action":"close","id":id}))],
            "foreign",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&foreign, &session_b, "foreign").await.unwrap();
    let foreign_output = process_outputs(&session_b)[0].clone();
    assert_eq!(foreign_output["error"], unavailable(), "{foreign_output}");

    let owner_session = agent(
        model(turn(
            vec![
                call("inspect", json!({"action":"inspect","id":id})),
                call("null", json!({"action":"wait","id":id,"timeout_ms":null})),
            ],
            "inspect",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&owner_session, &session_a, "inspect").await.unwrap();
    let outputs = process_outputs(&session_a);
    assert_eq!(outputs[2]["running"], true, "{}", outputs[2]);
    assert!(
        outputs[3]["error"]
            .as_str()
            .unwrap()
            .contains("invalid process arguments"),
        "{}",
        outputs[3]
    );

    // The original owner can still close it: the foreign attempt neither
    // attached nor stopped the process.
    let close = agent(
        model(turn(
            vec![
                call("close", json!({"action":"close","id":id})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "closed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&close, &session_a, "close").await.unwrap();
    let outputs = process_outputs(&session_a);
    let snapshot = outputs
        .iter()
        .rev()
        .find(|output| output["joined"] == true && output["exit"].is_object())
        .unwrap();
    assert_eq!(snapshot["exit"]["signal"], 15, "{snapshot}");
    owner.join_shutdown().await.unwrap();
    session_a.shutdown().await.unwrap();
    session_b.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn output_flood_reports_gaps_and_direct_exit_does_not_wait_for_held_pipes() {
    let root = std::env::temp_dir().join(format!("ion-process-flood-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(2).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let first = agent(
        model(turn(
            vec![
                call(
                    "flood",
                    json!({"action":"start","command":"head -c 70000 /dev/zero | tr '\\000' x; printf '\\nEND\\n'"}),
                ),
                call(
                    "held",
                    json!({"action":"start","command":"sleep 1 & printf 'READY\\n'"}),
                ),
            ],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&first, &session, "first").await.unwrap();
    let outputs = process_outputs(&session);
    let flood = started_id(&outputs[0]);
    let held = started_id(&outputs[1]);
    let second = agent(
        model(turn(
            vec![
                call(
                    "wait_flood",
                    json!({"action":"wait","id":flood,"timeout_ms":10000}),
                ),
                call(
                    "read_flood",
                    json!({"action":"read","id":flood,"limit":65536}),
                ),
                call(
                    "wait_held",
                    json!({"action":"wait","id":held,"timeout_ms":10000}),
                ),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    let started = Instant::now();
    submit(&second, &session, "second").await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    let outputs = process_outputs(&session);
    let wait_flood = &outputs[2];
    assert_eq!(wait_flood["joined"], true);
    assert_eq!(wait_flood["exit"]["exit_code"], 0);
    let flood_read = &outputs[3];
    assert_eq!(flood_read["complete"], true, "{flood_read}");
    assert_eq!(flood_read["gap_bytes"], 4469, "{flood_read}");
    assert_eq!(flood_read["first_available"], 4469, "{flood_read}");
    assert!(flood_read["text"].as_str().unwrap().ends_with("\nEND\n"));
    let held_wait = &outputs[4];
    assert_eq!(held_wait["joined"], true, "{held_wait}");
    assert_eq!(held_wait["exit"]["exit_code"], 0, "{held_wait}");
    assert_eq!(held_wait["stdout"]["complete"], false, "{held_wait}");
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_input_reports_the_actual_written_prefix() {
    let root = std::env::temp_dir().join(format!("ion-process-input-cancel-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let first = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","command":"exec sleep 30"}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&first, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);

    let stop = CancellationToken::new();
    let trigger = stop.clone();
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = starts.clone();
    let input = agent(
        model(turn(
            vec![
                call(
                    "first",
                    json!({"action":"write","id":id,"data":"x".repeat(8192)}),
                ),
                call(
                    "second",
                    json!({"action":"write","id":id,"data":"y".repeat(65536)}),
                ),
            ],
            "input",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    let result = input
        .submit(
            session.clone(),
            "input".into(),
            String::new(),
            stop,
            move |event| {
                if let CodingAgentEvent::ToolStarted { name, .. } = event
                    && name == "process"
                    && observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
                {
                    let trigger = trigger.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        trigger.cancel();
                    });
                }
            },
        )
        .await;
    assert!(
        matches!(result, Err(CodingAgentError::Cancelled)),
        "{result:?}"
    );
    let outputs = process_outputs(&session);
    assert_eq!(outputs[1]["bytes_written"], 8192, "{}", outputs[1]);
    let partial = &outputs[2];
    assert_eq!(partial["cancelled"], true, "{partial}");
    assert_eq!(partial["enqueued"], true, "{partial}");
    let written = partial["bytes_written"].as_u64().unwrap();
    assert!(
        (0..65536).contains(&written),
        "cancelled write must report its actual partial prefix: {partial}"
    );

    let close = agent(
        model(turn(
            vec![
                call("close", json!({"action":"close","id":id})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "closed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&close, &session, "close").await.unwrap();
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ending_a_child_scope_joins_and_reclaims_capacity_for_the_parent() {
    let root = std::env::temp_dir().join(format!("ion-process-scope-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let tools = Arc::new(ProcessTools::new(&root, owner.clone()).unwrap());
    let child = Arc::new(CodingSession::create(root.join("child.sqlite"), Some(&root)).unwrap());
    let child_agent = agent(
        model(turn(
            vec![call(
                "start",
                json!({
                    "action":"start", "command":"touch child-ready; exec sleep 30"
                }),
            )],
            "child done",
        )),
        tools.clone(),
    );
    submit(&child_agent, &child, "child").await.unwrap();
    until(&root.join("child-ready")).await;
    child.shutdown().await.unwrap();
    owner.close_session(child.id()).await.unwrap();

    let parent = Arc::new(CodingSession::create(root.join("parent.sqlite"), Some(&root)).unwrap());
    let parent_agent = agent(
        model(turn(
            vec![call(
                "start",
                json!({
                    "action":"start", "command":"printf reused > reused"
                }),
            )],
            "parent done",
        )),
        tools,
    );
    submit(&parent_agent, &parent, "parent").await.unwrap();
    let output = process_outputs(&parent).pop().unwrap();
    until(&root.join("reused")).await;
    owner.join_shutdown().await.unwrap();
    parent.shutdown().await.unwrap();
    assert_eq!(output["started"], true, "{output}");
    assert!(output.get("error").is_none(), "{output}");
    assert_eq!(std::fs::read(root.join("reused")).unwrap(), b"reused");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn owner_shutdown_stops_and_joins_running_processes() {
    let root = std::env::temp_dir().join(format!("ion-process-shutdown-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","command":"printf ready > ready; exec sleep 30"}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    until(&root.join("ready")).await;
    owner.request_shutdown();
    let (first, second) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(owner.join_shutdown(), owner.join_shutdown())
    })
    .await
    .expect("shutdown did not join");
    first.unwrap();
    second.unwrap();
    let observer = agent(
        model(turn(
            vec![call(
                "inspect",
                json!({
                    "action":"inspect", "id":id
                }),
            )],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "inspect joined shutdown")
        .await
        .unwrap();
    let observed = process_outputs(&session).pop().unwrap();
    assert_eq!(observed["joined"], true, "{observed}");
    assert_eq!(observed["exit"]["signal"], 15, "{observed}");
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

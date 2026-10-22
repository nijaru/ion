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
async fn until_file(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(path).is_ok_and(|contents| contents.trim() == expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "file {path:?} did not contain {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn pipe_and_pty_keep_the_starting_environment_across_binding_replacement() {
    for backend in ["pipe", "pty"] {
        let root = std::env::temp_dir().join(format!("ion-process-env-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let owner = Arc::new(ProcessOwner::new(2).unwrap());
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let environment = |version: &str| {
            Arc::new(crate::CommandEnvironment::new(
            "/bin/bash".into(),
            [("VIRTUAL_ENV".into(), version.into()), ("KITTY_WINDOW_ID".into(), "outer".into())].into(),
        ).unwrap().with_setup("export FROM_SETUP=ready; probe() { printf '%s/%s' \"$VIRTUAL_ENV\" \"$FROM_SETUP\"; }".into()).unwrap())
        };
        let first = agent(
            model(turn(
                vec![call(
                    "start",
                    json!({
                        "action":"start", "backend":backend,
                        "command":"IFS= read -r line; probe > observed; printf '%s' \"${KITTY_WINDOW_ID-unset}\" > terminal"
                    }),
                )],
                "started",
            )),
            Arc::new(
                ProcessTools::with_environment(&root, owner.clone(), environment("old")).unwrap(),
            ),
        );
        submit(&first, &session, "start").await.unwrap();
        let id = started_id(&process_outputs(&session)[0]);
        let replacement = agent(
            model(turn(
                vec![
                    call("input", json!({"action":"write","id":id,"data":"go\n"})),
                    call("join", json!({"action":"wait","id":id,"timeout_ms":10000})),
                ],
                "joined",
            )),
            Arc::new(
                ProcessTools::with_environment(&root, owner.clone(), environment("new")).unwrap(),
            ),
        );
        submit(&replacement, &session, "finish").await.unwrap();
        owner.join_shutdown().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("observed")).unwrap(),
            "old/ready"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("terminal")).unwrap(),
            if backend == "pty" { "unset" } else { "outer" }
        );
        std::fs::remove_dir_all(root).unwrap();
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
async fn cancelled_close_reports_incomplete_join_and_retains_stop() {
    let root = std::env::temp_dir().join(format!("ion-process-close-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let tools = Arc::new(ProcessTools::new(&root, owner.clone()).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let starter = agent(
        model(turn(
            vec![call(
                "start",
                json!({
                    "action":"start", "command":"trap '' TERM; printf ready > ready; exec sleep 30"
                }),
            )],
            "started",
        )),
        tools.clone(),
    );
    submit(&starter, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    until(&root.join("ready")).await;

    let stop = CancellationToken::new();
    let trigger = stop.clone();
    let closer = agent(
        model(vec![response(vec![call(
            "close",
            json!({"action":"close","id":id}),
        )])]),
        tools.clone(),
    );
    let result = closer
        .submit(
            session.clone(),
            "close".into(),
            String::new(),
            stop,
            move |event| {
                if matches!(event, CodingAgentEvent::ToolStarted { .. }) {
                    trigger.cancel();
                }
            },
        )
        .await;
    let close = session
        .view()
        .unwrap()
        .entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } if result.call_id == "close" => {
                match &result.outcome {
                    ToolOutcome::Observed { output, .. } => Some(output.clone()),
                    _ => None,
                }
            }
            _ => None,
        });
    let waiter = agent(
        model(turn(
            vec![
                call("wait", json!({"action":"wait","id":id,"timeout_ms":5000})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "joined",
        )),
        tools,
    );
    let joined = submit(&waiter, &session, "join").await;
    let outputs = process_outputs(&session);
    // Settle owned work before assertions so the regression cannot leak a process.
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();

    assert!(
        matches!(result, Err(CodingAgentError::Cancelled)),
        "{result:?}"
    );
    joined.unwrap();
    let close = close.expect("close observation");
    assert_eq!(close.value["cancelled"], true, "{close:?}");
    assert_eq!(close.value["joined"], false, "{close:?}");
    assert!(
        close.is_error,
        "cancelled close must not report successful settlement: {close:?}"
    );
    let wait = &outputs[2];
    assert_eq!(wait["joined"], true, "{wait}");
    assert_eq!(
        wait["exit"]["signal"], 9,
        "stop must survive waiter cancellation: {wait}"
    );
    assert_eq!(outputs[3]["acknowledged"], true);
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
                    "action":"start", "backend":"pty",
                    "command":"touch child-ready; exec sleep 30"
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
    // Shell redirection creates the file before printf writes. Do not stop
    // the reused slot until its independent native effect is observable.
    until_file(&root.join("reused"), "reused").await;
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

#[tokio::test]
async fn core_pty_start_has_a_real_controlling_terminal_and_dimensions() {
    let root = std::env::temp_dir().join(format!("ion-pty-contract-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = concat!(
        "i=n o=n e=n d=n; ",
        "[ -t 0 ] && i=y; [ -t 1 ] && o=y; [ -t 2 ] && e=y; ",
        "[ -c /dev/tty ] && d=y; ",
        "printf 'in=%s out=%s err=%s ttydev=%s\\n' \"$i\" \"$o\" \"$e\" \"$d\"; ",
        "printf 'tty=%s\\n' \"$(tty)\"; ",
        "printf 'size=%s\\n' \"$(stty size)\"; ",
        "printf 'pgid=%s tpgid=%s pid=%s\\n' ",
        "\"$(ps -o pgid= -p $$ | tr -d ' ')\" ",
        "\"$(ps -o tpgid= -p $$ | tr -d ' ')\" \"$$\"; ",
        "printf 'term=%s\\n' \"$TERM\""
    );
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({
                    "action":"start", "backend":"pty", "rows":30, "columns":100,
                    "command": command
                }),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let start = process_outputs(&session)[0].clone();
    assert_eq!(start["backend"], "pty", "{start}");
    assert_eq!(start["screen"]["available"], true, "{start}");
    assert_eq!(start["screen"]["rows"], 30, "{start}");
    assert_eq!(start["screen"]["columns"], 100, "{start}");
    assert!(start.get("stdout").is_none(), "{start}");
    assert!(start.get("stderr").is_none(), "{start}");
    let id = started_id(&start);

    let observer = agent(
        model(turn(
            vec![
                call("wait", json!({"action":"wait","id":id,"timeout_ms":10000})),
                call(
                    "raw",
                    json!({"action":"read","id":id,"stream":"terminal","limit":65536}),
                ),
                call("screen", json!({"action":"screen","id":id,"rows":24})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    let wait = &outputs[1];
    assert_eq!(wait["joined"], true, "{wait}");
    assert_eq!(wait["timed_out"], false, "{wait}");
    assert_eq!(wait["exit"]["exit_code"], 0, "{wait}");
    assert_eq!(wait["terminal"]["complete"], true, "{wait}");
    let raw = &outputs[2];
    let text = raw["text"].as_str().unwrap();
    assert!(text.contains("in=y out=y err=y ttydev=y"), "{text}");
    assert!(text.contains("size=30 100"), "{text}");
    assert!(text.contains("term=xterm-256color"), "{text}");
    assert!(text.contains("tty=/dev/"), "{text}");
    let pgrp = text
        .lines()
        .find(|line| line.starts_with("pgid="))
        .expect("pgrp observation");
    let values = pgrp
        .split_whitespace()
        .map(|part| part.split_once('=').unwrap().1)
        .collect::<Vec<_>>();
    assert_eq!(
        values[0], values[1],
        "foreground group is not the shell: {pgrp}"
    );
    assert_eq!(
        values[1], values[2],
        "shell is not its own process-group leader: {pgrp}"
    );
    let screen = &outputs[3];
    assert_eq!(screen["visible_rows"], 24, "{screen}");
    assert_eq!(screen["rows"], 30, "{screen}");
    assert_eq!(screen["columns"], 100, "{screen}");
    assert!(
        screen["text"].as_str().unwrap().contains("size=30 100"),
        "{screen}"
    );
    assert_eq!(outputs[4]["acknowledged"], true, "{}", outputs[4]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pty_screen_projects_alternate_tui_split_unicode_and_input_modes() {
    let root = std::env::temp_dir().join(format!("ion-pty-screen-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = concat!(
        "printf 'MAIN\\033[31mRED\\033[0m\\033[3;5H'; ",
        "printf '\\033[?1049h'; ",
        "printf '\\033[2;3HALT'; ",
        "printf '\\033[?1h\\033=\\033[?2004h'; ",
        "printf '\\033[6;1H\\303'; sleep 0.05; printf '\\251'"
    );
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":command}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    let observer = agent(
        model(turn(
            vec![
                call("wait", json!({"action":"wait","id":id,"timeout_ms":10000})),
                call(
                    "raw",
                    json!({"action":"read","id":id,"stream":"terminal","limit":65536}),
                ),
                call("screen", json!({"action":"screen","id":id,"rows":24})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    let raw = outputs[2]["text"].as_str().unwrap();
    assert!(raw.contains("MAIN"), "{raw:?}");
    assert!(raw.contains("\u{1b}[?1049h"), "{raw:?}");
    assert!(raw.contains("\u{1b}[31m"), "{raw:?}");
    let screen = &outputs[3];
    assert_eq!(screen["alternate_screen"], true, "{screen}");
    assert_eq!(screen["application_cursor"], true, "{screen}");
    assert_eq!(screen["application_keypad"], true, "{screen}");
    assert_eq!(screen["bracketed_paste"], true, "{screen}");
    assert_eq!(screen["cursor_row"], 5, "{screen}");
    assert_eq!(screen["cursor_column"], 1, "{screen}");
    let text = screen["text"].as_str().unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines[1], "  ALT", "{screen}");
    assert_eq!(lines[5], "\u{e9}", "{screen}");
    assert!(screen["source_bytes"].as_u64().unwrap() > 0, "{screen}");
    assert_eq!(outputs[4]["acknowledged"], true, "{}", outputs[4]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pty_program_dsr_receives_the_automatic_cursor_reply() {
    let root = std::env::temp_dir().join(format!("ion-pty-dsr-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = concat!(
        "stty raw -echo; ",
        "printf '\\033[4;7H\\033[6n'; ",
        "IFS= read -r -d R response; ",
        "printf '%s' \"$response\" > reply; ",
        "printf 'REPLY:%s\\n' \"$response\""
    );
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":command}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    let observer = agent(
        model(turn(
            vec![
                call("wait", json!({"action":"wait","id":id,"timeout_ms":10000})),
                call("screen", json!({"action":"screen","id":id,"rows":8})),
                call(
                    "raw",
                    json!({"action":"read","id":id,"stream":"terminal","limit":65536}),
                ),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    let wait = &outputs[1];
    assert_eq!(wait["joined"], true, "{wait}");
    assert_eq!(wait["exit"]["exit_code"], 0, "{wait}");
    assert_eq!(std::fs::read(root.join("reply")).unwrap(), b"\x1b[4;7");
    let raw = outputs[3]["text"].as_str().unwrap();
    assert!(raw.contains("REPLY:\u{1b}[4;7"), "{raw:?}");
    assert_eq!(outputs[4]["acknowledged"], true, "{}", outputs[4]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pty_resize_updates_the_tty_projection_and_signals_the_foreground_group() {
    let root = std::env::temp_dir().join(format!("ion-pty-resize-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = concat!(
        "trap 'stty size > winch; printf \"SIGWINCH\\n\"' WINCH; ",
        "stty size > initial; ",
        "printf 'READY\\n'; ",
        "while :; do sleep 0.02; done"
    );
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({
                    "action":"start", "backend":"pty", "rows":24, "columns":80,
                    "command": command
                }),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    until_file(&root.join("initial"), "24 80").await;

    let resizer = agent(
        model(turn(
            vec![call(
                "resize",
                json!({"action":"resize","id":id,"rows":12,"columns":40}),
            )],
            "resized",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&resizer, &session, "resize").await.unwrap();
    let outputs = process_outputs(&session);
    let resize = &outputs[1];
    assert_eq!(resize["resized"], true, "{resize}");
    assert_eq!(resize["rows"], 12, "{resize}");
    assert_eq!(resize["columns"], 40, "{resize}");
    assert_eq!(resize["screen"]["rows"], 12, "{resize}");
    assert_eq!(resize["screen"]["columns"], 40, "{resize}");
    until_file(&root.join("winch"), "12 40").await;

    let observer = agent(
        model(turn(
            vec![
                call("screen", json!({"action":"screen","id":id,"rows":12})),
                call("close", json!({"action":"close","id":id})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    let screen = &outputs[2];
    assert_eq!(screen["rows"], 12, "{screen}");
    assert_eq!(screen["columns"], 40, "{screen}");
    let text = screen["text"].as_str().unwrap();
    assert!(text.contains("SIGWINCH"), "{screen}");
    let close = &outputs[3];
    assert_eq!(close["joined"], true, "{close}");
    assert_eq!(close["exit"]["signal"], 15, "{close}");
    assert_eq!(close["terminal"]["complete"], true, "{close}");
    assert_eq!(outputs[4]["acknowledged"], true, "{}", outputs[4]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_dimensions_and_cross_backend_actions_reject_without_effects() {
    let root = std::env::temp_dir().join(format!("ion-pty-invalid-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(2).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let first = agent(
        model(turn(
            vec![
                call(
                    "pipe_dims",
                    json!({"action":"start","backend":"pipe","rows":24,"command":"printf bad > bad-pipe"}),
                ),
                call(
                    "pty_zero",
                    json!({"action":"start","backend":"pty","rows":0,"command":"printf bad > bad-zero"}),
                ),
                call(
                    "pty_cells",
                    json!({"action":"start","backend":"pty","rows":200,"columns":200,"command":"printf bad > bad-cells"}),
                ),
                call(
                    "pipe",
                    json!({"action":"start","command":"printf pipe > pipe-marker; exec sleep 30"}),
                ),
                call(
                    "pty",
                    json!({"action":"start","backend":"pty","command":"printf pty > pty-marker; exec sleep 30"}),
                ),
            ],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&first, &session, "start").await.unwrap();
    let outputs = process_outputs(&session);
    assert!(
        outputs[0]["error"]
            .as_str()
            .unwrap()
            .contains("does not accept terminal dimensions"),
        "{}",
        outputs[0]
    );
    assert!(
        outputs[1]["error"].as_str().unwrap().contains("rows"),
        "{}",
        outputs[1]
    );
    assert!(
        outputs[2]["error"].as_str().unwrap().contains("maximum"),
        "{}",
        outputs[2]
    );
    assert!(!root.join("bad-pipe").exists());
    assert!(!root.join("bad-zero").exists());
    assert!(!root.join("bad-cells").exists());
    let pipe = started_id(&outputs[3]);
    let pty = started_id(&outputs[4]);
    until(&root.join("pipe-marker")).await;
    until(&root.join("pty-marker")).await;

    let second = agent(
        model(turn(
            vec![
                call(
                    "pipe_resize",
                    json!({"action":"resize","id":pipe,"rows":12,"columns":40}),
                ),
                call(
                    "pipe_terminal",
                    json!({"action":"read","id":pipe,"stream":"terminal"}),
                ),
                call("pty_eof", json!({"action":"eof","id":pty})),
                call(
                    "pty_stdout",
                    json!({"action":"read","id":pty,"stream":"stdout"}),
                ),
                call(
                    "pty_bad_resize",
                    json!({"action":"resize","id":pty,"rows":201,"columns":80}),
                ),
                call("inspect_pty", json!({"action":"inspect","id":pty})),
                call("close_pipe", json!({"action":"close","id":pipe})),
                call("close_pty", json!({"action":"close","id":pty})),
                call("ack_pipe", json!({"action":"acknowledge","id":pipe})),
                call("ack_pty", json!({"action":"acknowledge","id":pty})),
            ],
            "checked",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&second, &session, "check").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    assert!(
        outputs[5]["error"]
            .as_str()
            .unwrap()
            .contains("only available for PTY"),
        "{}",
        outputs[5]
    );
    assert!(
        outputs[6]["error"]
            .as_str()
            .unwrap()
            .contains("only available for PTY"),
        "{}",
        outputs[6]
    );
    assert!(
        outputs[7]["error"].as_str().unwrap().contains("PTY EOF"),
        "{}",
        outputs[7]
    );
    assert!(
        outputs[8]["error"]
            .as_str()
            .unwrap()
            .contains("merged terminal stream"),
        "{}",
        outputs[8]
    );
    assert!(
        outputs[9]["error"].as_str().unwrap().contains("rows"),
        "{}",
        outputs[9]
    );
    let inspect = &outputs[10];
    assert_eq!(inspect["backend"], "pty", "{inspect}");
    assert_eq!(inspect["running"], true, "{inspect}");
    assert_eq!(inspect["screen"]["available"], true, "{inspect}");
    assert_eq!(
        inspect["screen"]["rows"], 24,
        "invalid resize took effect: {inspect}"
    );
    assert_eq!(
        inspect["screen"]["columns"], 80,
        "invalid resize took effect: {inspect}"
    );
    assert_eq!(outputs[11]["joined"], true, "{}", outputs[11]);
    assert_eq!(outputs[12]["joined"], true, "{}", outputs[12]);
    assert_eq!(outputs[13]["acknowledged"], true, "{}", outputs[13]);
    assert_eq!(outputs[14]["acknowledged"], true, "{}", outputs[14]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn oversized_control_string_keeps_raw_terminal_bytes_and_disables_screen() {
    let root = std::env::temp_dir().join(format!("ion-pty-osc-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = concat!(
        "printf '\\033]0;'; head -c 5000 /dev/zero | tr '\\000' x; ",
        "printf '\\007'; printf 'AFTER\\n'"
    );
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":command}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    let observer = agent(
        model(turn(
            vec![
                call("wait", json!({"action":"wait","id":id,"timeout_ms":10000})),
                call("screen", json!({"action":"screen","id":id,"rows":24})),
                call(
                    "resize",
                    json!({"action":"resize","id":id,"rows":12,"columns":40}),
                ),
                call(
                    "raw",
                    json!({"action":"read","id":id,"stream":"terminal","limit":65536}),
                ),
                call("inspect", json!({"action":"inspect","id":id})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    assert_eq!(outputs[1]["joined"], true, "{}", outputs[1]);
    assert_eq!(outputs[1]["exit"]["exit_code"], 0, "{}", outputs[1]);
    assert!(
        outputs[2]["error"]
            .as_str()
            .unwrap()
            .contains("screen projection unavailable"),
        "{}",
        outputs[2]
    );
    assert!(outputs[3]["error"].is_string(), "{}", outputs[3]);
    let raw = &outputs[4];
    let text = raw["text"].as_str().unwrap();
    assert!(text.starts_with("\u{1b}]0;"), "{:?}", &text[..20]);
    assert!(text.contains(&"x".repeat(5000)), "{raw}");
    assert!(text.ends_with("AFTER\r\n"), "{raw}");
    assert_eq!(raw["complete"], true, "{raw}");
    let inspect = &outputs[5];
    assert_eq!(inspect["screen"]["available"], false, "{inspect}");
    assert!(
        inspect["screen"]["reason"]
            .as_str()
            .unwrap()
            .contains("4096"),
        "{inspect}"
    );
    assert_eq!(inspect["terminal"]["complete"], true, "{inspect}");
    assert_eq!(outputs[6]["acknowledged"], true, "{}", outputs[6]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn application_shutdown_stops_and_joins_a_live_pty() {
    let root = std::env::temp_dir().join(format!("ion-pty-shutdown-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({
                    "action":"start", "backend":"pty",
                    "command":"touch pty-ready; exec sleep 30"
                }),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    until(&root.join("pty-ready")).await;
    owner.request_shutdown();
    let (first, second) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(owner.join_shutdown(), owner.join_shutdown())
    })
    .await
    .expect("PTY shutdown did not join");
    first.unwrap();
    second.unwrap();

    let observer = agent(
        model(turn(
            vec![
                call("inspect", json!({"action":"inspect","id":id})),
                call("screen", json!({"action":"screen","id":id})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    let inspect = &outputs[1];
    assert_eq!(inspect["joined"], true, "{inspect}");
    assert_eq!(inspect["exit"]["signal"], 15, "{inspect}");
    assert_eq!(inspect["terminal"]["complete"], true, "{inspect}");
    assert_eq!(inspect["screen"]["available"], true, "{inspect}");
    assert_eq!(outputs[2]["visible_rows"], 24, "{}", outputs[2]);
    assert_eq!(outputs[3]["acknowledged"], true, "{}", outputs[3]);
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn exited_pty_screen_remains_stable_until_ack_and_releases_capacity() {
    let root = std::env::temp_dir().join(format!("ion-pty-ack-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let tools = Arc::new(ProcessTools::new(&root, owner.clone()).unwrap());
    let first = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":"printf 'STABLE\\n'"}),
            )],
            "started",
        )),
        tools.clone(),
    );
    submit(&first, &session, "first").await.unwrap();
    let first_id = started_id(&process_outputs(&session)[0]);
    let observer = agent(
        model(turn(
            vec![
                call(
                    "wait",
                    json!({"action":"wait","id":first_id,"timeout_ms":10000}),
                ),
                call("screen", json!({"action":"screen","id":first_id})),
                call("ack", json!({"action":"acknowledge","id":first_id})),
            ],
            "observed",
        )),
        tools.clone(),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);
    assert!(
        outputs[2]["text"].as_str().unwrap().contains("STABLE"),
        "{}",
        outputs[2]
    );
    assert_eq!(outputs[3]["acknowledged"], true, "{}", outputs[3]);

    let second = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":"printf 'SECOND\\n'"}),
            )],
            "started",
        )),
        tools.clone(),
    );
    submit(&second, &session, "second").await.unwrap();
    let second_start = process_outputs(&session).pop().unwrap();
    assert_eq!(second_start["started"], true, "{second_start}");
    assert!(second_start.get("error").is_none(), "{second_start}");
    let second_id = started_id(&second_start);
    let observer = agent(
        model(turn(
            vec![
                call(
                    "wait",
                    json!({"action":"wait","id":second_id,"timeout_ms":10000}),
                ),
                call("screen", json!({"action":"screen","id":second_id})),
                call("ack", json!({"action":"acknowledge","id":second_id})),
            ],
            "observed",
        )),
        tools,
    );
    submit(&observer, &session, "observe second").await.unwrap();
    let outputs = process_outputs(&session);
    assert!(
        outputs.last().unwrap()["acknowledged"] == true,
        "{outputs:?}"
    );
    assert!(
        outputs[outputs.len() - 2]["text"]
            .as_str()
            .unwrap()
            .contains("SECOND"),
        "{outputs:?}"
    );
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn interactive_bash_reads_a_command_and_exits_in_the_owned_session() {
    let root = std::env::temp_dir().join(format!("ion-pty-bash-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let command = format!(
        "HOME='{}' HISTFILE=/dev/null bash --noprofile --norc -i",
        root.display()
    );
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":command}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    let data = "printf 'INTERACTIVE_OK\\n'; exit\n";
    let input = agent(
        model(turn(
            vec![
                call("empty", json!({"action":"write","id":id,"data":""})),
                call("write", json!({"action":"write","id":id,"data":data})),
                call("wait", json!({"action":"wait","id":id,"timeout_ms":10000})),
                call(
                    "raw",
                    json!({"action":"read","id":id,"stream":"terminal","limit":65536}),
                ),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "input",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&input, &session, "input").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    assert_eq!(outputs[1]["bytes_written"], 0, "{}", outputs[1]);
    assert_eq!(outputs[1]["cancelled"], false, "{}", outputs[1]);
    assert!(outputs[1].get("error").is_none(), "{}", outputs[1]);
    assert_eq!(outputs[2]["bytes_written"], data.len(), "{}", outputs[2]);
    assert_eq!(outputs[2]["cancelled"], false, "{}", outputs[2]);
    let wait = &outputs[3];
    assert_eq!(wait["joined"], true, "{wait}");
    assert_eq!(wait["timed_out"], false, "{wait}");
    assert_eq!(wait["exit"]["exit_code"], 0, "{wait}");
    let raw = outputs[4]["text"].as_str().unwrap();
    assert!(raw.contains("INTERACTIVE_OK"), "{raw:?}");
    assert_eq!(outputs[5]["acknowledged"], true, "{}", outputs[5]);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

async fn wait_for_attachment_text(lease: &TerminalAttachment, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = lease.page(0, 64).expect("attachment page");
        if page.text.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "attachment screen did not contain {needle:?}: {}",
            page.text
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn human_attachment_owns_terminal_input_until_detach() {
    let root = std::env::temp_dir().join(format!("ion-pty-attach-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":"cat"}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let start = process_outputs(&session)[0].clone();
    let id = started_id(&start);
    let lease = tokio::time::timeout(
        Duration::from_secs(5),
        owner.attach_terminal(session.id(), &root, id),
    )
    .await
    .expect("attachment acquire did not answer")
    .expect("attachment lease");
    assert!(lease.is_attached(), "{}", start);
    let summaries = owner.list_terminals(session.id(), &root).await;
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].attached);

    let observer = agent(
        model(turn(
            vec![
                call("write", json!({"action":"write","id":id,"data":"model\n"})),
                call(
                    "resize",
                    json!({"action":"resize","id":id,"rows":30,"columns":100}),
                ),
                call("screen", json!({"action":"screen","id":id,"rows":5})),
                call("inspect", json!({"action":"inspect","id":id})),
            ],
            "observed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&observer, &session, "observe").await.unwrap();
    let outputs = process_outputs(&session);

    let ack = lease.write(b"human\r".to_vec()).await;
    assert_eq!(ack.bytes_written, 6, "{ack:?}");
    assert!(ack.error.is_none(), "{ack:?}");
    wait_for_attachment_text(&lease, "human").await;
    lease.resize(30, 100).await.unwrap();
    let page = lease.page(0, 64).expect("resized attachment page");
    assert_eq!(page.screen.rows, 30, "{page:?}");
    assert_eq!(page.screen.columns, 100, "{page:?}");
    lease.release().await;
    assert!(!owner.list_terminals(session.id(), &root).await[0].attached);

    let follower = agent(
        model(turn(
            vec![call(
                "write",
                json!({"action":"write","id":id,"data":"after\r"}),
            )],
            "followed",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&follower, &session, "follow").await.unwrap();
    // A write acknowledgement covers delivery to the master, not projection
    // latency; reattach and observe the echoed bytes before closing.
    let after = owner
        .attach_terminal(session.id(), &root, id)
        .await
        .expect("reattach after detach");
    wait_for_attachment_text(&after, "after").await;
    after.release().await;
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
    let followed = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();

    let expected = "attached human";
    let write = &outputs[1];
    assert!(
        write["error"].as_str().unwrap().contains(expected),
        "{write}"
    );
    let resize = &outputs[2];
    assert!(
        resize["error"].as_str().unwrap().contains(expected),
        "{resize}"
    );
    assert_eq!(outputs[3]["visible_rows"], 5, "{}", outputs[3]);
    assert_eq!(outputs[4]["human_attached"], true, "{}", outputs[4]);

    let write = &followed[5];
    assert!(write.get("error").is_none(), "{write}");
    assert_eq!(write["bytes_written"], 6, "{write}");
    assert_eq!(followed[6]["cancelled"], false, "{}", followed[6]);
    assert_eq!(followed[7]["acknowledged"], true, "{}", followed[7]);
}

#[tokio::test]
async fn attachment_scope_rejects_foreign_pipe_and_double_attach() {
    let root = std::env::temp_dir().join(format!("ion-pty-attach-scope-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let other = root.join("other");
    std::fs::create_dir(&other).unwrap();
    let owner = Arc::new(ProcessOwner::new(2).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let runner = agent(
        model(turn(
            vec![
                call(
                    "pty",
                    json!({"action":"start","backend":"pty","command":"exec sleep 30"}),
                ),
                call("pipe", json!({"action":"start","command":"exec sleep 30"})),
            ],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let outputs = process_outputs(&session);
    let pty = started_id(&outputs[0]);
    let pipe = started_id(&outputs[1]);
    let lease = owner
        .attach_terminal(session.id(), &root, pty)
        .await
        .expect("attach live PTY");
    let double = owner.attach_terminal(session.id(), &root, pty).await.err();
    let foreign = owner
        .attach_terminal(Uuid::now_v7(), &root, pty)
        .await
        .err();
    let cwd = owner.attach_terminal(session.id(), &other, pty).await.err();
    let pipe = owner.attach_terminal(session.id(), &root, pipe).await.err();
    lease.release().await;
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();

    assert!(
        double
            .as_ref()
            .is_some_and(|error| error.contains("already has an attached human")),
        "{double:?}"
    );
    assert!(
        foreign
            .as_ref()
            .is_some_and(|error| error.contains("unavailable")),
        "{foreign:?}"
    );
    assert!(
        cwd.as_ref()
            .is_some_and(|error| error.contains("unavailable")),
        "{cwd:?}"
    );
    assert!(
        pipe.as_ref()
            .is_some_and(|error| error.contains("requires a PTY")),
        "{pipe:?}"
    );
}

#[tokio::test]
async fn attachment_view_ends_when_the_program_exits() {
    let root = std::env::temp_dir().join(format!("ion-pty-attach-exit-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":"printf done; exec sleep 0.2"}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    let lease = owner
        .attach_terminal(session.id(), &root, id)
        .await
        .expect("attach live PTY");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lease.is_live() {
        assert!(Instant::now() < deadline, "program did not exit");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let write = lease.write(b"late".to_vec()).await;
    let page = lease.page(0, 24).expect("screen survives exit until ack");
    lease.release().await;
    let joiner = agent(
        model(turn(
            vec![
                call("wait", json!({"action":"wait","id":id,"timeout_ms":5000})),
                call("ack", json!({"action":"acknowledge","id":id})),
            ],
            "joined",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&joiner, &session, "join").await.unwrap();
    let outputs = process_outputs(&session);
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();

    assert!(write.error.is_some(), "{write:?}");
    assert_eq!(write.bytes_written, 0, "{write:?}");
    assert!(page.text.contains("done"), "{page:?}");
    assert_eq!(outputs[1]["timed_out"], false, "{}", outputs[1]);
    assert_eq!(outputs[1]["exit"]["exit_code"], 0, "{}", outputs[1]);
    assert_eq!(outputs[2]["acknowledged"], true, "{}", outputs[2]);
}

#[tokio::test]
async fn stale_attachment_cannot_release_a_newer_lease() {
    let root = std::env::temp_dir().join(format!("ion-pty-attach-stale-{}", Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let owner = Arc::new(ProcessOwner::new(1).unwrap());
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let runner = agent(
        model(turn(
            vec![call(
                "start",
                json!({"action":"start","backend":"pty","command":"exec sleep 30"}),
            )],
            "started",
        )),
        Arc::new(ProcessTools::new(&root, owner.clone()).unwrap()),
    );
    submit(&runner, &session, "start").await.unwrap();
    let id = started_id(&process_outputs(&session)[0]);
    let lease = owner
        .attach_terminal(session.id(), &root, id)
        .await
        .expect("attach live PTY");
    let stale = lease.stale_view();
    let stale_write = stale.write(b"stale".to_vec()).await;
    stale.release().await;
    let still_attached =
        lease.is_attached() && owner.list_terminals(session.id(), &root).await[0].attached;
    let write = lease.write(b"live".to_vec()).await;
    lease.release().await;
    owner.join_shutdown().await.unwrap();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();

    assert!(stale_write.error.is_some(), "{stale_write:?}");
    assert_eq!(stale_write.bytes_written, 0, "{stale_write:?}");
    assert!(
        still_attached,
        "a stale token released the live attachment lease"
    );
    assert!(write.error.is_none(), "{write:?}");
    assert_eq!(write.bytes_written, 4, "{write:?}");
}

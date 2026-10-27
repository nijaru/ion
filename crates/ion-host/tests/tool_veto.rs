use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    ChildOutcome, CodeLimits, CodingAgent, CodingSession, CodingToolOutput, CodingToolSource,
    SessionEntry, ToolDefinition, ToolExecutor, ToolInvocation, ToolOutcome, ToolRegistration,
    ToolSet,
};
use ion_host::{CommandEnvironment, LocalTools, ToolVeto, ToolVetoSettings, code_mode::QuickJs};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ion-veto-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn policy(&self, body: &str, timeout_ms: u64) -> Arc<ToolVeto> {
        let path = self.0.join("policy.py");
        fs::write(&path, body).unwrap();
        Arc::new(
            ToolVeto::new(
                &self.0,
                Arc::new(ToolVetoSettings {
                    program: "python3".into(),
                    args: vec![path.to_str().unwrap().into(), "literal ' \" $ ;".into()],
                    timeout_ms,
                }),
                Arc::new(CommandEnvironment::inherited()),
            )
            .unwrap(),
        )
    }
    fn session(&self) -> Arc<CodingSession> {
        Arc::new(CodingSession::create(self.0.join("session.sqlite"), Some(&self.0)).unwrap())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn call(id: &str, name: &str, arguments: Value) -> Content {
    Content::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
        raw_arguments: None,
    })
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
fn agent(
    root: &Workspace,
    policy: Arc<ToolVeto>,
    calls: Vec<Content>,
) -> (CodingAgent, Arc<ScriptedModelService>) {
    let model = Arc::new(ScriptedModelService::new([
        response(calls),
        response(vec![Content::Text("finished".into())]),
    ]));
    let sources: Vec<Arc<dyn CodingToolSource>> = vec![
        Arc::new(LocalTools::new(&root.0).unwrap()),
        Arc::new(Deferred),
    ];
    let tools = ToolSet::new(sources)
        .with_history()
        .with_code_mode(Arc::new(QuickJs), CodeLimits::default())
        .with_policy(policy);
    (
        CodingAgent::with_tool_set(
            model.clone(),
            Arc::new(tools),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        ),
        model,
    )
}
struct Deferred;
impl CodingToolSource for Deferred {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "deferred".into(),
                description: "Discoverable deferred capability".into(),
                input_schema: json!({"type":"object"}),
            })
            .deferred(),
            self,
        )]
    }
}
impl ToolExecutor for Deferred {
    fn execute<'a>(&'a self, _: ToolInvocation<'a>) -> ion_ai::BoxFuture<'a, CodingToolOutput> {
        panic!("ineligible or stopped executor constructor must never run");
    }
}
async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn one_policy_gates_direct_intrinsics_and_composed_children_with_typed_facts() {
    let root = Workspace::new();
    fs::write(root.0.join("input"), "original").unwrap();
    let policy = root.policy(
        r#"import json, sys
assert sys.argv[1] == "literal ' \" $ ;"
event = json.load(sys.stdin)
sys.stderr.write('PRIVATE_DIAGNOSTIC' + 'x' * 131072)
with open('policy.jsonl', 'a') as log:
    log.write(json.dumps(event) + '\n')
if event['tool_name'] in ['write', 'history', 'tool_search']:
    print(json.dumps({'decision':'deny', 'reason':'blocked by test policy'}))
else:
    print(json.dumps({'decision':'allow'}))
"#,
        5000,
    );
    let content = "literal ' $(touch INJECTED)\nUnicode 🦀";
    let (agent, model) = agent(
        &root,
        policy,
        vec![
            call("read", "read", json!({"path":"input"})),
            call(
                "write",
                "write",
                json!({"path":"direct", "content":content}),
            ),
            call("history", "history", json!({})),
            call("search", "tool_search", json!({"query":"deferred"})),
            call("undeclared", "deferred", json!({})),
            call(
                "code",
                "code_mode",
                json!({"code":r#"
            const outcomes = await Promise.allSettled([
                tools.read({path:'input'}),
                tools.write({path:'composed',content:'MUST_NOT_WRITE'}),
                tools.history({})
            ]);
            return outcomes.map(value => value.status);
        "#}),
            ),
        ],
    );
    let session = root.session();
    agent
        .submit(
            session.clone(),
            "exercise veto".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let view = session.view().unwrap();
    let direct: Vec<_> = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(direct.len(), 6);
    assert!(matches!(direct[0].outcome, ToolOutcome::Observed { .. }));
    assert!(
        direct[1..5]
            .iter()
            .all(|result| matches!(result.outcome, ToolOutcome::NotDispatched { .. }))
    );
    let ToolOutcome::Observed { output, .. } = &direct[5].outcome else {
        panic!("parent composition was rejected")
    };
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(
        output.value["result"],
        json!(["fulfilled", "rejected", "rejected"])
    );
    let children: Vec<_> = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ChildToolResult { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(children.len(), 3);
    assert_eq!(
        children
            .iter()
            .filter(|outcome| matches!(outcome, ChildOutcome::NotDispatched { .. }))
            .count(),
        2
    );
    assert!(
        !root.0.join("direct").exists()
            && !root.0.join("composed").exists()
            && !root.0.join("INJECTED").exists()
    );
    assert_eq!(
        fs::read_to_string(root.0.join("input")).unwrap(),
        "original"
    );
    let events: Vec<Value> = fs::read_to_string(root.0.join("policy.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events.len(),
        8,
        "unknown/undeclared calls must not invoke policy"
    );
    assert_eq!(events[1]["arguments"]["content"], content);
    assert!(events.iter().all(
        |event| event["origin"]["session"] == session.id().to_string()
            && event["cwd"] == root.0.to_str().unwrap()
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["origin"]["composed_child"].is_number())
            .count(),
        3
    );
    assert!(
        !model.requests()[1]
            .tools
            .iter()
            .any(|tool| tool.name == "deferred"),
        "vetoed search activated a deferred tool"
    );
    assert!(!format!("{:?}", model.requests()).contains("PRIVATE_DIAGNOSTIC"));
    let Content::ToolCall(call) = call("untracked", "read", json!({"path":"input"})) else {
        unreachable!()
    };
    let result = agent
        .tool_catalog()
        .execute_untracked(&call, CancellationToken::new())
        .await;
    assert!(
        result.is_error
            && result
                .value
                .to_string()
                .contains("requires an admitted invocation")
    );
    assert_eq!(
        fs::read_to_string(root.0.join("policy.jsonl"))
            .unwrap()
            .lines()
            .count(),
        8
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_after_policy_allow_never_constructs_the_selected_executor() {
    let root = Workspace::new();
    let policy = root.policy(
        "import json, sys\njson.load(sys.stdin)\nprint('{\"decision\":\"allow\"}')\n",
        5000,
    );
    let model = Arc::new(ScriptedModelService::new([
        response(vec![call(
            "discover",
            "tool_search",
            json!({"query":"deferred"}),
        )]),
        response(vec![call("stopped", "deferred", json!({}))]),
    ]));
    let agent = CodingAgent::with_tool_set(
        model,
        Arc::new(
            ToolSet::new([Arc::new(Deferred) as Arc<dyn CodingToolSource>]).with_policy(policy),
        ),
        ModelRef {
            provider: "test".into(),
            model: "test".into(),
        },
    );
    let session = root.session();
    let stop = CancellationToken::new();
    let trigger = stop.clone();
    let result = agent
        .submit(
            session.clone(),
            "stop after approval".into(),
            String::new(),
            stop,
            move |event| {
                if let ion_core::CodingAgentEvent::ToolStarted { name, .. } = event
                    && name == "deferred"
                {
                    trigger.cancel();
                }
            },
        )
        .await;
    assert!(
        matches!(result, Err(ion_core::CodingAgentError::Cancelled)),
        "{result:?}"
    );
    let view = session.view().unwrap();
    assert!(view.entries.iter().any(|entry| matches!(entry,
        SessionEntry::ToolResult { result, .. } if result.call_id == "stopped"
        && matches!(result.outcome, ToolOutcome::NotDispatched { .. }))));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn protocol_failures_and_parent_composition_denial_never_start_selected_work() {
    for (body, expected) in [
        (
            "import sys; print('{\"decision\":\"allow\"}'); sys.exit(7)",
            "program exited unsuccessfully",
        ),
        (
            "print('PRIVATE_INVALID_REPLY')",
            "invalid tool veto decision JSON",
        ),
        (
            "print('{\"decision\":\"allow\",\"replace_arguments\":\"PRIVATE_VALUE\"}')",
            "invalid tool veto decision JSON",
        ),
        ("print('x' * 65537)", "reply exceeds 65536 bytes"),
        (
            "print('{\"decision\":\"deny\",\"reason\":\"no VM\"}')",
            "no VM",
        ),
    ] {
        let root = Workspace::new();
        let policy = root.policy(body, 5000);
        let (agent, model) = agent(
            &root,
            policy,
            vec![call(
                "parent",
                "code_mode",
                json!({"code":"throw new Error('VM_RAN')"}),
            )],
        );
        let session = root.session();
        agent
            .submit(
                session.clone(),
                "reject before VM".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let view = session.view().unwrap();
        assert!(
            !view
                .entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::ChildToolAdmitted { .. }))
        );
        let result = view
            .entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::ToolResult { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        let ToolOutcome::NotDispatched { reason } = &result.outcome else {
            panic!("expected {expected}, got {:?}", result.outcome)
        };
        assert!(
            reason.contains(expected) && !reason.contains("PRIVATE_") && !reason.contains("VM_RAN"),
            "expected {expected}, got {reason}"
        );
        assert!(
            !serde_json::to_string(&model.requests()[1])
                .unwrap()
                .contains("PRIVATE_")
        );
        session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn protocol_failure_stops_same_group_descendant_after_policy_exit() {
    let root = Workspace::new();
    let policy = root.policy(
        r#"import json, os, signal, sys, time
json.load(sys.stdin)
if os.fork() == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    null = os.open(os.devnull, os.O_RDWR)
    for fd in (0, 1, 2): os.dup2(null, fd)
    with open('detached-pipes', 'w') as marker: marker.write(str(os.getpid()))
    while True: time.sleep(1)
while not os.path.exists('detached-pipes'): time.sleep(0.001)
print('{"decision":"allow"}')
sys.exit(7)
"#,
        5000,
    );
    let (agent, _) = agent(
        &root,
        policy,
        vec![call(
            "write",
            "write",
            json!({"path":"effect", "content":"NO"}),
        )],
    );
    let session = root.session();
    agent
        .submit(
            session.clone(),
            "failed policy".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(!root.0.join("effect").exists());
    let pid = fs::read_to_string(root.0.join("detached-pipes")).unwrap();
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=,command=", "-p", &pid])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    let live = !status.is_empty() && !status.trim_start().starts_with('Z');
    if live {
        // Reproduce the original leak without leaving its owned process behind.
        assert!(
            status.contains(root.0.to_str().unwrap()),
            "PID is not our policy: {status}"
        );
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid.parse().unwrap()).unwrap(),
            rustix::process::Signal::KILL,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let output = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid])
                    .output()
                    .unwrap();
                if output.stdout.is_empty()
                    || String::from_utf8_lossy(&output.stdout)
                        .trim_start()
                        .starts_with('Z')
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    session.shutdown().await.unwrap();
    assert!(
        !live,
        "failed policy left a same-group descendant running after reaping its direct child"
    );
}

#[tokio::test]
async fn timeout_and_stop_join_the_policy_group_before_refusing_dispatch() {
    for cancel in [false, true] {
        let root = Workspace::new();
        let policy = root.policy(
            r#"import json, os, signal, sys, time
json.load(sys.stdin)
child = os.fork()
if child == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    with open('descendant', 'w') as output: output.write(str(os.getpid()))
    while True: time.sleep(1)
with open('entered', 'w') as output: output.write(str(os.getpid()))
while True: time.sleep(1)
"#,
            if cancel { 5000 } else { 2000 },
        );
        let (agent, _) = agent(
            &root,
            policy,
            vec![call(
                "write",
                "write",
                json!({"path":"effect", "content":"NO"}),
            )],
        );
        let session = root.session();
        let stop = CancellationToken::new();
        let operation = agent.submit(
            session.clone(),
            "held policy".into(),
            String::new(),
            stop.clone(),
            |_| {},
        );
        tokio::pin!(operation);
        tokio::select! {
            () = async { wait_file(&root.0.join("entered")).await; wait_file(&root.0.join("descendant")).await; } => {},
            result = &mut operation => panic!("policy returned before its hold: {result:?}"),
        }
        if cancel {
            stop.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), &mut operation)
            .await
            .expect("policy group was not joined");
        if cancel {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        assert!(!root.0.join("effect").exists());
        let view = session.view().unwrap();
        assert!(view.entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult {result,..} if matches!(result.outcome, ToolOutcome::NotDispatched {..}))));
        // A dead orphan can remain a zombie until the OS reaps it. Neither that
        // nor a live process is inferred from kill(pid, 0) alone.
        for marker in ["entered", "descendant"] {
            let pid = fs::read_to_string(root.0.join(marker)).unwrap();
            let status = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid])
                .output()
                .unwrap();
            assert!(
                status.stdout.is_empty()
                    || String::from_utf8_lossy(&status.stdout)
                        .trim_start()
                        .starts_with('Z'),
                "live policy {marker}: {}",
                String::from_utf8_lossy(&status.stdout)
            );
        }
        session.shutdown().await.unwrap();
    }
}

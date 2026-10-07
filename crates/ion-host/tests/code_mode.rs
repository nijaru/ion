use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, Usage,
};
use ion_core::{
    ChildOutcome, CodeLimits, CodingAgent, CodingAgentError, CodingSession, CodingToolSource,
    ForkPoint, LiveTranscript, SessionEntry, ToolSet, TranscriptItem, TranscriptProjection,
};
use ion_host::{LocalTools, code_mode::QuickJs};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ion-code-gate-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn session(&self) -> CodingSession {
        CodingSession::create(self.0.join("session.sqlite"), &self.0).unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn identity() -> ModelRef {
    ModelRef {
        provider: "test".into(),
        model: "test".into(),
    }
}
fn code(id: &str, body: &str) -> Content {
    Content::ToolCall(ToolCall {
        id: id.into(),
        name: "code_mode".into(),
        arguments: json!({"code":body}),
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
fn make_agent(
    root: &Path,
    scripts: impl IntoIterator<Item = Script>,
    limits: CodeLimits,
) -> (CodingAgent, Arc<ScriptedModelService>) {
    let model = Arc::new(ScriptedModelService::new(scripts));
    let source: Arc<dyn CodingToolSource> = Arc::new(LocalTools::new(root).unwrap());
    let tools = ToolSet::new([source]).with_code_mode(Arc::new(QuickJs), limits);
    (
        CodingAgent::with_tool_set(model.clone(), Arc::new(tools), identity()),
        model,
    )
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
async fn native_fanout_selects_context_and_preserves_occurrences_inspection_and_forks() {
    let root = Workspace::new();
    let session = root.session();
    fs::write(root.0.join("one"), "PRIVATE_INTERMEDIATE_ONE").unwrap();
    fs::write(root.0.join("two"), "PRIVATE_INTERMEDIATE_TWO").unwrap();
    let first = "const defs = await tools.describe('read');
        const values = await Promise.all(['one','two'].map(path => tools.call(defs[0].name,{path})));
        await tools.call('write',{path:'changed',content:'changed'});
        const missing = await tools.call('read',{path:'missing'});
        return {lengths:values.map(r => r.value.content.length), missing:missing.is_error};";
    let (agent, model) = make_agent(
        &root.0,
        [
            response(vec![
                code("reused", first),
                code(
                    "other",
                    "void tools.call('write',{path:'unawaited',content:'done'}); return true;",
                ),
            ]),
            response(vec![code(
                "reused",
                "const r=await tools.call('read',{path:'one'}); return r.value.content.length;",
            )]),
            response(vec![Content::Text("DONE".into())]),
        ],
        CodeLimits::default(),
    );
    let mut live = LiveTranscript::with_user_input(&Message::user_input("run".into(), []));
    let mut saw_running_child = false;
    let result = agent
        .submit(
            &session,
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |event| {
                let started = matches!(&event, ion_core::CodingAgentEvent::ChildToolStarted { .. });
                live.observe(event);
                if started {
                    saw_running_child = true;
                    assert!(live.projection().items.iter().any(|item| matches!(item, TranscriptItem::ActivityGroup(group) if group.activities.iter().any(|parent| parent.state == ion_core::ActivityState::Running && parent.children.iter().any(|child| child.state == ion_core::ActivityState::Running)))));
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(result, "DONE");
    assert!(saw_running_child);
    assert_eq!(
        fs::read_to_string(root.0.join("changed")).unwrap(),
        "changed"
    );
    assert_eq!(
        fs::read_to_string(root.0.join("unawaited")).unwrap(),
        "done"
    );
    let view = session.view().unwrap();
    let intents = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ChildToolAdmitted { intent, .. } => Some(intent),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(intents.len(), 6);
    assert_eq!(intents[0].parent, intents[3].parent);
    assert_eq!(
        intents[4].parent.assistant_entry,
        intents[0].parent.assistant_entry
    );
    assert_eq!(intents[4].parent.ordinal, 1);
    assert_ne!(
        intents[5].parent.assistant_entry,
        intents[0].parent.assistant_entry
    );
    let saved = TranscriptProjection::from_session(&view);
    assert_eq!(&saved, live.projection());
    let children = saved
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ActivityGroup(group) => Some(&group.activities),
            _ => None,
        })
        .flatten()
        .flat_map(|activity| &activity.children)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 6);
    assert_eq!(children[3].state, ion_core::ActivityState::Failed);
    assert!(
        children
            .iter()
            .any(|child| child.result.as_ref().is_some_and(|result| result
                .value
                .to_string()
                .contains("PRIVATE_INTERMEDIATE_ONE")))
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        let wire = serde_json::to_string(request).unwrap();
        assert!(!wire.contains("PRIVATE_INTERMEDIATE"), "{wire}");
        assert!(wire.contains("lengths"));
    }
    let clone = session.clone_to(root.0.join("clone.sqlite")).unwrap();
    let fork = session
        .fork_to(root.0.join("fork.sqlite"), ForkPoint::AfterTurn(1))
        .unwrap();
    for copied in [&clone, &fork] {
        assert_eq!(
            TranscriptProjection::from_session(&copied.view().unwrap()),
            saved
        );
    }
    drop(session);
    let reopened = CodingSession::open(root.0.join("session.sqlite")).unwrap();
    assert_eq!(
        TranscriptProjection::from_session(&reopened.view().unwrap()),
        saved
    );
}

#[tokio::test]
async fn returned_guest_does_not_cancel_unawaited_host_work_at_its_deadline() {
    let root = Workspace::new();
    let session = root.session();
    let (agent, model) = make_agent(
        &root.0,
        [
            response(vec![code(
                "parent",
                "for (let i=0;i<4;i++) void tools.call('exec',{command:`sleep 0.4; printf done > settled-${i}`}); return 7;",
            )]),
            response(vec![Content::Text("DONE".into())]),
        ],
        CodeLimits {
            deadline: Duration::from_millis(200),
            max_concurrency: 2,
            max_json_bytes: 4096,
            ..CodeLimits::default()
        },
    );
    agent
        .submit(
            &session,
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    for i in 0..4 {
        assert_eq!(
            fs::read_to_string(root.0.join(format!("settled-{i}"))).unwrap(),
            "done"
        );
    }
    assert_eq!(session.view().unwrap().entries.iter().filter(|entry| matches!(entry, SessionEntry::ChildToolResult { outcome: ChildOutcome::Observed { output }, .. } if !output.is_error)).count(), 4);
    assert_eq!(model.requests().len(), 2);
}

#[tokio::test]
async fn cancellation_settles_native_children_before_return() {
    let root = Workspace::new();
    let session = Arc::new(root.session());
    let (agent, model) = make_agent(
        &root.0,
        [response(vec![code(
            "parent",
            "await Promise.all(['a','b'].map(x => tools.call('exec',{command:`printf started; printf ready > ${x}; exec sleep 30`,timeout_ms:120000}))); return true;",
        )])],
        CodeLimits::default(),
    );
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        agent
            .submit(
                &task_session,
                "run".into(),
                String::new(),
                task_stop,
                |_| {},
            )
            .await
    });
    wait_file(&root.0.join("a")).await;
    wait_file(&root.0.join("b")).await;
    stop.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, Err(CodingAgentError::Cancelled)),
        "{result:?}"
    );
    let view = session.view().unwrap();
    let outputs = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ChildToolResult {
                outcome: ChildOutcome::Observed { output },
                ..
            } => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    for output in outputs {
        assert_eq!(output.value["cancelled"], true);
        assert!(output.value["stdout"].as_str().unwrap().contains("started"));
    }
    assert_eq!(model.requests().len(), 1);
    assert!(matches!(
        view.entries.last(),
        Some(SessionEntry::TurnEnded { .. })
    ));
}

#[tokio::test]
async fn child_commit_failure_blocks_consumption_drains_started_work_and_reopens_unknown_without_replay()
 {
    let root = Workspace::new();
    let session = root.session();
    let connection = rusqlite::Connection::open(session.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_child BEFORE INSERT ON entries
        WHEN json_extract(NEW.body,'$.kind')='child_tool_result' AND json_extract(NEW.body,'$.data.child')=0 AND json_extract(NEW.body,'$.data.outcome.state')='observed'
        BEGIN SELECT RAISE(ABORT,'child observation fault'); END;").unwrap();
    let (agent, model) = make_agent(&root.0, [response(vec![code("parent",
        "await Promise.all([
            tools.call('exec',{command:'printf first > first; sleep 0.15; printf FIRST'}),
            tools.call('exec',{command:'printf second > second; printf SECOND; exec sleep 30',timeout_ms:120000})]);
         await tools.call('write',{path:'dependent',content:'must not happen'}); return true;")])], CodeLimits::default());
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        agent.submit(
            &session,
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(result, Err(CodingAgentError::Session(_))),
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(root.0.join("first")).unwrap(), "first");
    assert_eq!(fs::read_to_string(root.0.join("second")).unwrap(), "second");
    assert!(!root.0.join("dependent").exists());
    assert_eq!(model.requests().len(), 1);
    let before = session.view().unwrap();
    assert_eq!(before.entries.iter().filter(|entry| matches!(entry, SessionEntry::ChildToolResult { child: 1, outcome: ChildOutcome::Observed { output }, .. } if output.value["cancelled"] == true)).count(), 1);
    connection
        .execute_batch("DROP TRIGGER reject_child;")
        .unwrap();
    drop(connection);
    drop(session);
    let reopened = CodingSession::open(root.0.join("session.sqlite")).unwrap();
    let (next, next_model) = make_agent(
        &root.0,
        [response(vec![Content::Text("RECOVERED".into())])],
        CodeLimits::default(),
    );
    next.submit(
        &reopened,
        "continue without replay".into(),
        String::new(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    let view = reopened.view().unwrap();
    assert!(view.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ChildToolResult {
            child: 0,
            outcome: ChildOutcome::Unknown,
            ..
        }
    )));
    assert_eq!(
        view.entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::ChildToolAdmitted { .. }))
            .count(),
        2
    );
    assert_eq!(next_model.requests().len(), 1);
    assert!(!root.0.join("dependent").exists());
}

#[tokio::test]
async fn child_intent_fault_prevents_native_dispatch() {
    let root = Workspace::new();
    let session = root.session();
    let connection = rusqlite::Connection::open(session.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_intent BEFORE INSERT ON entries
        WHEN json_extract(NEW.body,'$.kind')='child_tool_admitted'
        BEGIN SELECT RAISE(ABORT,'intent fault'); END;",
        )
        .unwrap();
    let (agent, model) = make_agent(
        &root.0,
        [response(vec![code(
            "parent",
            "await tools.call('write',{path:'effect',content:'must not happen'}); return true;",
        )])],
        CodeLimits::default(),
    );
    let result = agent
        .submit(
            &session,
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await;
    assert!(
        matches!(result, Err(CodingAgentError::Session(_))),
        "{result:?}"
    );
    assert!(!root.0.join("effect").exists());
    assert_eq!(model.requests().len(), 1);
    assert!(
        !session
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry, SessionEntry::ChildToolAdmitted { .. }))
    );
    connection
        .execute_batch("DROP TRIGGER reject_intent;")
        .unwrap();
    drop(connection);
    drop(session);
    let reopened = CodingSession::open(root.0.join("session.sqlite")).unwrap();
    let (next, _) = make_agent(
        &root.0,
        [response(vec![Content::Text("RECOVERED".into())])],
        CodeLimits::default(),
    );
    next.submit(
        &reopened,
        "continue".into(),
        String::new(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert!(!root.0.join("effect").exists());
}

#[tokio::test]
async fn guest_failure_retains_prior_mutation_and_deadline_settles_native_capture() {
    for timeout in [false, true] {
        let root = Workspace::new();
        let session = root.session();
        let script = if timeout {
            "await tools.call('exec',{command:'printf kept > effect; printf BEFORE_DEADLINE; exec sleep 30',timeout_ms:120000}); return true;"
        } else {
            "await tools.call('write',{path:'effect',content:'kept'}); throw new Error('after mutation');"
        };
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code("parent", script)]),
                response(vec![Content::Text("FAULT_REPORTED".into())]),
            ],
            CodeLimits {
                deadline: Duration::from_millis(500),
                ..CodeLimits::default()
            },
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            agent.submit(
                &session,
                "run".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(fs::read_to_string(root.0.join("effect")).unwrap(), "kept");
        let view = session.view().unwrap();
        assert!(view.entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.name == "code_mode" && result.is_error)));
        assert!(view.entries.iter().any(|entry| matches!(entry, SessionEntry::ChildToolResult { outcome: ChildOutcome::Observed { output }, .. } if if timeout { output.value["cancelled"] == true && output.value["stdout"].as_str().unwrap().contains("BEFORE_DEADLINE") } else { !output.is_error })));
        assert_eq!(model.requests().len(), 2);
    }
}

#[tokio::test]
async fn child_images_stay_inspectable_and_reply_limit_is_cumulative() {
    let root = Workspace::new();
    let session = root.session();
    let image = image::RgbImage::new(1, 1);
    image.save(root.0.join("pixel.png")).unwrap();
    fs::write(root.0.join("report"), "PRIVATE_".repeat(100)).unwrap();
    let (agent, model) = make_agent(
        &root.0,
        [
            response(vec![code(
                "parent",
                "const i=await tools.call('read',{path:'pixel.png'}); if(i.image_mime_types[0]!=='image/png') throw 'missing image'; await tools.call('read',{path:'report'}); await tools.call('read',{path:'report'}); await tools.call('write',{path:'dependent',content:'bad'}); return true;",
            )]),
            response(vec![Content::Text("LIMIT_REPORTED".into())]),
        ],
        CodeLimits {
            max_json_bytes: 2048,
            max_host_bytes: 1700,
            ..CodeLimits::default()
        },
    );
    agent
        .submit(
            &session,
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(!root.0.join("dependent").exists());
    let view = session.view().unwrap();
    let outputs = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ChildToolResult {
                outcome: ChildOutcome::Observed { output },
                ..
            } => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].images.len(), 1);
    assert!(
        outputs[1..]
            .iter()
            .all(|output| !output.is_error && output.value.to_string().contains("PRIVATE_"))
    );
    let requests = model.requests();
    let request = &requests[1];
    assert!(!serde_json::to_string(request).unwrap().contains("PRIVATE_"));
    assert!(request.messages.iter().all(|message| {
        message
            .content
            .iter()
            .all(|part| !matches!(part, Content::Image(_)))
    }));
}

#[tokio::test]
async fn reply_and_audit_budgets_preserve_observations_and_stop_dependents() {
    for audit in [false, true] {
        let root = Workspace::new();
        let session = root.session();
        fs::write(root.0.join("report"), "PRIVATE_REPORT_".repeat(200)).unwrap();
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code(
                    "parent",
                    "const r=await tools.call('read',{path:'report'}); await tools.call('write',{path:'dependent',content:'bad'}); return r.value;",
                )]),
                response(vec![Content::Text("LIMIT_REPORTED".into())]),
            ],
            CodeLimits {
                max_json_bytes: 1024,
                audit_admission_bytes: if audit { 1 } else { 32 * 1024 * 1024 },
                ..CodeLimits::default()
            },
        );
        agent
            .submit(
                &session,
                "run".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let view = session.view().unwrap();
        if audit {
            assert!(view.entries.iter().any(|entry| matches!(
                entry,
                SessionEntry::ChildToolResult {
                    outcome: ChildOutcome::NotDispatched { .. },
                    ..
                }
            )));
        } else {
            assert!(view.entries.iter().any(|entry| matches!(entry, SessionEntry::ChildToolResult { outcome: ChildOutcome::Observed { output }, .. } if !output.is_error && output.value.to_string().contains("PRIVATE_REPORT_"))));
        }
        assert!(!root.0.join("dependent").exists());
        assert!(
            !serde_json::to_string(&model.requests()[1])
                .unwrap()
                .contains("PRIVATE_REPORT_")
        );
    }
}

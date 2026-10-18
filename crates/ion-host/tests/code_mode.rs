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
    fn session(&self) -> Arc<CodingSession> {
        Arc::new(CodingSession::create(self.0.join("session.sqlite"), Some(&self.0)).unwrap())
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

#[test]
fn guest_and_native_effects_progress_with_one_host_blocking_worker() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = Workspace::new();
        fs::write(root.0.join("input.txt"), "original\n").unwrap();
        let session = root.session();
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code(
                    "capacity",
                    r#"
                    const before = await tools.read({path:'input.txt'});
                    if (before.is_error) throw new Error(JSON.stringify(before.value));
                    const content = before.value.content + 'completed\n';
                    const written = await tools.write({path:'result.txt',content});
                    if (written.is_error) throw new Error(JSON.stringify(written.value));
                    const after = await tools.read({path:'result.txt'});
                    if (after.is_error) throw new Error(JSON.stringify(after.value));
                    return after.value.content;
                "#,
                )]),
                response(vec![Content::Text("complete".into())]),
            ],
            CodeLimits::default(),
        );
        let stop = CancellationToken::new();
        let submission = agent.submit(
            session.clone(),
            "compose".into(),
            String::new(),
            stop.clone(),
            |_| {},
        );
        tokio::pin!(submission);
        let early = tokio::time::timeout(Duration::from_secs(5), &mut submission).await;
        let progressed = early.is_ok();
        let outcome = match early {
            Ok(outcome) => outcome,
            Err(_) => {
                // The old capacity cycle is rescued only after the probe. Keep
                // the real operation alive until its VM and native work join.
                stop.cancel();
                tokio::time::timeout(Duration::from_secs(5), &mut submission)
                    .await
                    .expect("cancelled capacity probe failed to join")
            }
        };
        session.ensure_idle().unwrap();
        assert!(
            progressed,
            "VM occupied the capacity needed by its native child"
        );
        outcome.unwrap();
        assert!(!stop.is_cancelled());
        let view = session.view().unwrap();
        let parent = view
            .entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::ToolResult { result, .. } if result.name == "code_mode" => {
                    match &result.outcome {
                        ion_core::ToolOutcome::Observed { output, .. } => Some(output),
                        _ => None,
                    }
                }
                _ => None,
            })
            .unwrap();
        assert!(!parent.is_error, "{}", parent.value);
        assert_eq!(parent.value["result"], "original\ncompleted\n");
        assert_eq!(
            fs::read_to_string(root.0.join("input.txt")).unwrap(),
            "original\n"
        );
        assert_eq!(
            fs::read_to_string(root.0.join("result.txt")).unwrap(),
            "original\ncompleted\n"
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
        assert!(children.iter().all(
            |outcome| matches!(outcome, ChildOutcome::Observed { output } if !output.is_error)
        ));
        assert_eq!(model.requests().len(), 2);
    });
}

#[derive(Default)]
struct DiscoverySource {
    pending: std::sync::atomic::AtomicBool,
    published: std::sync::atomic::AtomicBool,
}
impl CodingToolSource for DiscoverySource {
    fn registrations(self: Arc<Self>) -> Vec<ion_core::ToolRegistration> {
        let refreshed = self.published.load(std::sync::atomic::Ordering::SeqCst);
        (0..13)
            .map(|index| {
                ion_core::ToolRegistration::new(
                    ion_core::ToolDefinition::external(ion_ai::ToolSpec {
                        name: if index == 12 { "CANDIDATE_11".into() } else { format!("candidate_{index:02}") },
                        description: if refreshed { "changed" } else { "Similar discovery candidate_11" }.into(),
                        input_schema: json!({"type":"object","properties":{"index":{"const":index}}}),
                    })
                    .deferred(),
                    self.clone(),
                )
            })
            .collect()
    }
    fn refresh<'a>(&'a self, _: CancellationToken) -> ion_ai::BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            if self.pending.load(std::sync::atomic::Ordering::SeqCst) {
                self.published
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Vec::new()
        })
    }
}
impl ion_core::ToolExecutor for DiscoverySource {
    fn execute<'a>(
        &'a self,
        _invocation: ion_core::ToolInvocation<'a>,
    ) -> ion_ai::BoxFuture<'a, ion_core::CodingToolOutput> {
        Box::pin(async move {
            // Publish a changed source while the issued guest retains its catalogue.
            self.pending
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.refresh(CancellationToken::new()).await;
            ion_core::CodingToolOutput {
                value: json!("executed"),
                images: Vec::new(),
                is_error: false,
            }
        })
    }
}

#[tokio::test]
async fn discovery_is_exact_read_only_deferred_and_request_frozen() {
    let root = Workspace::new();
    let session = root.session();
    let model = Arc::new(ScriptedModelService::new([
        response(vec![code(
            "discover",
            r#"
            const page = await searchTools('SIMILAR');
            const exact = await describeTool('candidate_11');
            const bounded = await searchTools('SIMILAR', {limit: 2});
            const emptyOptions = await searchTools('SIMILAR', {});
            const priority = await searchTools('CaNdIdAtE_11', {limit: 3});
            const one = await searchTools('CaNdIdAtE_11', {limit: 1});
            const missing = await Promise.all(['candidate_11x','Candidate_11','Similar discovery candidate_11','tool_search','code_mode'].map(describeTool));
            const exclusions = await Promise.all(['tool_search','code_mode'].map(query => searchTools(query)));
            await tools[exact.name]({});
            const frozen = await describeTool(exact.name);
            return {page, bounded, emptyOptions, priority, one, exact, missing, exclusions, frozen};
        "#,
        )]),
        response(vec![code(
            "next",
            "return await describeTool('candidate_11');",
        )]),
        response(vec![Content::Text("DONE".into())]),
    ]));
    let source = Arc::new(DiscoverySource::default());
    let tools = ToolSet::new([source as Arc<dyn CodingToolSource>])
        .with_code_mode(Arc::new(QuickJs), CodeLimits::default());
    let agent = CodingAgent::with_tool_set(model.clone(), Arc::new(tools), identity());
    agent
        .submit(
            session.clone(),
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let view = session.view().unwrap();
    let outputs = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } => match &result.outcome {
                ion_core::ToolOutcome::Observed { output, .. } => Some(output),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let result = &outputs[0].value["result"];
    assert!(!outputs[0].is_error, "{outputs:?}");
    assert_eq!(result["page"].as_array().unwrap().len(), 10);
    assert!(
        result["page"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["name"] != "candidate_11" && item.as_object().unwrap().len() == 2)
    );
    assert_eq!(
        result["exact"],
        json!({"name":"candidate_11","description":"Similar discovery candidate_11","input_schema":{"type":"object","properties":{"index":{"const":11}}}})
    );
    assert_eq!(
        result["bounded"],
        json!([result["page"][0], result["page"][1]])
    );
    assert_eq!(result["emptyOptions"], result["page"]);
    assert_eq!(
        result["priority"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["candidate_11", "CANDIDATE_11", "candidate_00"]
    );
    assert_eq!(result["one"], json!([result["priority"][0]]));
    assert_eq!(result["frozen"], result["exact"]);
    assert_eq!(result["missing"], json!([null, null, null, null, null]));
    assert_eq!(result["exclusions"], json!([[], []]));
    assert_eq!(outputs[0].value["calls"], 1);
    assert_eq!(outputs[1].value["calls"], 0);
    assert_eq!(outputs[1].value["result"]["description"], "changed");
    let intents = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ChildToolAdmitted { intent, .. } => Some(intent),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(intents.len(), 1);
    assert_eq!(
        serde_json::to_value(&intents[0].definition).unwrap(),
        result["exact"]
    );
    for request in model.requests() {
        assert!(
            !request
                .tools
                .iter()
                .any(|tool| tool.name.starts_with("candidate_"))
        );
    }
}

#[tokio::test]
async fn observer_panic_stops_and_joins_other_started_vm_children() {
    let root = Workspace::new();
    let session = root.session();
    let (agent, model) = make_agent(
        &root.0,
        [response(vec![code(
            "parent",
            r#"
        const slow=tools.exec({command:'printf "%s" "$$" > slow.pid; exec sleep 30',timeout_ms:120000});
        const fast=tools.exec({command:'while [ ! -f slow.pid ]; do sleep 0.01; done; printf FAST_DONE',timeout_ms:120000});
        await Promise.all([slow,fast]); return true;
    "#,
        )])],
        CodeLimits::default(),
    );
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = observed.clone();
    let result=tokio::time::timeout(Duration::from_secs(5),agent.submit(session.clone(),"run".into(),String::new(),CancellationToken::new(),move |event| {
        if matches!(event,ion_core::CodingAgentEvent::ChildToolFinished { outcome:ChildOutcome::Observed { output },.. } if output.value["stdout"]=="FAST_DONE") {
            flag.store(true,std::sync::atomic::Ordering::SeqCst);
            panic!("controlled observer failure while another child is running");
        }
    })).await.unwrap();
    let view = session.view().unwrap();
    let pid = fs::read_to_string(root.0.join("slow.pid")).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .output()
        .unwrap()
        .status
        .success();
    assert!(
        matches!(result, Err(CodingAgentError::ObserverPanicked)),
        "{result:?}"
    );
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!alive, "owned slow child survived joined observer failure");
    assert_eq!(model.requests().len(), 1);
    assert!(view.entries.iter().any(|entry|matches!(entry,SessionEntry::ChildToolResult { outcome:ChildOutcome::Observed { output },.. } if output.value["cancelled"]==true)));
    assert_eq!(
        view.turns()[0].end,
        Some(ion_core::TurnEndReason::Cancelled)
    );
    session.ensure_idle().unwrap();
}

#[tokio::test]
async fn native_fanout_selects_context_and_preserves_occurrences_inspection_and_forks() {
    let root = Workspace::new();
    let session = root.session();
    fs::write(root.0.join("one"), "PRIVATE_INTERMEDIATE_ONE").unwrap();
    fs::write(root.0.join("two"), "PRIVATE_INTERMEDIATE_TWO").unwrap();
    let first = "const def = await describeTool('read');
        const values = await Promise.all(['one','two'].map(path => tools[def.name]({path})));
        await tools.write({path:'changed',content:'changed'});
        const missing = await tools.read({path:'missing'});
        let rejected = false;
        try { await tools.unknown_capability({}); } catch { rejected = true; }
        return {lengths:values.map(r => r.value.content.length), missing:missing.is_error, rejected};";
    let (agent, model) = make_agent(
        &root.0,
        [
            response(vec![
                code("reused", first),
                code(
                    "other",
                    "void tools.write({path:'unawaited',content:'done'}); return true;",
                ),
            ]),
            response(vec![code(
                "reused",
                "const r=await tools.read({path:'one'}); return r.value.content.length;",
            )]),
            response(vec![Content::Text("DONE".into())]),
        ],
        CodeLimits::default(),
    );
    let observed = Arc::new(std::sync::Mutex::new((
        LiveTranscript::with_user_input(&Message::user_input("run".into(), [])),
        false,
    )));
    let observer = observed.clone();
    let result = agent
        .submit(
            session.clone(),
            "run".into(),
            String::new(),
            CancellationToken::new(),
            move |event| {
                let started = matches!(&event, ion_core::CodingAgentEvent::ChildToolStarted { .. });
                let mut state = observer.lock().unwrap();
                let (live, saw_running_child) = &mut *state;
                live.observe(&event);
                if started {
                    *saw_running_child = true;
                    assert!(live.projection().items.iter().any(|item| matches!(item, TranscriptItem::ActivityGroup(group) if group.activities.iter().any(|parent| parent.state == ion_core::ActivityState::Running && parent.children.iter().any(|child| child.state == ion_core::ActivityState::Running)))));
                }
            },
        )
        .await
        .unwrap();
    let observed = observed.lock().unwrap();
    let (live, saw_running_child) = &*observed;
    assert_eq!(result, "DONE");
    assert!(*saw_running_child);
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
    // Unknown capability rejection admits no child; observed native failure does.
    assert_eq!(intents.len(), 6);
    let selected = view
        .entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } if result.call_id == "reused" => {
                match &result.outcome {
                    ion_core::ToolOutcome::Observed { output, .. } => Some(&output.value["result"]),
                    _ => None,
                }
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(selected["missing"], true);
    assert_eq!(selected["rejected"], true);
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
    let reopened = Arc::new(CodingSession::open(root.0.join("session.sqlite")).unwrap());
    assert_eq!(
        TranscriptProjection::from_session(&reopened.view().unwrap()),
        saved
    );
}

#[tokio::test]
async fn guest_budget_excludes_awaited_host_work_and_post_return_settlement() {
    for awaited in [false, true] {
        let root = Workspace::new();
        let session = root.session();
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code(
                    "parent",
                    &format!(
                        "const pending = [0,1,2,3].map(i => tools.exec({{command:`sleep 0.4; printf done > settled-${{i}}`}})); {} return 7;",
                        if awaited {
                            "await Promise.all(pending);"
                        } else {
                            ""
                        }
                    ),
                )]),
                response(vec![Content::Text("DONE".into())]),
            ],
            CodeLimits {
                execution_budget: Duration::from_millis(200),
                max_concurrency: 2,
                max_json_bytes: 4096,
                ..CodeLimits::default()
            },
        );
        agent
            .submit(
                session.clone(),
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
        assert!(session.view().unwrap().entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.call_id == "parent" && matches!(&result.outcome, ion_core::ToolOutcome::Observed { output, .. } if !output.is_error && output.value["result"] == 7))));
        assert_eq!(model.requests().len(), 2);
    }
}

#[tokio::test]
async fn cancellation_settles_native_children_before_return() {
    let root = Workspace::new();
    let session = root.session();
    let (agent, model) = make_agent(
        &root.0,
        [response(vec![code(
            "parent",
            "await Promise.all(['a','b'].map(x => tools.exec({command:`printf started; printf ready > ${x}; exec sleep 30`,timeout_ms:120000}))); return true;",
        )])],
        CodeLimits::default(),
    );
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        agent
            .submit(
                task_session.clone(),
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
            tools.exec({command:'printf first > first; sleep 0.15; printf FIRST'}),
            tools.exec({command:'printf second > second; printf SECOND; exec sleep 30',timeout_ms:120000})]);
         await tools.write({path:'dependent',content:'must not happen'}); return true;")])], CodeLimits::default());
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        agent.submit(
            session.clone(),
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
    let reopened = Arc::new(CodingSession::open(root.0.join("session.sqlite")).unwrap());
    let (next, next_model) = make_agent(
        &root.0,
        [response(vec![Content::Text("RECOVERED".into())])],
        CodeLimits::default(),
    );
    next.submit(
        reopened.clone(),
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
            "await tools.write({path:'effect',content:'must not happen'}); return true;",
        )])],
        CodeLimits::default(),
    );
    let result = agent
        .submit(
            session.clone(),
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
    let reopened = Arc::new(CodingSession::open(root.0.join("session.sqlite")).unwrap());
    let (next, _) = make_agent(
        &root.0,
        [response(vec![Content::Text("RECOVERED".into())])],
        CodeLimits::default(),
    );
    next.submit(
        reopened.clone(),
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
async fn failed_guest_evidence_is_rediscovered_after_cut_and_reopen_without_reexecution() {
    let root = Workspace::new();
    let session = root.session();
    let report = "PRIVATE_RAW_😀é".repeat(300);
    fs::write(root.0.join("report"), &report).unwrap();
    let (agent, model) = make_agent(
        &root.0,
        [
            response(vec![code(
                "original",
                "await tools.read({path:'report'}); await tools.exec({command:'printf ONCE >> effects; printf ORIGINAL_FAILURE >&2; exit 7'}); throw new Error('after effects');",
            )]),
            response(vec![Content::Text("FAILED_GUEST".into())]),
            // Deliberately omit the locator to exercise a lossy summary boundary,
            // not to claim that a live model preserves or understands evidence.
            response(vec![Content::Text("Earlier work omitted.".into())]),
        ],
        CodeLimits::default(),
    );
    agent
        .submit(
            session.clone(),
            "run".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(&model.requests()[1])
            .unwrap()
            .contains("PRIVATE_RAW")
    );
    let parent = session
        .view()
        .unwrap()
        .entries
        .into_iter()
        .find_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } if result.call_id == "original" => {
                Some(result.outcome.inspection_output().value["parent"].clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(
        parent.is_object(),
        "failed guest did not expose a recovery locator"
    );
    assert!(
        agent
            .compact(session.clone(), CancellationToken::new(), |_| {})
            .await
            .unwrap()
    );
    assert!(
        !serde_json::to_string(&session.context_messages().unwrap())
            .unwrap()
            .contains("assistant_entry")
    );
    fs::remove_file(root.0.join("report")).unwrap();
    drop(session);
    let reopened = Arc::new(CodingSession::open(root.0.join("session.sqlite")).unwrap());
    let script = "const parents=await inspectSaved({kind:'parents'});
        const parent=parents.parents[0].parent;
        const meta=await inspectSaved({kind:'children',parent});
        let offset=0, text='';
        do { const page=await inspectSaved({kind:'output',parent,child:0,pointer:'/content',offset,limit:512});
             text+=page.json; offset=page.next_offset; } while(offset!==null);
        const failure=await inspectSaved({kind:'output',parent,child:1});
        return {parents,content:JSON.parse(text), original_failed:failure.is_error, original:JSON.parse(failure.json), states:meta.children.map(c=>c.state)};";
    let (next, next_model) = make_agent(
        &root.0,
        [
            response(vec![code("recover", script)]),
            response(vec![Content::Text("RECOVERED".into())]),
        ],
        CodeLimits::default(),
    );
    next.submit(
        reopened.clone(),
        "recover saved evidence".into(),
        String::new(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    let view = reopened.view().unwrap();
    let output = view
        .entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } if result.call_id == "recover" => {
                Some(result.outcome.inspection_output())
            }
            _ => None,
        })
        .unwrap();
    assert!(!output.is_error, "{:?}", output.value);
    assert_eq!(output.value["calls"], 0);
    assert_eq!(
        output.value["result"]["parents"]["parents"],
        json!([{"parent":parent,"child_count":2}])
    );
    assert_eq!(
        output.value["result"]["parents"]["next_before"],
        serde_json::Value::Null
    );
    assert_eq!(output.value["result"]["content"], report);
    assert_eq!(output.value["result"]["original_failed"], true);
    assert_eq!(output.value["result"]["original"]["exit_code"], 7);
    assert_eq!(
        output.value["result"]["original"]["stderr"],
        "ORIGINAL_FAILURE"
    );
    assert_eq!(
        output.value["result"]["states"],
        json!(["observed", "observed"])
    );
    assert!(
        serde_json::to_string(&next_model.requests()[1])
            .unwrap()
            .contains("PRIVATE_RAW")
    );
    assert_eq!(
        view.entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::ChildToolAdmitted { .. }))
            .count(),
        2
    );
    assert_eq!(fs::read_to_string(root.0.join("effects")).unwrap(), "ONCE");
}

#[tokio::test]
async fn guest_failure_retains_prior_mutation_and_budget_exhaustion_settles_native_capture() {
    for exhaust_budget in [false, true] {
        let root = Workspace::new();
        let session = root.session();
        let script = if exhaust_budget {
            "await tools.write({path:'effect',content:'kept'}); void tools.exec({command:'printf BEFORE_BUDGET; exec sleep 30',timeout_ms:120000}); while(true) {}"
        } else {
            "await tools.write({path:'effect',content:'kept'}); throw new Error('after mutation');"
        };
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code("parent", script)]),
                response(vec![Content::Text("FAULT_REPORTED".into())]),
            ],
            CodeLimits {
                execution_budget: Duration::from_millis(500),
                ..CodeLimits::default()
            },
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            agent.submit(
                session.clone(),
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
        assert!(view.entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.name == "code_mode" && matches!(&result.outcome, ion_core::ToolOutcome::Observed { output, .. } if output.is_error))));
        let child = view
            .entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionEntry::ChildToolResult {
                    outcome: ChildOutcome::Observed { output },
                    ..
                } => Some(output),
                _ => None,
            })
            .expect("started child has a committed observation");
        if exhaust_budget {
            assert_eq!(child.value["cancelled"], true);
            assert!(
                child.value["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("BEFORE_BUDGET")
            );
        } else {
            assert!(!child.is_error);
        }
        assert_eq!(model.requests().len(), 2);
    }
}

#[tokio::test]
async fn only_explicit_text_and_images_reach_model_context_even_after_script_error() {
    for fails in [false, true] {
        let root = Workspace::new();
        let session = root.session();
        image::RgbImage::new(1, 1)
            .save(root.0.join("unselected.png"))
            .unwrap();
        image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]))
            .save(root.0.join("selected.png"))
            .unwrap();
        fs::write(root.0.join("report"), "PRIVATE_OBSERVATION").unwrap();
        let body = format!(
            "await tools.read({{path:'unselected.png'}}); const chosen=await tools.read({{path:'selected.png'}}); const report=await tools.read({{path:'report'}}); text({{length:report.value.content.length}}); image(chosen.images[0]); {}",
            if fails {
                "throw new Error('after selection');"
            } else {
                ""
            }
        );
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code("select", &body)]),
                response(vec![Content::Text("done".into())]),
            ],
            CodeLimits::default(),
        );
        let agent = agent.with_limits(ion_core::AgentLimits {
            image_input: true,
            ..ion_core::AgentLimits::default()
        });
        agent
            .submit(
                session.clone(),
                "select".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let requests = model.requests();
        let result = requests[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|part| match part {
                Content::ToolResult(result) if result.name == "code_mode" => Some(result),
                _ => None,
            })
            .unwrap();
        assert_eq!(result.is_error, fails);
        assert_eq!(
            result.result["output"],
            json!([{"type":"text","text":"{\"length\":19}"}, {"type":"image","index":0}])
        );
        assert_eq!(result.images.len(), 1);
        assert!(
            !serde_json::to_string(&requests[1])
                .unwrap()
                .contains("PRIVATE_OBSERVATION")
        );
        let view = session.view().unwrap();
        let images = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::ChildToolResult {
                    outcome: ChildOutcome::Observed { output },
                    ..
                } if !output.images.is_empty() => Some(&output.images[0]),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(images.len(), 2);
        assert_ne!(images[0], images[1]);
        assert_eq!(&result.images[0], images[1]);
    }
}

#[tokio::test]
async fn caught_output_exhaustion_cannot_dispatch_a_dependent_effect() {
    let root = Workspace::new();
    let session = root.session();
    let (agent, _) = make_agent(
        &root.0,
        [
            response(vec![code(
                "select",
                "try { text('x'.repeat(1000)); } catch (_) {} await tools.write({path:'dependent',content:'bad'}); return true;",
            )]),
            response(vec![Content::Text("done".into())]),
        ],
        CodeLimits {
            max_output_bytes: 128,
            ..CodeLimits::default()
        },
    );
    agent
        .submit(
            session.clone(),
            "select".into(),
            String::new(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(!root.0.join("dependent").exists());
    assert!(
        session
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry,
                SessionEntry::ToolResult { result, .. } if matches!(&result.outcome,
                    ion_core::ToolOutcome::Observed { output, .. } if output.is_error
                )
            ))
    );
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
                "const i=await tools.read({path:'pixel.png'}); if(i.images[0].mime_type!=='image/png') throw 'missing image'; await tools.read({path:'report'}); await tools.read({path:'report'}); await tools.write({path:'dependent',content:'bad'}); return true;",
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
            session.clone(),
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
    // The third case admits and executes the read, then exhausts audit capacity
    // on its observed result before the guest can consume it/start a dependent.
    for (max_json_bytes, audit_admission_bytes, observed) in [
        (1024, 32 * 1024 * 1024, true),
        (1024, 1, false),
        (128 * 1024, 8192, true),
    ] {
        let root = Workspace::new();
        let session = root.session();
        fs::write(root.0.join("report"), "PRIVATE_REPORT_".repeat(4000)).unwrap();
        let (agent, model) = make_agent(
            &root.0,
            [
                response(vec![code(
                    "parent",
                    "const r=await tools.read({path:'report'}); await tools.write({path:'dependent',content:'bad'}); return r.value;",
                )]),
                response(vec![Content::Text("LIMIT_REPORTED".into())]),
            ],
            CodeLimits {
                max_json_bytes,
                audit_admission_bytes,
                ..CodeLimits::default()
            },
        );
        agent
            .submit(
                session.clone(),
                "run".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let view = session.view().unwrap();
        if !observed {
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
        if audit_admission_bytes == 8192 {
            assert!(view.entries.iter().any(|entry| matches!(entry,
                SessionEntry::ToolResult { result, .. }
                if matches!(&result.outcome, ion_core::ToolOutcome::Observed { output, .. } if output.is_error && output.value["error"].as_str().is_some_and(|error| error.contains("audit admission byte limit")))
            )));
        }
        assert!(!root.0.join("dependent").exists());
        assert!(
            !serde_json::to_string(&model.requests()[1])
                .unwrap()
                .contains("PRIVATE_REPORT_")
        );
    }
}

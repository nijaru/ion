//! Exercise the actual MCP request boundary through direct and composed dispatch.
use super::*;
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, Usage,
};
use ion_core::{
    Agent, ChildOutcome, CodeLimits, Session, SessionEntry, ToolDecision, ToolOutcome, ToolPolicy,
    ToolPolicyRequest,
};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Policy(AtomicUsize);
impl ToolPolicy for Policy {
    fn decide<'a>(
        &'a self,
        request: ToolPolicyRequest<'a>,
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolDecision, String>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if request.call.name != "code_mode" {
            assert_eq!(request.call.name, "mcp__7_custody__probe");
            assert_eq!(request.call.arguments, json!({"count":1}));
        }
        Box::pin(async { Ok(ToolDecision::Allow) })
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
async fn registered_input_gate_precedes_mcp_request_and_child_policy_and_reopens_passively() {
    for composed in [false, true] {
        let (root, source, tools) = super::invocation_tests::fixture("schema").await;
        let policy = Arc::new(Policy::default());
        let tools = tools
            .with_policy(policy.clone())
            .with_code_mode(Arc::new(crate::code_mode::QuickJs), CodeLimits::default());
        let scripts = if composed {
            vec![
                response(vec![call(
                    "parent",
                    "code_mode",
                    json!({"code":r#"
                const unavailable = 'mcp__7_custody__unavailable';
                if (unavailable in tools || await describeTool(unavailable) !== null ||
                    (await searchTools('unavailable')).some(tool => tool.name === unavailable)) {
                    throw new Error('unavailable contract was advertised');
                }
                const refused = await tools.mcp__7_custody__probe({count:'1',extra:true}).then(() => false, () => true);
                if (!refused) throw new Error('invalid contract was dispatched');
                return (await tools.mcp__7_custody__probe({count:1})).value.structured_content.calls;
            "#}),
                )]),
                response(vec![Content::Text("done".into())]),
            ]
        } else {
            vec![
                response(vec![
                    call(
                        "invalid",
                        "mcp__7_custody__probe",
                        json!({"count":"1", "extra":true}),
                    ),
                    call("unavailable", "mcp__7_custody__unavailable", json!({})),
                ]),
                response(vec![call(
                    "corrected",
                    "mcp__7_custody__probe",
                    json!({"count":1}),
                )]),
                response(vec![Content::Text("done".into())]),
            ]
        };
        let model = Arc::new(ScriptedModelService::new(scripts));
        let agent = Agent::with_tool_set(
            model.clone(),
            Arc::new(tools),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        );
        assert_eq!(
            agent
                .tool_catalog()
                .unavailable()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["mcp__7_custody__unavailable"]
        );
        let path = root.join("session.sqlite");
        let session = Arc::new(Session::create(&path, Some(&root)).unwrap());
        agent
            .submit(
                session.clone(),
                "exercise input contracts".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(model.requests().iter().all(|request| {
            request
                .tools
                .iter()
                .all(|tool| tool.name != "mcp__7_custody__unavailable")
        }));
        source.shutdown().await.unwrap();
        let calls = fs::read_to_string(root.join("calls")).unwrap();
        let requests: Vec<Value> = calls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests.len(),
            1,
            "invalid input issued an MCP request or a valid call replayed"
        );
        assert_eq!(requests[0]["params"]["name"], "probe");
        assert_eq!(requests[0]["params"]["arguments"], json!({"count":1}));
        assert_eq!(
            policy.0.load(Ordering::SeqCst),
            if composed { 2 } else { 1 }
        );
        let view = session.view().unwrap();
        if composed {
            let children: Vec<_> = view
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    SessionEntry::ChildToolResult { outcome, .. } => Some(outcome),
                    _ => None,
                })
                .collect();
            assert_eq!(children.len(), 2);
            assert!(
                matches!(children[0], ChildOutcome::NotDispatched {reason} if reason.contains("input schema check failed"))
            );
            assert!(matches!(children[1], ChildOutcome::Observed {output} if !output.is_error));
            assert!(view.entries.iter().any(|entry| matches!(entry,
                SessionEntry::ChildToolAdmitted {intent, ..} if intent.call.arguments == json!({"count":"1", "extra":true}))));
            assert!(view.entries.iter().any(|entry| matches!(entry,
                SessionEntry::ToolResult {result, ..} if matches!(&result.outcome, ToolOutcome::Observed {output, ..} if output.value["result"] == 1 && !output.is_error))));
        } else {
            let results: Vec<_> = view
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    SessionEntry::ToolResult { result, .. } => Some(&result.outcome),
                    _ => None,
                })
                .collect();
            assert_eq!(results.len(), 3);
            assert!(
                matches!(results[0], ToolOutcome::NotDispatched {reason} if reason.contains("input schema check failed"))
            );
            assert!(
                matches!(results[1], ToolOutcome::NotDispatched {reason} if reason.contains("not declared"))
            );
            assert!(matches!(results[2], ToolOutcome::Observed {output, ..} if !output.is_error));
        }
        let before = serde_json::to_value(&view.entries).unwrap();
        let model_calls = model.requests().len();
        session.shutdown().await.unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            serde_json::to_value(reopened.view().unwrap().entries).unwrap(),
            before
        );
        assert_eq!(fs::read_to_string(root.join("calls")).unwrap(), calls);
        assert_eq!(model.requests().len(), model_calls);
        reopened.shutdown().await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

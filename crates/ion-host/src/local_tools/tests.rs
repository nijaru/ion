use super::files::hex_digest;
use super::*;
use ion_ai::ToolCall;
use std::{
    fs::{self, File},
    time::Duration,
};

#[tokio::test]
async fn optional_native_inputs_reject_null_before_effects() {
    let root = std::env::temp_dir().join(format!("ion-input-boundary-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file.txt"), "old").unwrap();
    let catalog =
        ion_core::ToolSet::new([Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>])
            .snapshot();
    let mut failures = Vec::new();
    for (name, field, mut arguments) in [
        ("read", "offset", json!({"path":"file.txt"})),
        ("read", "limit", json!({"path":"file.txt"})),
        (
            "edit",
            "base_digest",
            json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}]}),
        ),
        ("ls", "limit", json!({})),
        ("ls", "path", json!({})),
        ("find", "pattern", json!({"pattern":"*"})),
        ("find", "path", json!({"pattern":"*"})),
        ("find", "limit", json!({"pattern":"*"})),
        ("grep", "pattern", json!({"pattern":"needle"})),
        ("grep", "path", json!({"pattern":"needle"})),
        ("grep", "glob", json!({"pattern":"needle"})),
        ("grep", "ignore_case", json!({"pattern":"needle"})),
        ("grep", "literal", json!({"pattern":"needle"})),
        ("grep", "limit", json!({"pattern":"needle"})),
        (
            "exec",
            "timeout_ms",
            json!({"command":"printf effect > marker"}),
        ),
    ] {
        arguments[field] = Value::Null;
        let output = catalog
            .execute_untracked(
                &ToolCall {
                    id: "boundary".into(),
                    name: name.into(),
                    arguments,
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        let unchanged = fs::read(root.join("file.txt")).unwrap() == b"old";
        let no_marker = !root.join("marker").exists();
        if !output.is_error || !unchanged || !no_marker {
            failures.push(format!(
                "{name}.{field}: {} unchanged={unchanged} no_marker={no_marker}",
                output.value
            ));
        }
        // Isolate every probe even on the original, effectful defect.
        fs::write(root.join("file.txt"), "old").unwrap();
        if !no_marker {
            fs::remove_file(root.join("marker")).unwrap();
        }
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn native_schema_keeps_exec_timeout_optional_and_uncapped() {
    // The frozen provider schema is where the optional-and-uncapped
    // timeout contract is declared. Runtime long-timeout behavior belongs
    // to the command owner; assert only the declaration here.
    let catalog = ion_core::ToolSet::new([
        Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
    ])
    .snapshot();
    let specs = catalog.specs();
    let exec = specs
        .iter()
        .find(|spec| spec.name == "exec")
        .expect("exec is registered");
    assert_eq!(exec.input_schema["required"], json!(["command"]));
    assert_eq!(exec.input_schema["additionalProperties"], false);
    assert_eq!(
        exec.input_schema["properties"]["timeout_ms"]["type"],
        "integer"
    );
    assert_eq!(exec.input_schema["properties"]["timeout_ms"]["minimum"], 1);
    // No upper bound: the declaration accepts arbitrarily long positive
    // limits rather than inventing a maximum.
    assert_eq!(
        exec.input_schema["properties"]["timeout_ms"].get("maximum"),
        None
    );
}

#[test]
fn native_schema_requires_edit_digest_and_exposes_write_opt_out() {
    let catalog = ion_core::ToolSet::new([
        Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
    ])
    .snapshot();
    let specs = catalog.specs();
    let edit = specs.iter().find(|spec| spec.name == "edit").unwrap();
    assert!(
        edit.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("base_digest"))
    );
    assert_eq!(
        edit.input_schema["properties"]["base_digest"]["type"],
        "string"
    );
    let write = specs.iter().find(|spec| spec.name == "write").unwrap();
    assert!(
        !write.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("base_digest"))
    );
    assert_eq!(
        write.input_schema["properties"]["unconditional"]["type"],
        "boolean"
    );
}

#[test]
fn native_discovery_schema_and_presentation_are_registered() {
    let catalog = ion_core::ToolSet::new([
        Arc::new(LocalTools::new(std::env::temp_dir()).unwrap()) as Arc<dyn ToolSource>,
    ])
    .snapshot();
    let specs = catalog.specs();
    assert_eq!(specs.len(), 7);
    for (name, kind, key) in [
        ("ls", ToolActivityKind::List, "path"),
        ("find", ToolActivityKind::Search, "pattern"),
        ("grep", ToolActivityKind::Search, "pattern"),
    ] {
        let spec = specs.iter().find(|spec| spec.name == name).unwrap();
        assert_eq!(spec.input_schema["additionalProperties"], false);
        let call = ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: json!({key: "target"}),
            raw_arguments: None,
        };
        let activity = catalog.activity(&call);
        assert_eq!(activity.kind, kind, "{name}");
        assert_eq!(activity.subject.as_deref(), Some("target"));
    }
    let grep = specs.iter().find(|spec| spec.name == "grep").unwrap();
    assert_eq!(grep.input_schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(grep.input_schema["properties"]["limit"]["maximum"], 1000);
    assert_eq!(
        grep.input_schema["properties"]["literal"]["type"],
        "boolean"
    );
    let find = specs.iter().find(|spec| spec.name == "find").unwrap();
    assert_eq!(find.input_schema["required"], json!(["pattern"]));
    let ls = specs.iter().find(|spec| spec.name == "ls").unwrap();
    assert_eq!(ls.input_schema["properties"]["limit"]["maximum"], 1000);
}

#[tokio::test]
async fn concurrent_guarded_writes_admit_one_current_digest() {
    let root = std::env::temp_dir().join(format!("ion-write-race-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "seed").unwrap();
    std::os::unix::fs::symlink("file", root.join("alias")).unwrap();
    let catalogs = [0, 1].map(|_| {
        ion_core::ToolSet::new([Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>])
            .snapshot()
    });
    let calls = [("file", "first"), ("alias", "second")].map(|(path, content)| ToolCall {
        id: content.into(),
        name: "write".into(),
        raw_arguments: None,
        arguments: json!({"path":path,"content":content,"base_digest":hex_digest(b"seed")}),
    });
    let (first, second) = tokio::join!(
        catalogs[0].execute_untracked(&calls[0], CancellationToken::new()),
        catalogs[1].execute_untracked(&calls[1], CancellationToken::new()),
    );
    assert_ne!(first.is_error, second.is_error, "{first:?} {second:?}");
    let (winner, loser) = if first.is_error {
        (&second, &first)
    } else {
        (&first, &second)
    };
    assert_eq!(
        loser.value["error"],
        "file changed since supplied base_digest"
    );
    let bytes = fs::read(root.join("file")).unwrap();
    assert_eq!(winner.value["new_digest"], hex_digest(&bytes));
    assert_eq!(
        bytes,
        if first.is_error {
            b"second".as_slice()
        } else {
            b"first".as_slice()
        }
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn concurrent_native_edits_preserve_guards_and_disjoint_changes() {
    let root = std::env::temp_dir().join(format!("ion-edit-race-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("aliases")).unwrap();
    std::os::unix::fs::symlink("../file", root.join("aliases/link")).unwrap();
    let source = format!("LEFT:\n{}RIGHT:\n", "payload0123456789\n".repeat(200_000));
    let catalogs = [0, 1].map(|_| {
        ion_core::ToolSet::new([Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>])
            .snapshot()
    });
    let mut failures = Vec::new();
    for guarded in [true, false] {
        fs::write(root.join("file"), &source).unwrap();
        let calls = [("file", "LEFT", "1"), ("aliases/link", "RIGHT", "2")].map(|(path, anchor, value)| {
            let mut arguments = json!({"path":path,"edits":[{"old_text":format!("{anchor}:"),"new_text":format!("{anchor}:{value}")}]});
            if guarded { arguments["base_digest"] = json!(hex_digest(source.as_bytes())); }
            ToolCall { id: anchor.into(), name: "edit".into(), arguments, raw_arguments: None }
        });
        let outputs = tokio::join!(
            catalogs[0].execute_untracked(&calls[0], CancellationToken::new()),
            catalogs[1].execute_untracked(&calls[1], CancellationToken::new()),
        );
        let final_bytes = fs::read(root.join("file")).unwrap();
        let final_text = std::str::from_utf8(&final_bytes).unwrap();
        let left = final_text.starts_with("LEFT:1\n");
        let right = final_text.ends_with("RIGHT:2\n");
        let successful = [&outputs.0, &outputs.1]
            .iter()
            .filter(|output| !output.is_error)
            .count();
        let correct = if guarded {
            successful == 1
                && (left ^ right)
                && [&outputs.0, &outputs.1].iter().all(|output| {
                    if output.is_error {
                        output.value["error"] == "file changed since supplied base_digest"
                    } else {
                        output.value["new_digest"] == hex_digest(&final_bytes)
                    }
                })
        } else {
            successful == 0
                && final_bytes == source.as_bytes()
                && [&outputs.0, &outputs.1].iter().all(|output| {
                    output.value["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("requires base_digest"))
                })
        };
        if !correct {
            failures.push(format!(
                "guarded={guarded} successful={successful} left={left} right={right}"
            ));
        }
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn stopped_native_mutations_leave_an_externally_locked_target_unchanged() {
    let root = std::env::temp_dir().join(format!("ion-mutation-stop-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "old").unwrap();
    let holder = File::open(&root).unwrap();
    holder.lock().unwrap();
    let catalog =
        ion_core::ToolSet::new([Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>])
            .snapshot();
    for (name, arguments) in [
        (
            "edit",
            json!({"path":"file","edits":[{"old_text":"old","new_text":"edited"}],"base_digest":hex_digest(b"old")}),
        ),
        (
            "write",
            json!({"path":"file","content":"written","base_digest":hex_digest(b"old")}),
        ),
    ] {
        let call = ToolCall {
            id: name.into(),
            name: name.into(),
            arguments,
            raw_arguments: None,
        };
        let stop = CancellationToken::new();
        let pending = catalog.execute_untracked(&call, stop.clone());
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        stop.cancel();
        let output = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap();
        assert!(
            output.is_error
                && output.value["error"]
                    .as_str()
                    .unwrap()
                    .contains("cancelled"),
            "{}",
            output.value
        );
        assert_eq!(fs::read(root.join("file")).unwrap(), b"old");
    }
    drop(holder);
    let output = files::write(
        &root,
        &json!({"path":"file","content":"released","base_digest":hex_digest(b"old")}),
        &CancellationToken::new(),
        None,
    );
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(fs::read(root.join("file")).unwrap(), b"released");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn queued_file_work_keeps_async_polling_live_and_cancels_before_effects() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root =
            std::env::temp_dir().join(format!("ion-file-dispatch-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file.txt"), "old").unwrap();
        let catalog = ion_core::ToolSet::new([
            Arc::new(LocalTools::new(&root).unwrap()) as Arc<dyn ToolSource>
        ])
        .snapshot();
        for (name, arguments) in [
            ("read", json!({"path":"file.txt"})),
            ("ls", json!({})),
            ("find", json!({"pattern":"*.txt"})),
            ("grep", json!({"pattern":"old"})),
            (
                "edit",
                json!({"path":"file.txt","edits":[{"old_text":"old","new_text":"new"}],"base_digest":hex_digest(b"old")}),
            ),
            ("write", json!({"path":"file.txt","content":"new","base_digest":hex_digest(b"old")})),
        ] {
            // Hold the real blocking-pool resource, not a fake native tool.
            // Cancellation must remain serviceable while dispatch is queued.
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, held) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                held.blocking_recv().unwrap();
            });
            started.await.unwrap();
            let call = ToolCall {
                id: name.into(),
                name: name.into(),
                arguments,
                raw_arguments: None,
            };
            let stop = CancellationToken::new();
            let mut work = Box::pin(catalog.execute_untracked(&call, stop.clone()));
            let observed = tokio::time::timeout(Duration::from_millis(10), &mut work).await;
            let pending = observed.is_err();
            stop.cancel();
            release.send(()).unwrap();
            let output = match observed {
                Ok(output) => output,
                Err(_) => work.await,
            };
            blocker.await.unwrap();
            assert!(pending, "{name} bypassed bounded blocking dispatch");
            assert!(output.is_error, "{name}: {}", output.value);
            assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(), "old");
        }
        fs::remove_dir_all(root).unwrap();
    });
}

#[tokio::test]
async fn turn_cancellation_waits_for_native_tool_settlement_with_or_without_warming() {
    use ion_ai::{
        Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role,
        Script, ScriptedModelService, Usage,
    };
    use ion_core::{
        Agent, AgentError, AgentEvent, AgentLimits, PromptCacheWarmingPolicy, Session,
        SessionEntry, TurnEndReason,
    };

    for warming in [false, true] {
        let root = std::env::temp_dir().join(format!("ion-turn-cancel-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(Session::create(&path, Some(&root)).unwrap());
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "running".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"printf 'OBSERVED_BEFORE_CANCEL\\n'; printf ready > ready; exec sleep 30", "timeout_ms": 120000}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            usage: Usage {
                input_tokens: Some(10_000),
                cache_write_input_tokens: Some(10_000),
                ..Usage::unknown()
            },
            termination: ResponseTermination::Completed,
            returned_model: None,
        };
        let model = Arc::new(ScriptedModelService::new([Script::Stream(vec![
            ModelStreamEvent::Completed(response),
        ])]));
        let agent = Agent::new(
            model.clone(),
            Arc::new(LocalTools::new(&root).unwrap()),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        )
        .with_limits(AgentLimits {
            prompt_cache_warming: warming.then_some(PromptCacheWarmingPolicy {
                lifetime_seconds: 300,
                cache_write_microusd_per_million: 5_000_000,
                cache_read_microusd_per_million: 100_000,
                output_microusd_per_million: 1_000_000,
                minimum_savings_microusd: 1,
            }),
            ..AgentLimits::default()
        });
        let stop = CancellationToken::new();
        let trigger = stop.clone();
        let running_session = session.clone();
        let task = tokio::spawn(async move {
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            let observed = events.clone();
            let result = agent
                .submit(
                    running_session.clone(),
                    "run".into(),
                    String::new(),
                    stop,
                    move |event| observed.lock().unwrap().push(event),
                )
                .await;
            let events = std::mem::take(&mut *events.lock().unwrap());
            (result, events)
        });
        let ready = tokio::time::timeout(Duration::from_secs(4), async {
            while !root.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        trigger.cancel(); // Also stop the task if startup failed.
        let (result, events) = tokio::time::timeout(Duration::from_secs(4), task)
            .await
            .expect("Turn cancellation did not settle")
            .unwrap();
        ready.expect("native command did not start");
        assert!(matches!(result, Err(AgentError::Cancelled)), "{result:?}");
        let view = session.view().unwrap();
        let output = view
            .entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::ToolResult { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        let ion_core::ToolOutcome::Observed { output, .. } = &output.outcome else {
            panic!("missing observed cancellation")
        };
        assert_eq!(
            output.value["cancelled"], true,
            "must record observed cancellation, not a synthetic unknown effect: {output:?}"
        );
        assert_eq!(output.value["stdout"], "OBSERVED_BEFORE_CANCEL\n");
        assert!(output.value["exit_code"].is_number() || output.value["signal"].is_number());
        assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolFinished { outcome: ion_core::ToolOutcome::Observed { output, .. }, .. } if output.value["cancelled"] == true)));
        assert!(matches!(
            view.entries.last(),
            Some(SessionEntry::TurnEnded {
                reason: TurnEndReason::Cancelled,
                ..
            })
        ));
        assert_eq!(
            model.requests().len(),
            1,
            "no dependent request or warming after cancellation"
        );
        drop(session);
        let reopened = Arc::new(Session::open(&path).unwrap());
        assert_eq!(reopened.view().unwrap().entries, view.entries);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

use std::{fs, path::PathBuf};

use ion_ai::{Content, Message, ModelExecution, ModelRef, Role, ToolCall, ToolResult, Usage};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{HistoryError, Session};
use crate::{ForkPoint, SessionEntry, ToolOutcome, ToolResultProjection, ToolSettlement};

fn fixture() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("ion-history-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    (root.clone(), root.join("session.sqlite"))
}

fn model() -> ModelRef {
    ModelRef {
        provider: "test".into(),
        model: "test".into(),
    }
}

fn execution() -> ModelExecution {
    ModelExecution {
        route: ion_ai::ModelRoute::direct(model(), ion_ai::ModelRouteReason::Auxiliary),
        returned_model: None,
    }
}

fn settlement(call_id: &str, name: &str, outcome: ToolOutcome) -> ToolSettlement {
    ToolSettlement {
        call_id: call_id.into(),
        name: name.into(),
        outcome,
    }
}

fn history_query(session: &Session, value: Value) -> Result<Value, HistoryError> {
    session
        .prepare_history(&serde_json::to_string(&value).unwrap())?
        .read(&CancellationToken::new())
}

fn read_all(session: &Session, entry: u64, limit: usize) -> Result<String, HistoryError> {
    let mut offset = 0;
    let mut document = String::new();
    loop {
        let value = history_query(
            session,
            json!({"action":"read","entry":entry,"offset":offset,"limit":limit}),
        )?;
        let page = &value["entry"];
        document.push_str(page["content"].as_str().unwrap());
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset as u64);
                offset = usize::try_from(next).unwrap();
            }
            None => break,
        }
    }
    Ok(document)
}

#[test]
fn compaction_keeps_original_tool_output_bounded_states_and_utf8_pages() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn("investigate historical evidence".into(), model())
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Text("calling exec".into()),
                    Content::ToolCall(ToolCall {
                        id: "duplicate-call-id".into(),
                        name: "exec".into(),
                        arguments: json!({"command":"historical command"}),
                        raw_arguments: None,
                    }),
                ],
                provider_replay: Some(ion_ai::ProviderReplay::new(
                    "test",
                    "opaque",
                    json!("REPLAY_SECRET_ALPHA"),
                )),
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let large_output = json!({
        "stdout": format!("HISTORY_NEEDLE_START 😀 {}", "x".repeat(12_000)),
        "exit_code": 0
    });
    session
        .record_tool_result_with_context(
            turn,
            ToolResult {
                call_id: "duplicate-call-id".into(),
                name: "exec".into(),
                result: large_output.clone(),
                images: Vec::new(),
                is_error: false,
            },
            ToolResultProjection::RequestLimitExceeded,
            None,
        )
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "not-dispatched".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"never dispatch"}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    session
        .record_tool_settlement_with_context(
            turn,
            settlement(
                "not-dispatched",
                "exec",
                ToolOutcome::NotDispatched {
                    reason: "test dispatch refusal".into(),
                },
            ),
            None,
        )
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking("THINKING_SECRET_ALPHA".into()),
                    Content::Text("answer after evidence".into()),
                ],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let (second, _) = session.begin_turn("later turn".into(), model()).unwrap();
    session
        .record_assistant(
            second,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("later answer".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let plan = session
        .compaction_plan(0, usize::MAX, true, u64::MAX)
        .unwrap()
        .unwrap();
    session
        .record_compaction(
            plan.through_entry,
            "summary omits the historical needle".into(),
            execution(),
            Usage::unknown(),
        )
        .unwrap();
    let view = session.view().unwrap();
    let context = session.context_messages().unwrap();
    let summary = context[0]
        .content
        .iter()
        .find_map(|part| match part {
            Content::Text(text) => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(summary.contains(&format!("entries 1 through {}", plan.through_entry)));
    assert!(!summary.contains("HISTORY_NEEDLE_START"));
    let tool_entry = view
        .entries
        .iter()
        .position(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.call_id == "duplicate-call-id"))
        .unwrap() as u64
        + 1;

    let found = history_query(
        &session,
        json!({"action":"search","query":"history_needle_start"}),
    )
    .unwrap();
    assert_eq!(found["provenance"]["historical"], true);
    assert_eq!(found["provenance"]["through_entry"], view.entries.len());
    assert_eq!(found["entries"].as_array().unwrap().len(), 1);
    let hit = &found["entries"][0];
    assert_eq!(hit["entry"], tool_entry);
    assert_eq!(hit["kind"], "tool_result");
    assert_eq!(hit["state"], "observed");
    assert_eq!(hit["is_error"], false);
    assert_eq!(
        hit["tool"],
        json!({"call_id":"duplicate-call-id","name":"exec"})
    );
    assert!(
        hit["preview"]
            .as_str()
            .unwrap()
            .contains("HISTORY_NEEDLE_START")
    );

    let document = read_all(&session, tool_entry, 7).unwrap();
    let decoded: Value = serde_json::from_str(&document).unwrap();
    assert_eq!(decoded, large_output);
    assert!(!document.contains("output_withheld"));
    assert!(!document.contains("REPLAY_SECRET_ALPHA"));
    let emoji = document.find('😀').unwrap();
    assert!(
        history_query(
            &session,
            json!({"action":"read","entry":tool_entry,"offset":emoji + 1,"limit":32})
        )
        .is_err()
    );

    let not_dispatched_entry = view
        .entries
        .iter()
        .position(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.call_id == "not-dispatched"))
        .unwrap() as u64
        + 1;
    let not_dispatched = read_all(&session, not_dispatched_entry, 64).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&not_dispatched).unwrap(),
        json!({"state":"not_dispatched","reason":"test dispatch refusal"})
    );

    // Search is literal and model-visible only: thinking and opaque replay are
    // not part of the record corpus.
    for hidden in ["THINKING_SECRET_ALPHA", "REPLAY_SECRET_ALPHA"] {
        let hidden = history_query(&session, json!({"action":"search","query":hidden})).unwrap();
        assert!(hidden["entries"].as_array().unwrap().is_empty(), "{hidden}");
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        session
            .prepare_history(&json!({"action":"list"}).to_string())
            .unwrap()
            .read(&cancelled)
            .is_err()
    );

    // Reopen and a copy preserve the exact coordinate and original output.
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(read_all(&reopened, tool_entry, 4096).unwrap(), document);
    let clone = reopened.clone_to(root.join("clone.sqlite")).unwrap();
    assert_eq!(read_all(&clone, tool_entry, 4096).unwrap(), document);
    let fork = reopened
        .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(1))
        .unwrap();
    assert_eq!(read_all(&fork, tool_entry, 4096).unwrap(), document);
    drop((fork, clone, reopened));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tool_result_documents_preserve_json_scalar_types() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session.begin_turn("scalars".into(), model()).unwrap();
    let values = [
        json!(true),
        json!(false),
        json!(null),
        json!("line\n雪"),
        json!(42),
        json!(1.5),
    ];
    let calls = values
        .iter()
        .enumerate()
        .map(|(index, _)| {
            Content::ToolCall(ToolCall {
                id: format!("scalar-{index}"),
                name: "scalar".into(),
                arguments: json!({}),
                raw_arguments: None,
            })
        })
        .collect::<Vec<_>>();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: calls,
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    for (index, value) in values.iter().enumerate() {
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: format!("scalar-{index}"),
                    name: "scalar".into(),
                    result: value.clone(),
                    images: Vec::new(),
                    is_error: false,
                },
            )
            .unwrap();
    }
    let view = session.view().unwrap();
    for (index, value) in values.iter().enumerate() {
        let entry = view
            .entries
            .iter()
            .position(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.call_id == format!("scalar-{index}")))
            .unwrap() as u64
            + 1;
        assert_eq!(
            read_all(&session, entry, 64).unwrap(),
            serde_json::to_string(value).unwrap()
        );
    }
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_metadata_is_bounded_while_entry_coordinates_stay_authoritative() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn("bounded metadata".into(), model())
        .unwrap();
    let long_call_id = format!(
        "CALL_PREFIX{}雪{}",
        "x".repeat(super::MAX_METADATA_BYTES - "CALL_PREFIX".len() - 1),
        "y".repeat(1_024)
    );
    let long_name = format!("exec{}", "n".repeat(1_024));
    let exact_call_id = format!(
        "EXACT{}",
        "z".repeat(super::MAX_METADATA_BYTES - "EXACT".len())
    );
    let exact_name = format!(
        "name{}",
        "z".repeat(super::MAX_METADATA_BYTES - "name".len())
    );
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::ToolCall(ToolCall {
                        id: long_call_id.clone(),
                        name: long_name.clone(),
                        arguments: json!({}),
                        raw_arguments: None,
                    }),
                    Content::ToolCall(ToolCall {
                        id: exact_call_id.clone(),
                        name: exact_name.clone(),
                        arguments: json!({}),
                        raw_arguments: None,
                    }),
                ],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    for (call_id, name) in [
        (long_call_id.clone(), long_name.clone()),
        (exact_call_id.clone(), exact_name.clone()),
    ] {
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id,
                    name,
                    result: json!({"ok":true}),
                    images: Vec::new(),
                    is_error: false,
                },
            )
            .unwrap();
    }
    let view = session.view().unwrap();
    let long_entry = view
        .entries
        .iter()
        .position(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if result.call_id == long_call_id))
        .unwrap() as u64
        + 1;
    let found = history_query(&session, json!({"action":"search","query":"CALL_PREFIX"})).unwrap();
    assert_eq!(found["entries"].as_array().unwrap().len(), 1, "{found}");
    let hit = &found["entries"][0];
    assert_eq!(hit["entry"], long_entry);
    assert_eq!(hit["tool_truncated"], true);
    let expected_call_id = &long_call_id[..super::MAX_METADATA_BYTES - 1];
    let expected_name = &long_name[..super::MAX_METADATA_BYTES];
    assert_eq!(hit["tool"]["call_id"], expected_call_id);
    assert_eq!(hit["tool"]["name"], expected_name);
    let read = history_query(&session, json!({"action":"read","entry":long_entry})).unwrap();
    let metadata = &read["entry"];
    assert_eq!(metadata["tool_truncated"], true);
    assert_eq!(metadata["tool"]["call_id"], expected_call_id);
    assert_eq!(metadata["tool"]["name"], expected_name);
    assert!(
        read_all(&session, long_entry, 64)
            .unwrap()
            .contains("{\"ok\":true}")
    );

    let exact = history_query(&session, json!({"action":"search","query":"EXACT"})).unwrap();
    assert_eq!(exact["entries"].as_array().unwrap().len(), 1, "{exact}");
    let exact_hit = &exact["entries"][0];
    assert!(exact_hit.get("tool_truncated").is_none());
    assert_eq!(exact_hit["tool"]["call_id"], exact_call_id);
    assert_eq!(exact_hit["tool"]["name"], exact_name);
    let exact_entry = exact_hit["entry"].as_u64().unwrap();
    let exact_read = history_query(&session, json!({"action":"read","entry":exact_entry})).unwrap();
    assert!(exact_read["entry"].get("tool_truncated").is_none());
    assert_eq!(exact_read["entry"]["tool"]["call_id"], exact_call_id);
    assert_eq!(exact_read["entry"]["tool"]["name"], exact_name);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn private_shell_thinking_replay_and_image_bytes_are_excluded_from_search_and_read() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let private = session
        .admit_user_shell("printf PRIVATE_SHELL_COMMAND".into(), true)
        .unwrap();
    session
        .record_user_shell(private, json!({"stdout":"PRIVATE_SHELL_OUTPUT"}), false)
        .unwrap();
    let public = session
        .admit_user_shell("printf PUBLIC_SHELL_COMMAND".into(), false)
        .unwrap();
    session
        .record_user_shell(public, json!({"stdout":"PUBLIC_SHELL_OUTPUT"}), false)
        .unwrap();
    let (turn, _) = session
        .begin_turn("inspect picture".into(), model())
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking("THINKING_SECRET_BETA".into()),
                    Content::Text("calling picture".into()),
                    Content::ToolCall(ToolCall {
                        id: "picture-call".into(),
                        name: "picture".into(),
                        arguments: json!({}),
                        raw_arguments: None,
                    }),
                ],
                provider_replay: Some(ion_ai::ProviderReplay::new(
                    "test",
                    "opaque",
                    json!("REPLAY_SECRET_BETA"),
                )),
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    let image = ion_ai::ImageContent::from_bytes(PNG).unwrap();
    let image_prefix = image.data()[..24].to_owned();
    session
        .record_tool_result(
            turn,
            ToolResult {
                call_id: "picture-call".into(),
                name: "picture".into(),
                result: json!({"path":"picture.png"}),
                images: vec![image],
                is_error: false,
            },
        )
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("picture observed".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let view = session.view().unwrap();
    let private_entry = view
        .entries
        .iter()
        .position(|entry| matches!(entry, SessionEntry::UserShellAdmitted { command, .. } if command.contains("PRIVATE_SHELL_COMMAND")))
        .unwrap() as u64
        + 1;
    for hidden in [
        "PRIVATE_SHELL_COMMAND",
        "PRIVATE_SHELL_OUTPUT",
        "THINKING_SECRET_BETA",
        "REPLAY_SECRET_BETA",
        image_prefix.as_str(),
    ] {
        let found = history_query(&session, json!({"action":"search","query":hidden})).unwrap();
        assert!(
            found["entries"].as_array().unwrap().is_empty(),
            "{hidden}: {found}"
        );
    }
    assert!(history_query(&session, json!({"action":"read","entry":private_entry})).is_err());
    let public_found = history_query(
        &session,
        json!({"action":"search","query":"PUBLIC_SHELL_COMMAND"}),
    )
    .unwrap();
    assert_eq!(public_found["entries"].as_array().unwrap().len(), 1);
    assert_eq!(public_found["entries"][0]["state"], "observed");
    let public_entry = public_found["entries"][0]["entry"].as_u64().unwrap();
    let public_document = read_all(&session, public_entry, 64).unwrap();
    assert!(public_document.contains("PUBLIC_SHELL_COMMAND"));
    assert!(public_document.contains("PUBLIC_SHELL_OUTPUT"));
    assert!(!public_document.contains("PRIVATE"));

    let picture = history_query(&session, json!({"action":"search","query":"picture"})).unwrap();
    let picture_hit = picture["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "tool_result")
        .unwrap();
    assert_eq!(picture_hit["image_count"], 1);
    assert_eq!(picture_hit["images_omitted"], true);
    let picture_entry = picture_hit["entry"].as_u64().unwrap();
    let picture_document = read_all(&session, picture_entry, 64).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&picture_document).unwrap(),
        json!({"path":"picture.png"})
    );
    assert!(!picture_document.contains(&image_prefix));
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_cursor_scan_bound_and_committed_upper_snapshot_are_truthful() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    // Leave the matching record beyond one bounded page of newer physical
    // entries.
    let needle = session
        .admit_user_shell("printf OLD_UNIQUE_NEEDLE".into(), false)
        .unwrap();
    session
        .record_user_shell(needle, json!({"stdout":"OLD_UNIQUE_NEEDLE"}), false)
        .unwrap();
    for index in 0..(super::MAX_SCAN_ENTRIES + 8) {
        let admission = session
            .admit_user_shell(format!("printf filler-{index}"), false)
            .unwrap();
        session
            .record_user_shell(
                admission,
                json!({"stdout":format!("filler-{index}")}),
                false,
            )
            .unwrap();
    }
    let committed_upper = session.entry_count().unwrap();
    let prepared = session
        .prepare_history(&json!({"action":"search","query":"OLD_UNIQUE_NEEDLE"}).to_string())
        .unwrap();
    let first = prepared.read(&CancellationToken::new()).unwrap();
    assert!(first["entries"].as_array().unwrap().is_empty(), "{first}");
    assert!(first["scanned_entries"].as_u64().unwrap() <= super::MAX_SCAN_ENTRIES as u64);
    let mut cursor = first["next_before"].as_u64().unwrap();
    let mut found = None;
    for _ in 0..8 {
        let page = history_query(
            &session,
            json!({"action":"search","query":"OLD_UNIQUE_NEEDLE","before":cursor}),
        )
        .unwrap();
        if let Some(hit) = page["entries"].as_array().unwrap().first() {
            found = Some(hit["entry"].as_u64().unwrap());
            break;
        }
        cursor = page["next_before"].as_u64().unwrap();
    }
    assert_eq!(found, Some(needle));

    // A prepared read cannot see later commits, including a read of their exact
    // entry coordinate.
    let stale_read = session
        .prepare_history(&json!({"action":"read","entry":committed_upper + 1}).to_string())
        .unwrap();
    let later = session
        .admit_user_shell("printf LATER_COMMIT".into(), false)
        .unwrap();
    session
        .record_user_shell(later, json!({"stdout":"LATER_COMMIT"}), false)
        .unwrap();
    let bounded = prepared.read(&CancellationToken::new()).unwrap();
    assert_eq!(bounded["provenance"]["through_entry"], committed_upper);
    assert!(
        bounded["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["entry"].as_u64().unwrap() <= committed_upper)
    );
    assert!(stale_read.read(&CancellationToken::new()).is_err());
    assert_eq!(session.entry_count().unwrap(), later + 1);
    assert!(
        session
            .prepare_history(&json!({"action":"search","query":""}).to_string())
            .is_err()
    );
    assert!(
        session
            .prepare_history(&json!({"action":"list","limit":0}).to_string())
            .is_err()
    );
    for invalid in [
        json!({"action":"search","query":"x","before":null}),
        json!({"action":"list","before":null}),
    ] {
        assert!(
            session.prepare_history(&invalid.to_string()).is_err(),
            "{invalid}"
        );
    }
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepared_history_does_not_join_shell_settlement_committed_after_its_bound() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let admission = session
        .admit_user_shell("printf PREPARED_SHELL_COMMAND".into(), false)
        .unwrap();
    let prepared_read = session
        .prepare_history(&json!({"action":"read","entry":admission}).to_string())
        .unwrap();
    let prepared_search = session
        .prepare_history(&json!({"action":"search","query":"PREPARED_SHELL_OUTPUT"}).to_string())
        .unwrap();
    session
        .record_user_shell(admission, json!({"stdout":"PREPARED_SHELL_OUTPUT"}), false)
        .unwrap();

    let bounded = prepared_read.read(&CancellationToken::new()).unwrap();
    assert_eq!(bounded["provenance"]["through_entry"], admission);
    let document: Value =
        serde_json::from_str(bounded["entry"]["content"].as_str().unwrap()).unwrap();
    assert_eq!(document["outcome"]["state"], "unknown");
    assert!(!document.to_string().contains("PREPARED_SHELL_OUTPUT"));

    let search = prepared_search.read(&CancellationToken::new()).unwrap();
    assert!(search["entries"].as_array().unwrap().is_empty(), "{search}");
    let fresh = read_all(&session, admission, 64).unwrap();
    assert!(fresh.contains("PREPARED_SHELL_OUTPUT"));
    assert!(fresh.contains("\"state\":\"observed\""));
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_charges_a_shell_settlement_outside_its_cursor_window() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let output = format!("HUGE_SETTLEMENT_OUTPUT{}", "x".repeat(9 * 1024 * 1024));
    let admission = session
        .admit_user_shell("printf HUGE_SETTLEMENT_COMMAND".into(), false)
        .unwrap();
    session
        .record_user_shell(admission, json!({"stdout":output}), false)
        .unwrap();
    let page = history_query(
        &session,
        json!({
            "action":"search",
            "query":"HUGE_SETTLEMENT_OUTPUT",
            "before":admission + 1
        }),
    )
    .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["entries"][0]["entry"], admission);
    assert_eq!(page["entries"][0]["state"], "observed");
    assert!(
        page["scanned_bytes"].as_u64().unwrap() >= output.len() as u64,
        "{page}"
    );
    assert_eq!(page["oversized_entry"], true);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn list_is_newest_first_and_pages_without_repeating_or_skipping_entries() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    for index in 0..12 {
        let admission = session
            .admit_user_shell(format!("printf list-{index}"), false)
            .unwrap();
        session
            .record_user_shell(admission, json!({"stdout":format!("list-{index}")}), false)
            .unwrap();
    }
    let first = history_query(&session, json!({"action":"list","limit":5})).unwrap();
    let entries = first["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0]["entry"], 23);
    assert_eq!(entries[4]["entry"], 15);
    let next = first["next_before"].as_u64().unwrap();
    assert_eq!(next, 15);
    let second = history_query(&session, json!({"action":"list","limit":5,"before":next})).unwrap();
    let entries = second["entries"].as_array().unwrap();
    assert_eq!(entries[0]["entry"], 13);
    assert_eq!(entries[4]["entry"], 5);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

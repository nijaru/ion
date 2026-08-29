from pathlib import Path

# Tests that exercise ChildManager directly must now create the durable control
# parent they claim to own. The schema intentionally enforces this relation.
p = Path("crates/ion-core/src/tests/budget_children.rs")
text = p.read_text()
old = '''async fn durable_child_handles_support_spawn_status_wait_and_cancel() {\n    let store = SessionStore::open_in_memory().expect("store");\n    let parent = crate::SessionId::generate();\n    let (manager, tools) = crate::child_tools(\n'''
new = '''async fn durable_child_handles_support_spawn_status_wait_and_cancel() {\n    let store = SessionStore::open_in_memory().expect("store");\n    let parent_runtime = Runtime::start_with_store(\n        ScriptedProvider::new(Vec::new()),\n        ToolRegistry::default(),\n        store.clone(),\n    );\n    let parent = parent_runtime.session_id();\n    parent_runtime\n        .session()\n        .snapshot()\n        .await\n        .expect("persist parent session");\n    let (manager, tools) = crate::child_tools(\n'''
if old not in text:
    raise SystemExit("durable child parent fixture anchor missing")
text = text.replace(old, new, 1)
old = '''    manager.close().await.expect("close children");\n}\n\nfn delegate_tool(\n'''
new = '''    manager.close().await.expect("close children");\n    parent_runtime.session().close().await.expect("close parent");\n    parent_runtime.join().await.expect("join parent");\n}\n\nfn delegate_tool(\n'''
if old not in text:
    raise SystemExit("durable child parent cleanup anchor missing")
text = text.replace(old, new, 1)

old = '''async fn delegate_reports_child_lifecycle_progress() {\n    let store = SessionStore::open_in_memory().expect("store");\n    let tool = delegate_tool(\n        store,\n        vec![ScriptedMessage::text("child answer")],\n        crate::SessionId::generate(),\n        crate::RuntimeBudget::unbounded(),\n    );\n'''
new = '''async fn delegate_reports_child_lifecycle_progress() {\n    let store = SessionStore::open_in_memory().expect("store");\n    let parent_runtime = Runtime::start_with_store(\n        ScriptedProvider::new(Vec::new()),\n        ToolRegistry::default(),\n        store.clone(),\n    );\n    let parent_id = parent_runtime.session_id();\n    parent_runtime\n        .session()\n        .snapshot()\n        .await\n        .expect("persist parent session");\n    let tool = delegate_tool(\n        store,\n        vec![ScriptedMessage::text("child answer")],\n        parent_id,\n        crate::RuntimeBudget::unbounded(),\n    );\n'''
if old not in text:
    raise SystemExit("progress parent fixture anchor missing")
text = text.replace(old, new, 1)
old = '''    assert!(\n        updates.iter().any(|update| update.contains("finished")),\n        "missing child-finish progress: {updates:?}"\n    );\n}\n'''
new = '''    assert!(\n        updates.iter().any(|update| update.contains("finished")),\n        "missing child-finish progress: {updates:?}"\n    );\n    parent_runtime.session().close().await.expect("close parent");\n    parent_runtime.join().await.expect("join parent");\n}\n'''
if old not in text:
    raise SystemExit("progress parent cleanup anchor missing")
text = text.replace(old, new, 1)

# The fork boundary is captured while the parent delegate tool is executing.
# Later tool-result/assistant commits advance main, so comparing against the
# final main leaf is conceptually wrong. Assert that the durable source is the
# actual delegate ToolCall boundary and remains on the final main ancestry.
old = '''    let child = store.load(child_id).await.expect("child session");\n    let parent_main_leaf = parent\n        .lanes\n        .iter()\n        .find(|lane| lane.name == crate::session::lane::MAIN)\n        .expect("parent main lane")\n        .state\n        .leaf;\n    assert_eq!(child.session.control_parent_session_id, Some(parent_id));\n    assert_eq!(child.session.fork_source_session_id, Some(parent_id));\n    assert_eq!(child.session.fork_source_entry_id, parent_main_leaf);\n    assert_eq!(child.session.initial_model_ref, "child-model");\n'''
new = '''    let child = store.load(child_id).await.expect("child session");\n    let fork_entry_id = child\n        .session\n        .fork_source_entry_id\n        .expect("fork source entry");\n    let fork_entry = parent\n        .entries\n        .iter()\n        .find(|record| record.id == fork_entry_id)\n        .expect("fork source belongs to parent history");\n    assert!(matches!(\n        &fork_entry.entry,\n        crate::SessionEntry::ToolCall { call } if call.name == "delegate"\n    ));\n    let final_main_leaf = parent\n        .lanes\n        .iter()\n        .find(|lane| lane.name == crate::session::lane::MAIN)\n        .expect("parent main lane")\n        .state\n        .leaf;\n    let mut cursor = final_main_leaf;\n    let mut source_is_ancestor = false;\n    while let Some(entry_id) = cursor {\n        if entry_id == fork_entry_id {\n            source_is_ancestor = true;\n            break;\n        }\n        cursor = parent\n            .entries\n            .iter()\n            .find(|record| record.id == entry_id)\n            .expect("main ancestry is complete")\n            .parent;\n    }\n    assert!(source_is_ancestor, "fork source must remain on final main ancestry");\n    assert_eq!(child.session.control_parent_session_id, Some(parent_id));\n    assert_eq!(child.session.fork_source_session_id, Some(parent_id));\n    assert_eq!(child.session.initial_model_ref, "child-model");\n'''
if old not in text:
    raise SystemExit("fork point-in-time assertion anchor missing")
text = text.replace(old, new, 1)
p.write_text(text)

p = Path("crates/ion-core/src/tests/child_lifecycle.rs")
text = p.read_text()
old = '''async fn completed_children_release_live_child_slots() {\n    let store = SessionStore::open_in_memory().expect("store");\n    let parent = crate::SessionId::generate();\n    let (manager, tools) = crate::child_tools(\n'''
new = '''async fn completed_children_release_live_child_slots() {\n    let store = SessionStore::open_in_memory().expect("store");\n    let parent_runtime = Runtime::start_with_store(\n        ScriptedProvider::new(Vec::new()),\n        ToolRegistry::default(),\n        store.clone(),\n    );\n    let parent = parent_runtime.session_id();\n    parent_runtime\n        .session()\n        .snapshot()\n        .await\n        .expect("persist parent session");\n    let (manager, tools) = crate::child_tools(\n'''
if old not in text:
    raise SystemExit("child lifecycle parent fixture anchor missing")
text = text.replace(old, new, 1)
old = '''    manager.close().await.expect("close children");\n    store.close().await.expect("close store");\n}\n'''
new = '''    manager.close().await.expect("close children");\n    parent_runtime.session().close().await.expect("close parent");\n    parent_runtime.join().await.expect("join parent");\n    store.close().await.expect("close store");\n}\n'''
if old not in text:
    raise SystemExit("child lifecycle parent cleanup anchor missing")
p.write_text(text.replace(old, new, 1))

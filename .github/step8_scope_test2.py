from pathlib import Path

p = Path("crates/ion-core/src/tests/agent_store.rs")
s = p.read_text()
anchor = '''#[tokio::test]
async fn session_agent_admission_rejects_capability_escalation() {'''
if s.count(anchor) != 1:
    raise SystemExit(f"expected one anchor, got {s.count(anchor)}")

test = '''#[tokio::test]
async fn lane_agent_admission_rejects_structural_scope_widening() {
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: session_id,
            cwd: "/tmp/root".to_owned(),
            title: String::new(),
            initial_model_ref: "model-a".to_owned(),
            control_parent_session_id: None,
            fork_source_session_id: None,
            fork_source_entry_id: None,
        })
        .await
        .expect("root session");
    let root_agent = crate::AgentId::root(session_id);
    let mut parent = crate::session::lane::Config::new("model-a");
    parent.tools = crate::tool::ToolSelection::Only(std::collections::BTreeSet::from([
        "read".to_owned(),
    ]));
    parent.scopes = crate::session::lane::ScopeGrant::from_published(
        std::collections::BTreeSet::from(["scope-a".to_owned()]),
    );
    store
        .set_lane_config(session_id, crate::session::lane::MAIN, parent.clone())
        .await
        .expect("parent config");

    let rejected = crate::AgentId::generate();
    let mut wider = parent.clone();
    wider.scopes = crate::session::lane::ScopeGrant::from_published(
        std::collections::BTreeSet::from(["scope-a".to_owned(), "scope-b".to_owned()]),
    );
    let err = store
        .admit_lane_agent(
            session_id,
            rejected,
            root_agent,
            "scope-wider",
            None,
            wider,
        )
        .await
        .expect_err("shared-history admission must reject wider scopes");
    assert!(err.to_string().contains("may narrow but not escalate"));
    let loaded = store.load(session_id).await.expect("rejected load");
    assert!(loaded.agents.iter().all(|agent| agent.id != rejected));
    assert!(loaded.lanes.iter().all(|lane| lane.name != "scope-wider"));

    let admitted = crate::AgentId::generate();
    let mut narrowed = parent;
    narrowed.scopes = crate::session::lane::ScopeGrant::none();
    store
        .admit_lane_agent(
            session_id,
            admitted,
            root_agent,
            "scope-narrowed",
            None,
            narrowed.clone(),
        )
        .await
        .expect("shared-history admission may narrow scopes");
    let loaded = store.load(session_id).await.expect("admitted load");
    let lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == "scope-narrowed")
        .expect("narrowed lane");
    assert_eq!(lane.config, narrowed);

    store.close().await.expect("close store");
}

'''
s = s.replace(anchor, test + anchor, 1)
p.write_text(s)
print("shared-lane scope invariant patch applied")

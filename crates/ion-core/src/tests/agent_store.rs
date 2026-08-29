use super::support::*;

#[tokio::test]
async fn lane_agent_identity_and_lane_are_published_atomically() {
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: session_id,
            cwd: "/tmp".to_owned(),
            title: String::new(),
            initial_model_ref: "model-a".to_owned(),
            parent_session_id: None,
        })
        .await
        .expect("session");

    let root_agent = crate::AgentId::root(session_id);
    let loaded = store.load(session_id).await.expect("root load");
    assert_eq!(loaded.agents.len(), 1);
    assert_eq!(loaded.agents[0].id, root_agent);
    assert!(matches!(
        loaded.agents[0].history,
        crate::store::AgentHistory::Root
    ));

    let root = EntryRecord::provision(
        1,
        crate::SessionEntry::UserMessage {
            text: "shared root".to_owned(),
        },
    );
    let root_entry = root.id;
    store
        .append_entry(session_id, crate::session::lane::MAIN, root)
        .await
        .expect("root entry");

    let worker = crate::AgentId::generate();
    store
        .admit_lane_agent(
            session_id,
            worker,
            root_agent,
            "agent-worker",
            Some(root_entry),
            crate::session::lane::Config::new("model-a"),
        )
        .await
        .expect("agent admission");

    let loaded = store.load(session_id).await.expect("agent load");
    let agent = loaded
        .agents
        .iter()
        .find(|agent| agent.id == worker)
        .expect("worker agent");
    assert_eq!(agent.control_parent_id, Some(root_agent));
    assert_eq!(agent.session_id, session_id);
    assert_eq!(agent.lane_name, "agent-worker");
    assert!(matches!(
        agent.history,
        crate::store::AgentHistory::SharedLane {
            source_session_id,
            source_entry_id: Some(entry),
        } if source_session_id == session_id && entry == root_entry
    ));
    let lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == "agent-worker")
        .expect("agent lane");
    assert_eq!(lane.state.leaf, Some(root_entry));

    // The lane insert happens before the agent insert inside one transaction.
    // Reusing the immutable AgentId therefore forces the later insert to fail;
    // the preceding lane insert must roll back with it.
    assert!(
        store
            .admit_lane_agent(
                session_id,
                worker,
                root_agent,
                "must-not-survive",
                Some(root_entry),
                crate::session::lane::Config::new("model-a"),
            )
            .await
            .is_err()
    );
    let loaded = store.load(session_id).await.expect("post-failure load");
    assert_eq!(loaded.agents.len(), 2);
    assert!(
        loaded
            .lanes
            .iter()
            .all(|lane| lane.name != "must-not-survive")
    );
}

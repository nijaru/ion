from pathlib import Path

p = Path("crates/ion-core/src/tests/agent_store.rs")
s = p.read_text()

def one(old, new):
    global s
    if s.count(old) != 1:
        raise SystemExit(f"expected one match, got {s.count(old)}")
    s = s.replace(old, new, 1)

one(
'''    parent_config.tools =
        crate::tool::ToolSelection::Only(std::collections::BTreeSet::from(["read".to_owned()]));
    store''',
'''    parent_config.tools =
        crate::tool::ToolSelection::Only(std::collections::BTreeSet::from(["read".to_owned()]));
    parent_config.scopes = crate::session::lane::ScopeGrant::from_published(
        std::collections::BTreeSet::from(["scope-a".to_owned()]),
    );
    store''')

one(
'''    assert!(matches!(
        store.load(rejected_session).await,
        Err(crate::store::StoreError::NotFound(id)) if id == rejected_session
    ));

    let admitted_session = crate::SessionId::generate();''',
'''    assert!(matches!(
        store.load(rejected_session).await,
        Err(crate::store::StoreError::NotFound(id)) if id == rejected_session
    ));

    let scope_rejected_session = crate::SessionId::generate();
    let mut wider_scopes = crate::session::lane::Config::new("child-model");
    wider_scopes.tools = parent_config.tools.clone();
    wider_scopes.scopes = crate::session::lane::ScopeGrant::from_published(
        std::collections::BTreeSet::from(["scope-a".to_owned(), "scope-b".to_owned()]),
    );
    let err = store
        .admit_session_agent(
            SessionRecord {
                id: scope_rejected_session,
                cwd: "/tmp/root".to_owned(),
                title: String::new(),
                initial_model_ref: "child-model".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
            wider_scopes,
        )
        .await
        .expect_err("hosted admission must reject wider structural scopes");
    assert!(err.to_string().contains("may narrow but not escalate"));
    assert!(matches!(
        store.load(scope_rejected_session).await,
        Err(crate::store::StoreError::NotFound(id)) if id == scope_rejected_session
    ));

    let admitted_session = crate::SessionId::generate();''')

one(
'''    let mut child_config = crate::session::lane::Config::new("child-model");
    child_config.tools = parent_config.tools.clone();
    store''',
'''    let mut child_config = crate::session::lane::Config::new("child-model");
    child_config.tools = parent_config.tools.clone();
    child_config.scopes = crate::session::lane::ScopeGrant::none();
    store''')

p.write_text(s)
print("hosted scope invariant patch applied")

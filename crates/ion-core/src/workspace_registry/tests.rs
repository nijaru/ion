use super::*;

#[test]
fn read_only_inspection_keeps_missing_registry_absent_and_claim_unresolved() {
    let home = std::env::temp_dir().join(format!("ion-registry-inspect-{}", SessionId::new()));
    let host = home.join("host");
    let root = home.join("root");
    assert!(WorkspaceRegistry::unresolved_existing(&host, None, 10).is_err());
    assert!(!home.exists());
    fs::create_dir_all(&root).unwrap();
    let mut registry = WorkspaceRegistry::open(&host).unwrap();
    let binding = registry.bind("workspace", &root, "local").unwrap();
    let key = ClaimKey {
        session: SessionId::new(),
        invocation: InvocationId::new(1).unwrap(),
        attempt: AttemptId::new(2).unwrap(),
    };
    registry
        .admit(
            &binding,
            key,
            WorkspaceResources::Files,
            registry.revision(&binding).unwrap(),
        )
        .unwrap();
    let claims = WorkspaceRegistry::unresolved_existing(&host, None, 10).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].key, key);
    assert!(claims[0].terminal.is_none());
    assert_eq!(registry.unresolved(None, 10).unwrap(), claims);
    drop(registry);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn git_markers_reject_static_fifo_and_symlink_without_opening_them() {
    if std::env::var_os("ION_GIT_MARKER_TEST_CHILD").is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "workspace_registry::tests::git_markers_reject_static_fifo_and_symlink_without_opening_them"])
            .env("ION_GIT_MARKER_TEST_CHILD", "1")
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "marker child failed: {status}");
                return;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Git marker verification blocked on a special file");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    use std::os::unix::fs::symlink;
    let home = std::env::temp_dir().join(format!("ion-registry-special-{}", SessionId::new()));
    let root = home.join("root");
    fs::create_dir_all(&root).unwrap();
    let mut registry = WorkspaceRegistry::open(home.join("host")).unwrap();
    let marker = root.join(".git");
    let fifo = home.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    symlink(&fifo, &marker).unwrap();
    assert!(matches!(
        registry.bind("workspace", &root, "local"),
        Err(RegistryError::Invalid)
    ));
    fs::remove_file(&marker).unwrap();
    fs::rename(&fifo, &marker).unwrap();
    assert!(matches!(
        registry.bind("workspace", &root, "local"),
        Err(RegistryError::Invalid)
    ));
    fs::remove_file(&marker).unwrap();
    fs::create_dir(&marker).unwrap();
    let common = marker.join("commondir");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&common)
            .status()
            .unwrap()
            .success()
    );
    assert!(matches!(
        registry.bind("workspace", &root, "local"),
        Err(RegistryError::Invalid)
    ));
    drop(registry);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn registry_namespace_cannot_be_bound_as_a_workspace_or_descendant() {
    let home = std::env::temp_dir().join(format!("ion-registry-overlap-{}", SessionId::new()));
    let host = home.join("host");
    let stage = host.join("staging");
    let sibling = home.join("sibling");
    fs::create_dir_all(&stage).unwrap();
    fs::create_dir_all(&sibling).unwrap();
    let mut registry = WorkspaceRegistry::open(&host).unwrap();
    for root in [&home, &host, &stage] {
        assert!(matches!(
            registry.bind("unsafe", root, "local"),
            Err(RegistryError::RegistryInWorkspace)
        ));
    }
    registry.bind("safe", &sibling, "local").unwrap();
    drop(registry);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn failed_resolution_and_revision_exhaustion_preserve_quarantine() {
    let home = std::env::temp_dir().join(format!("ion-registry-fault-{}", SessionId::new()));
    let root = home.join("root");
    fs::create_dir_all(&root).unwrap();
    let mut registry = WorkspaceRegistry::open(home.join("host")).unwrap();
    let binding = registry.bind("workspace", &root, "local").unwrap();
    let key = ClaimKey {
        session: SessionId::new(),
        invocation: InvocationId::new(1).unwrap(),
        attempt: AttemptId::new(2).unwrap(),
    };
    let base = registry.revision(&binding).unwrap();
    registry
        .admit(&binding, key, WorkspaceResources::Files, base)
        .unwrap();
    let evidence = TerminalEvidence {
        receipt: RegistryReceipt {
            backend: "local".into(),
            identity: "joined".into(),
        },
        effect: EffectSummary::MayHaveMutated,
    };
    registry.connection.execute_batch("CREATE TRIGGER fail_release BEFORE UPDATE ON claims BEGIN SELECT RAISE(ABORT, 'injected write fault'); END;").unwrap();
    assert!(matches!(
        registry.resolve(key, evidence.clone()),
        Err(RegistryError::Sql(_))
    ));
    assert_eq!(registry.revision(&binding).unwrap(), base);
    assert!(registry.claim(key).unwrap().terminal.is_none());
    registry
        .connection
        .execute_batch(
            "DROP TRIGGER fail_release; UPDATE bindings SET revision=9223372036854775807;",
        )
        .unwrap();
    assert!(matches!(
        registry.resolve(key, evidence),
        Err(RegistryError::Capacity)
    ));
    assert!(registry.claim(key).unwrap().terminal.is_none());
    drop(registry);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn failed_admission_never_publishes_a_claim() {
    let home = std::env::temp_dir().join(format!("ion-registry-fault-{}", SessionId::new()));
    let root = home.join("root");
    fs::create_dir_all(&root).unwrap();
    let mut registry = WorkspaceRegistry::open(home.join("host")).unwrap();
    let binding = registry.bind("workspace", &root, "local").unwrap();
    registry.connection.execute_batch("CREATE TRIGGER fail_admission BEFORE INSERT ON claims BEGIN SELECT RAISE(ABORT, 'injected write fault'); END;").unwrap();
    let key = ClaimKey {
        session: SessionId::new(),
        invocation: InvocationId::new(1).unwrap(),
        attempt: AttemptId::new(2).unwrap(),
    };
    assert!(matches!(
        registry.admit(
            &binding,
            key,
            WorkspaceResources::Files,
            registry.revision(&binding).unwrap()
        ),
        Err(RegistryError::Sql(_))
    ));
    assert!(registry.unresolved(None, 10).unwrap().is_empty());
    drop(registry);
    fs::remove_dir_all(home).unwrap();
}

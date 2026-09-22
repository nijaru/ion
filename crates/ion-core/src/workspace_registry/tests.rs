use super::*;

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

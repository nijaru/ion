use std::process::Command;

use ion_core::{
    AttemptId, InvocationId, SessionId,
    workspace_registry::{ClaimKey, WorkspaceRegistry, WorkspaceResources},
};

#[test]
fn claims_cli_reads_existing_quarantine_without_initializing_or_releasing_it() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing");
    let absent = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args(["claims", "--registry", missing.to_str().expect("utf8 path")])
        .output()
        .expect("inspect missing registry");
    assert!(!absent.status.success());
    assert!(!missing.exists());

    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let host = directory.path().join("host");
    let mut registry = WorkspaceRegistry::open(&host).expect("registry");
    let binding = registry
        .bind("workspace", &workspace, "local")
        .expect("binding");
    let key = ClaimKey {
        session: SessionId::new(),
        invocation: InvocationId::new(1).expect("invocation"),
        attempt: AttemptId::new(2).expect("attempt"),
    };
    registry
        .admit(
            &binding,
            key,
            WorkspaceResources::Files,
            registry.revision(&binding).expect("revision"),
        )
        .expect("claim");
    let first = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args([
            "claims",
            "--registry",
            host.to_str().expect("utf8 host"),
            "--limit",
            "1",
        ])
        .output()
        .expect("inspect claim");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let page: serde_json::Value = serde_json::from_slice(&first.stdout).expect("page JSON");
    assert_eq!(page["claims"].as_array().expect("claims").len(), 1);
    assert!(page["claims"][0]["terminal"].is_null());
    let cursor = page["next_after"].as_str().expect("cursor");
    let next = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args([
            "claims",
            "--registry",
            host.to_str().expect("utf8 host"),
            "--after",
            cursor,
        ])
        .output()
        .expect("inspect next page");
    assert!(next.status.success());
    let page: serde_json::Value = serde_json::from_slice(&next.stdout).expect("next page JSON");
    assert!(page["claims"].as_array().expect("claims").is_empty());
    assert_eq!(
        registry.unresolved(None, 10).expect("retained claim").len(),
        1
    );
}

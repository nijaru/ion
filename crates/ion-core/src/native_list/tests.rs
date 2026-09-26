use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::*;
use crate::{AttemptId, AuthorityCeiling, InvocationId, SessionId};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ion-native-list-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_dir_all(self.0.with_extension("host-registry"));
    }
}

fn boundary(root: &Path) -> NativeListBoundary {
    let mut registry = WorkspaceRegistry::open(root.with_extension("host-registry")).unwrap();
    let workspace = registry.bind("test-workspace", root, "local-v1").unwrap();
    let boundary = NativeListBoundary::new(&registry, workspace).unwrap();
    boundary.set_live_authority(LiveToolAuthority::Allow);
    boundary
}

fn execution(
    boundary: &NativeListBoundary,
    action: PreparedAction,
    output_limit: usize,
) -> ToolExecution {
    ToolExecution {
        session: SessionId::new(),
        invocation: InvocationId::new(1).unwrap(),
        attempt: AttemptId::new(1).unwrap(),
        effect_key: "native-list-test".into(),
        binding: boundary.binding(),
        action,
        workspace: boundary.workspace.clone(),
        ceiling: AuthorityCeiling {
            workspace_mutation: false,
            unconfined_execution: false,
            remote_tools: false,
            egress_realms: vec![EgressRealm::Local],
        },
        approval: ApprovalState::NotRequired,
        output_limit,
        progress: crate::ToolProgressPublisher::disabled(),
        artifacts: crate::ArtifactPublisher::closed(),
    }
}

async fn list(boundary: &NativeListBoundary, args: Value, output_limit: usize) -> ToolAttemptState {
    let action = boundary.prepare(args).unwrap();
    boundary
        .execute(
            execution(boundary, action, output_limit),
            CancellationToken::new(),
        )
        .await
}

fn result(state: ToolAttemptState) -> ToolResult {
    match state {
        ToolAttemptState::Settled { result, .. } => result,
        other => panic!("expected settled list, got {other:?}"),
    }
}

#[tokio::test]
async fn lists_sorted_pages_and_directory_types_without_traversing_symlinks() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new();
    fs::write(root.path().join("zeta.txt"), "z").unwrap();
    fs::write(root.path().join("alpha.txt"), "a").unwrap();
    fs::create_dir(root.path().join("docs")).unwrap();
    fs::write(root.path().join("docs/page.md"), "page").unwrap();
    symlink(root.path().join("docs"), root.path().join("linked-docs")).unwrap();
    let boundary = boundary(root.path());

    let first = result(list(&boundary, json!({"limit": 2}), 4096).await);
    assert!(!first.is_error);
    assert_eq!(
        first.value["entries"],
        json!([
            {"name": "alpha.txt", "kind": "file"},
            {"name": "docs", "kind": "directory"}
        ])
    );
    assert_eq!(first.value["has_more"], true);
    assert_eq!(first.value["next_after"], "docs");
    assert!(first.value["workspace_revision"]["files"].is_u64());

    let second = result(list(&boundary, json!({"after": "docs", "limit": 2}), 4096).await);
    assert_eq!(
        second.value["entries"],
        json!([
            {"name": "linked-docs", "kind": "symlink"},
            {"name": "zeta.txt", "kind": "file"}
        ])
    );
    assert_eq!(second.value["has_more"], false);
    assert_eq!(
        result(list(&boundary, json!({"path": "docs"}), 4096).await).value["entries"],
        json!([{"name": "page.md", "kind": "file"}])
    );
    assert_eq!(
        result(list(&boundary, json!({"path": "linked-docs"}), 4096).await).value["error"],
        "path could not be opened as a directory"
    );
}

#[tokio::test]
async fn page_shrinks_to_output_bound_and_resumes_after_last_returned_name() {
    let root = TestRoot::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(root.path().join(name), "").unwrap();
    }
    let boundary = boundary(root.path());
    let one = result(list(&boundary, json!({"limit": 1}), 4096).await);
    let one_byte_size = serde_json::to_vec(&one).unwrap().len();
    let first = result(list(&boundary, json!({"limit": 3}), one_byte_size).await);
    assert!(!first.is_error);
    assert_eq!(
        first.value["entries"],
        json!([{"name": "a.txt", "kind": "file"}])
    );
    assert_eq!(first.value["has_more"], true);
    assert_eq!(first.value["next_after"], "a.txt");
    let second = result(list(&boundary, json!({"after": "a.txt"}), 4096).await);
    assert_eq!(
        second.value["entries"],
        json!([
            {"name": "b.txt", "kind": "file"},
            {"name": "c.txt", "kind": "file"}
        ])
    );
}

#[tokio::test]
async fn rejects_escape_forgery_cancel_and_live_revocation() {
    let root = TestRoot::new();
    let boundary = boundary(root.path());
    for args in [
        json!({"path": "../outside"}),
        json!({"path": "/tmp"}),
        json!({"path": "./docs"}),
        json!({"after": "nested/name"}),
        json!({"limit": 0}),
        json!({"limit": MAX_NATIVE_LIST_ENTRIES + 1}),
        json!({"unexpected": true}),
    ] {
        assert!(matches!(
            boundary.prepare(args),
            Err(ToolBoundaryError::InvalidArguments)
        ));
    }
    let action = boundary.prepare(json!({})).unwrap();
    assert_eq!(
        action.arguments,
        json!({"path": ".", "after": null, "limit": 100})
    );
    let mut changed = action.clone();
    changed.arguments["path"] = json!("other");
    assert!(matches!(
        boundary
            .execute(
                execution(&boundary, changed, 4096),
                CancellationToken::new()
            )
            .await,
        ToolAttemptState::NotStarted { .. }
    ));
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(
        boundary
            .execute(execution(&boundary, action.clone(), 4096), stop)
            .await,
        ToolAttemptState::NotStarted { .. }
    ));
    boundary.set_live_authority(LiveToolAuthority::Deny);
    assert_eq!(
        boundary.live_authority(&action, &boundary.workspace),
        LiveToolAuthority::Deny
    );
    assert!(matches!(
        boundary
            .execute(execution(&boundary, action, 4096), CancellationToken::new())
            .await,
        ToolAttemptState::NotStarted { .. }
    ));
}

#[tokio::test]
async fn root_replacement_is_rejected_before_list_admission() {
    let root = TestRoot::new();
    let boundary = boundary(root.path());
    let moved = root.path().with_extension("old");
    fs::rename(root.path(), &moved).unwrap();
    fs::create_dir(root.path()).unwrap();
    let action = boundary.prepare(json!({})).unwrap();
    assert!(matches!(
        boundary
            .execute(execution(&boundary, action, 4096), CancellationToken::new())
            .await,
        ToolAttemptState::NotStarted { .. }
    ));
    fs::remove_dir_all(moved).unwrap();
}

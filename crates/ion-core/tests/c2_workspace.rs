//! Durable exclusion for cooperating hosts using the same workspace root.
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use ion_core::{ScriptedTool, Stop, Tool, ToolOutcome, Workspace};

fn root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ion-workspace-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("workspace");
    root
}

fn call() -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        name: "edit".to_owned(),
        arguments: serde_json::json!({"path": "notes.txt"}),
    }
}

fn completed() -> Arc<ScriptedTool> {
    Arc::new(ScriptedTool::new(
        "edit",
        [ToolOutcome::Completed(serde_json::json!({}))],
    ))
}

async fn assert_blocked(root: &std::path::Path) {
    let second = completed();
    let workspace = Workspace::open(root).expect("reopen");
    let tool = workspace.bind(second.clone());
    let outcome = tool.execute(&call(), &Stop::new()).await;
    assert!(
        matches!(outcome, ToolOutcome::KnownFailure(_)),
        "{outcome:?}"
    );
    assert!(second.calls().is_empty(), "claim must precede execution");
}

#[tokio::test]
async fn an_uncertain_action_blocks_another_owner_after_reopen() {
    let root = root();
    let first = Arc::new(ScriptedTool::new(
        "edit",
        [ToolOutcome::Indeterminate("lost contact".into())],
    ));
    {
        let workspace = Workspace::open(&root).expect("first owner");
        let tool = workspace.bind(first.clone());
        assert!(matches!(
            tool.execute(&call(), &Stop::new()).await,
            ToolOutcome::Indeterminate(_)
        ));
    }
    assert_eq!(first.calls().len(), 1);
    assert_blocked(&root).await;
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn a_known_result_releases_the_claim() {
    let root = root();
    for _ in 0..2 {
        let workspace = Workspace::open(&root).expect("open");
        let underlying = completed();
        let tool = workspace.bind(underlying.clone());
        assert!(matches!(
            tool.execute(&call(), &Stop::new()).await,
            ToolOutcome::Completed(_)
        ));
        assert_eq!(underlying.calls().len(), 1);
    }
    std::fs::remove_dir_all(root).expect("cleanup");
}

struct WaitingTool {
    started: tokio::sync::Notify,
    finish: tokio::sync::Notify,
}

impl Tool for WaitingTool {
    fn spec(&self) -> ToolSpec {
        completed().spec()
    }
    fn identity(&self) -> String {
        "waiting-1".into()
    }
    fn execute<'a>(&'a self, _: &'a ToolCall, _: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async {
            self.started.notify_one();
            self.finish.notified().await;
            ToolOutcome::Completed(serde_json::json!({}))
        })
    }
}

#[tokio::test]
async fn independent_bindings_cannot_execute_concurrently() {
    let root = root();
    let underlying = Arc::new(WaitingTool {
        started: tokio::sync::Notify::new(),
        finish: tokio::sync::Notify::new(),
    });
    let tool = Workspace::open(&root)
        .expect("open")
        .bind(underlying.clone());
    let execution = tokio::spawn(async move { tool.execute(&call(), &Stop::new()).await });
    tokio::time::timeout(Duration::from_secs(5), underlying.started.notified())
        .await
        .expect("started");
    assert_blocked(&root).await;
    underlying.finish.notify_one();
    assert!(matches!(
        execution.await.expect("join"),
        ToolOutcome::Completed(_)
    ));
    let tool = Workspace::open(&root).expect("reopen").bind(completed());
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::Completed(_)
    ));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn dropping_an_execution_future_does_not_release_its_claim() {
    let root = root();
    let underlying = Arc::new(WaitingTool {
        started: tokio::sync::Notify::new(),
        finish: tokio::sync::Notify::new(),
    });
    let tool = Workspace::open(&root)
        .expect("open")
        .bind(underlying.clone());
    let execution = tokio::spawn(async move { tool.execute(&call(), &Stop::new()).await });
    tokio::time::timeout(Duration::from_secs(5), underlying.started.notified())
        .await
        .expect("started");
    execution.abort();
    assert!(execution.await.expect_err("aborted").is_cancelled());
    assert_blocked(&root).await;
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn a_preexisting_stop_does_not_claim_or_execute() {
    let root = root();
    let underlying = completed();
    let tool = Workspace::open(&root)
        .expect("open")
        .bind(underlying.clone());
    let stop = Stop::new();
    stop.request();
    assert!(matches!(
        tool.execute(&call(), &stop).await,
        ToolOutcome::KnownFailure(_)
    ));
    assert!(underlying.calls().is_empty());
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::Completed(_)
    ));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn panic_keeps_the_claim_and_a_bad_coordinator_never_executes() {
    let root = root();
    let tool = Workspace::open(&root)
        .expect("open")
        .bind(support::PanickingTool::new("edit"));
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::Indeterminate(_)
    ));
    assert_blocked(&root).await;
    std::fs::remove_dir_all(&root).expect("cleanup");

    let workspace = root.join("new");
    std::fs::create_dir_all(&workspace).expect("new workspace");
    let underlying = completed();
    let binding = Workspace::open(&workspace).expect("open");
    let tool = binding.bind(underlying.clone());
    let db = rusqlite::Connection::open(workspace.join(".ion/claims.sqlite")).expect("raw db");
    db.pragma_update(None, "user_version", 99)
        .expect("unknown version");
    drop(db);
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::KnownFailure(_)
    ));
    assert!(underlying.calls().is_empty());
    assert!(Workspace::open(&workspace).is_err());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn failed_claim_and_failed_release_never_authorize_an_extra_action() {
    let root = root();
    let binding = Workspace::open(&root).expect("open");
    let underlying = completed();
    let tool = binding.bind(underlying.clone());
    let db = rusqlite::Connection::open(root.join(".ion/claims.sqlite")).expect("raw db");
    db.execute_batch("CREATE TRIGGER refuse_claim BEFORE INSERT ON claim BEGIN SELECT RAISE(ABORT, 'claim fault'); END;")
        .expect("inject claim fault");
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::KnownFailure(_)
    ));
    assert!(underlying.calls().is_empty());
    db.execute_batch("DROP TRIGGER refuse_claim; CREATE TRIGGER refuse_release BEFORE DELETE ON claim BEGIN SELECT RAISE(ABORT, 'release fault'); END;")
        .expect("inject release fault");
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::Indeterminate(_)
    ));
    assert_eq!(underlying.calls().len(), 1);
    drop(db);
    assert_blocked(&root).await;
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn oversized_arguments_are_refused_before_claim_or_execution() {
    let root = root();
    let underlying = completed();
    let tool = Workspace::open(&root)
        .expect("open")
        .bind(underlying.clone());
    let mut large = call();
    large.arguments = serde_json::json!({"text": "x".repeat(64 * 1024)});
    assert!(matches!(
        tool.execute(&large, &Stop::new()).await,
        ToolOutcome::KnownFailure(_)
    ));
    assert!(underlying.calls().is_empty());
    assert!(matches!(
        tool.execute(&call(), &Stop::new()).await,
        ToolOutcome::Completed(_)
    ));
    std::fs::remove_dir_all(root).expect("cleanup");
}

struct ProcessTool(std::path::PathBuf);
impl Tool for ProcessTool {
    fn spec(&self) -> ToolSpec {
        completed().spec()
    }
    fn identity(&self) -> String {
        "process-test-1".into()
    }
    fn execute<'a>(&'a self, _: &'a ToolCall, _: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async {
            std::fs::write(self.0.join("started"), b"started").expect("signal parent");
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn separate_sessions_share_the_workspace_claim() {
    use ion_ai::{Script, ScriptedModelService};
    use ion_core::{Session, SubmitRequest, ToolRegistry};
    let root = root();
    let underlying = Arc::new(WaitingTool {
        started: tokio::sync::Notify::new(),
        finish: tokio::sync::Notify::new(),
    });
    let mut tools = ToolRegistry::new();
    tools.insert(
        Workspace::open(&root)
            .expect("workspace")
            .bind(underlying.clone()),
    );
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(support::stream(support::tool_answer(call()))),
        Script::Stream(support::stream(support::answer("done"))),
    ]));
    let mut spec = support::spec();
    spec.config.tool_names = vec!["edit".into()];
    let mut first = Session::create(
        &root.join("first.sqlite"),
        spec.clone(),
        support::services(model, tools),
    )
    .await
    .expect("first session");
    let first_handle = first.handle();
    let turn = first_handle
        .submit(SubmitRequest::user("edit"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    tokio::time::timeout(Duration::from_secs(5), underlying.started.notified())
        .await
        .expect("started");

    let second_tool = completed();
    let mut tools = ToolRegistry::new();
    tools.insert(
        Workspace::open(&root)
            .expect("workspace")
            .bind(second_tool.clone()),
    );
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(support::stream(support::tool_answer(call()))),
        Script::Stream(support::stream(support::answer("blocked"))),
    ]));
    let mut second = Session::create(
        &root.join("second.sqlite"),
        spec,
        support::services(model, tools),
    )
    .await
    .expect("second session");
    let handle = second.handle();
    let other = handle
        .submit(SubmitRequest::user("edit"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    tokio::time::timeout(Duration::from_secs(5), handle.wait(other))
        .await
        .expect("bounded wait")
        .expect("wait");
    assert!(second_tool.calls().is_empty());
    let view = handle.turn(other).await.expect("view").expect("turn");
    assert!(matches!(
        view.invocations[0].state,
        ion_core::InvocationState::Failed { .. }
    ));

    underlying.finish.notify_one();
    tokio::time::timeout(Duration::from_secs(5), first_handle.wait(turn))
        .await
        .expect("bounded wait")
        .expect("wait");
    assert!(first.close().await.expect("close first").is_closed());
    assert!(second.close().await.expect("close second").is_closed());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn a_root_nested_below_a_coordinated_workspace_is_refused() {
    let outer = root();
    Workspace::open(&outer).expect("outer coordinator");
    let nested = outer.join("packages").join("inner");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let error = Workspace::open(&nested).expect_err("a nested root must be refused");
    assert!(error.to_string().contains("nested"), "{error}");
    assert!(
        !nested.join(".ion").exists(),
        "a refused root must not gain a second coordinator"
    );
    std::fs::remove_dir_all(outer).expect("cleanup");
}

#[test]
fn an_unrelated_ancestor_file_does_not_refuse_a_nested_root() {
    let outer = root();
    std::fs::create_dir_all(outer.join(".ion")).expect("metadata dir");
    std::fs::write(
        outer.join(".ion").join("claims.sqlite"),
        b"not a coordinator",
    )
    .expect("unrelated file");
    let nested = outer.join("inner");
    std::fs::create_dir_all(&nested).expect("nested dir");
    Workspace::open(&nested).expect("unrelated files are not coordinators");
    std::fs::remove_dir_all(outer).expect("cleanup");
}

// Run only by the parent below, in its own OS process and runtime.
#[test]
fn claim_owner_process() {
    let Some(root) = std::env::var_os("ION_CLAIM_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let tool = Workspace::open(&root)
        .expect("open child")
        .bind(Arc::new(ProcessTool(root)));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(tool.execute(&call(), &Stop::new()));
    panic!("the operation must not return");
}

#[test]
fn killing_the_owner_does_not_make_the_workspace_available() {
    let root = root();
    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "claim_owner_process"])
        .env("ION_CLAIM_TEST_ROOT", &root)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn child");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("started").exists() && Instant::now() < deadline {
        if child.try_wait().expect("child status").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = root.join("started").exists();
    let _ = child.kill();
    child.wait().expect("join killed child");
    assert!(started, "child never reached execution");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(assert_blocked(&root));
    std::fs::remove_dir_all(root).expect("cleanup");
}

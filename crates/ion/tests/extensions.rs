//! Subprocess extension tests (DESIGN.md §24): language-neutral
//! stdio transport, ordinary tool contract, typed crash failures, and
//! the project trust gate for executable configuration.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use ion::settings::{Settings, load_extension_defs};
use ion_core::{
    ExtensionDef, ExtensionService, Runtime, RuntimeEvent, ScriptedMessage, ScriptedProvider,
    SessionStore, ToolCatalog,
};

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn ext_def(script: &str) -> ExtensionDef {
    ExtensionDef {
        name: "textkit".to_owned(),
        command: "python3".to_owned(),
        args: vec![fixture(script)],
    }
}

#[tokio::test]
async fn extension_publishes_and_serves_tools_through_the_catalog() {
    let catalog = ToolCatalog::default();
    ExtensionService::new()
        .start_into(&[ext_def("fake_extension.py")], &catalog)
        .await;

    // Same logical shape as MCP tools: namespaced, schema-carrying.
    let spec = catalog
        .specs()
        .into_iter()
        .find(|spec| spec.name == "textkit__upper")
        .expect("extension tool registered");
    assert_eq!(spec.description, "Uppercase the text");

    let outcome = catalog
        .execute(
            "textkit__upper",
            &json!({ "text": "hello" }),
            CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.output);
    assert_eq!(outcome.output, "HELLO");
}

#[tokio::test]
async fn extension_crash_is_a_typed_failure_and_the_runtime_survives() {
    let catalog = ToolCatalog::default();
    ExtensionService::new()
        .start_into(&[ext_def("crashing_extension.py")], &catalog)
        .await;
    // The ghost tool is present while the peer is live.
    assert!(catalog.specs().iter().any(|s| s.name == "textkit__ghost"));

    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "textkit__ghost".to_owned(),
            arguments: json!({}),
        },
        ScriptedMessage::text("still here"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let policy = Arc::new(ion_core::AllowlistPolicy::new(["textkit__ghost"]));
    let runtime = Runtime::start_with_policy(provider, catalog.clone(), store.clone(), policy);
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    loop {
        let Ok(event) = events.recv().await else {
            break;
        };
        if matches!(event, RuntimeEvent::OperationFinished { .. }) {
            break;
        }
    }
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // The crash surfaced as a model-visible error naming the
    // extension; the operation then completed normally.
    let loaded = store.load(session_id).await.expect("load");
    let transcript = loaded
        .entries
        .iter()
        .map(|(_, entry)| serde_json::to_string(entry).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        transcript.contains("extension `textkit` crashed"),
        "{transcript}"
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !catalog
                .specs()
                .iter()
                .any(|spec| spec.name == "textkit__ghost")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a dead extension must be removed from future capability snapshots");
}

#[tokio::test]
async fn a_second_language_registers_the_same_logical_tool() {
    // Bash fixture: the transport contract holds regardless of the
    // extension's runtime (§24.2).
    let catalog = ToolCatalog::default();
    ExtensionService::new()
        .start_into(
            &[ExtensionDef {
                name: "pingkit".to_owned(),
                command: "bash".to_owned(),
                args: vec![fixture("ping_extension.sh")],
            }],
            &catalog,
        )
        .await;
    let outcome = catalog
        .execute("pingkit__ping", &json!({}), CancellationToken::new())
        .await;
    assert!(!outcome.is_error, "{}", outcome.output);
    assert_eq!(outcome.output, "pong");
}

#[tokio::test]
async fn project_extensions_respect_workspace_trust() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let project = dir.path();
    std::fs::create_dir_all(project.join(".ion")).expect("mkdir");
    std::fs::write(
        project.join(".ion").join("extensions.toml"),
        "[[extensions]]\nname = \"proj\"\ncommand = \"python3\"\nargs = [\"x.py\"]\n",
    )
    .expect("manifest");

    let settings = Settings::empty();

    // Untrusted: skipped with a visible warning, not loaded silently.
    let untrusted = load_extension_defs(&settings, Some(project), false);
    assert!(untrusted.is_empty());

    // Trusted: explicit grant loads the manifest.
    let trusted = load_extension_defs(&settings, Some(project), true);
    assert_eq!(trusted.len(), 1);
    assert_eq!(trusted[0].name, "proj");

    // No project root: only user settings (none here).
    assert!(load_extension_defs(&settings, None, true).is_empty());
}

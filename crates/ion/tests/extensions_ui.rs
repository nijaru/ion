//! Extension UI contributions (DESIGN.md §24 + Phase G): typed push
//! events over custom notifications, dialogs over elicitation,
//! registered commands over custom requests, and peer-death state
//! clearing. All presentation-only; the store and lane stay untouched.

use std::sync::Arc;
use std::time::Duration;

use ion_core::{
    DialogPropertyKind, ExtensionDef, ExtensionService, ExtensionUiEvent, ExtensionUiUpdate,
    ToolCatalog,
};

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn ui_extension_def(name: &str) -> ExtensionDef {
    ExtensionDef {
        name: name.to_owned(),
        command: "python3".to_owned(),
        args: vec![fixture("ui_extension.py")],
    }
}

/// Two copies of the same extension both register `/greet`; the second
/// must be suffixed (`uikit:greet`-style) so both stay invocable, pi's
/// `/review:1` / `/review:2` semantics.
#[tokio::test]
async fn extension_pushes_status_and_widget() {
    let catalog = ToolCatalog::default();
    let service = ExtensionService::new();
    let mut events = service.ui_hub().subscribe();
    service
        .start_into(&[ui_extension_def("uikit")], &catalog)
        .await;

    // The initialize response carries status + widget pushes; drain
    // until both arrive (handshake happens before start_into returns).
    let mut status = None;
    let mut widget = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && (status.is_none() || widget.is_none()) {
        let Ok(Ok(ExtensionUiEvent::Update { extension, update })) =
            tokio::time::timeout(Duration::from_secs(1), events.recv()).await
        else {
            break;
        };
        assert_eq!(extension, "uikit");
        match update {
            ExtensionUiUpdate::Status { key, text } => {
                assert_eq!(key, "uikit");
                status = text;
            }
            ExtensionUiUpdate::Widget { key, lines, below } => {
                assert_eq!(key, "hint");
                assert!(!below, "default placement is above the editor");
                widget = lines;
            }
            _ => {}
        }
    }
    assert_eq!(status.as_deref(), Some("ready"), "status push missing");
    assert_eq!(
        widget.as_deref(),
        Some(
            &[
                "uikit widget line 1".to_owned(),
                "uikit widget line 2".to_owned()
            ][..]
        ),
        "widget push missing"
    );
}

#[tokio::test]
async fn extension_registers_and_runs_a_command_with_a_dialog() {
    let catalog = ToolCatalog::default();
    let service = ExtensionService::new();
    let mut events = service.ui_hub().subscribe();
    service
        .start_into(&[ui_extension_def("uikit")], &catalog)
        .await;

    // Discovery completes with the tools handshake.
    let mut commands = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        commands = service.commands();
        if !commands.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let command = commands
        .iter()
        .find(|command| command.name == "greet")
        .expect("greet command registered");
    assert_eq!(command.extension, "uikit");
    assert_eq!(command.description, "Greet someone");

    // Invoking greet elicits a name; answer it and the command result
    // carries the greeting message.
    let service_arc = Arc::new(service);
    let invoke = {
        let service = Arc::clone(&service_arc);
        tokio::spawn(async move { service.run_command("greet", "").await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut dialog_respond = None;
    while tokio::time::Instant::now() < deadline && dialog_respond.is_none() {
        if let Ok(Ok(ExtensionUiEvent::Dialog {
            extension,
            dialog,
            respond,
        })) = tokio::time::timeout(Duration::from_millis(250), events.recv()).await
        {
            assert_eq!(extension, "uikit");
            assert_eq!(dialog.message, "Who should I greet?");
            let property = dialog
                .properties
                .iter()
                .find(|property| property.name == "name")
                .expect("name property");
            assert!(property.required);
            assert!(
                matches!(&property.kind, DialogPropertyKind::Text { .. }),
                "string property decoded as text"
            );
            dialog_respond = Some(respond);
        }
    }
    let respond = dialog_respond.expect("the command must have parked a dialog for the user");
    let mut values = std::collections::BTreeMap::new();
    values.insert("name".to_owned(), serde_json::json!("Ada"));
    let answered = ExtensionUiEvent::answer_dialog(
        &respond,
        ion_core::DialogAnswer {
            values,
            declined: false,
        },
    )
    .await;
    assert!(answered, "first answer must win the round trip");

    let result = tokio::time::timeout(Duration::from_secs(5), invoke)
        .await
        .expect("command completes")
        .expect("command ok");
    assert_eq!(result, Ok(Some("hello Ada (from uikit)".to_owned())));

    // The answered dialog cannot be re-answered.
    let mut values = std::collections::BTreeMap::new();
    values.insert("name".to_owned(), serde_json::json!("Eve"));
    let answered_again = ExtensionUiEvent::answer_dialog(
        &respond,
        ion_core::DialogAnswer {
            values,
            declined: false,
        },
    )
    .await;
    assert!(!answered_again, "second answer must be a no-op");
}

#[tokio::test]
async fn command_name_collisions_get_numeric_suffixes_in_discovery_order() {
    let catalog = ToolCatalog::default();
    let service = ExtensionService::new();
    service
        .start_into(
            &[
                ui_extension_def("uikit"),
                ExtensionDef {
                    name: "uikit2".to_owned(),
                    command: "python3".to_owned(),
                    args: vec![fixture("ui_extension.py")],
                },
            ],
            &catalog,
        )
        .await;

    let mut commands = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        commands = service.commands();
        if commands.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["greet:1", "greet:2"],
        "load-order suffixes: {names:?}"
    );
}

#[tokio::test]
async fn peer_death_clears_ui_state() {
    let catalog = ToolCatalog::default();
    let service = ExtensionService::new();
    let mut events = service.ui_hub().subscribe();
    service
        .start_into(&[ui_extension_def("uikit")], &catalog)
        .await;

    // Crash the peer through its tool (crash: true).
    let crash = catalog
        .execute(
            "uikit__ping",
            &serde_json::json!({ "crash": true }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(crash.is_error);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ExtensionUiEvent::PeerDown { extension })) =
            tokio::time::timeout(Duration::from_millis(250), events.recv()).await
        {
            assert_eq!(extension, "uikit");
            return;
        }
    }
    panic!("peer death must publish PeerDown so frontends drop its UI state");
}

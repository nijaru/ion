use super::*;
use crate::terminal_client::Editor;
use crate::terminal_client::{Action, tests::Fixture};
use ion_host::{
    McpConfig, McpExposure, McpServer, McpStdioServer, McpToolCounts, McpTools, SessionSetup,
};
use ion_terminal::{KeyCode, KeyEvent, Modifiers};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[test]
fn inspection_is_the_safe_default_and_actions_require_visible_confirmation() {
    for status in [
        McpServerStatus::Available,
        McpServerStatus::Unavailable,
        McpServerStatus::Reconnecting,
        McpServerStatus::CleanupFailed,
        McpServerStatus::Closing,
    ] {
        let server = McpServerSnapshot {
            name: "peer".into(),
            status,
            tools: McpToolCounts {
                direct: 1,
                deferred: 2,
                hidden: 3,
            },
            resources: true,
        };
        let picker = actions(&server);
        assert_eq!(
            picker
                .items
                .iter()
                .any(|item| matches!(item.value, PickerValue::Mcp(McpChoice::Reconnect(_)))),
            matches!(
                status,
                McpServerStatus::Available | McpServerStatus::Unavailable
            )
        );
        let mut ui = Frontend {
            editor: Editor {
                draft: "保留_🦀".into(),
                cursor: 3,
                ..Editor::default()
            },
            ..Frontend::default()
        };
        ui.pickers.open(picker);
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::None
        ));
        ui.pickers.painted_rows(100, 20, ui.themes.theme());
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::Pick(PickerValue::Mcp(McpChoice::Details(name))) if name == "peer")
        );
        assert_eq!((ui.editor.draft.as_str(), ui.editor.cursor), ("保留_🦀", 3));
    }
}

#[tokio::test]
async fn manager_inspection_is_passive_and_removed_actions_cannot_retarget() {
    let mut fixture = Fixture::new();
    let config = McpConfig::new(fixture.host.config_root());
    config
        .add(
            "offline",
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                description: None,
                cwd: None,
                env: Default::default(),
                command: fixture
                    .root
                    .join("missing-peer")
                    .to_string_lossy()
                    .into_owned(),
                args: Vec::new(),
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &fixture.root, CancellationToken::new()).await;
    assert!(!startup.diagnostics.is_empty());
    let tools = startup.tools.unwrap();
    let model = fixture.control.agent().model().clone();
    fixture.control = SessionSetup::new(
        fixture.control.session().clone(),
        fixture
            .host
            .sessions(Some(fixture.control.session().cwd().unwrap().to_owned())),
        Arc::new(
            ion_host::CodingApplication::new(fixture.host.clone(), None)
                .with_mcp_tools(tools.clone()),
        ),
        Some(&model),
        None,
        None,
    )
    .unwrap()
    .into_control();
    let before = fixture.control.session().view().unwrap().entries;
    let mut ui = Frontend {
        editor: Editor {
            draft: "preserve".into(),
            cursor: 4,
            ..Editor::default()
        },
        ..Frontend::default()
    };
    command(
        &mut fixture.control,
        &mut ui,
        "",
        EditorDraft::at_end("/mcp".into()),
    );
    assert_eq!(ui.pickers.active().unwrap().items.len(), 1);
    apply(
        &mut fixture.control,
        &mut ui,
        McpChoice::Server {
            name: "offline".into(),
            reconnect: false,
        },
    )
    .unwrap();
    let mut picker = ui.pickers.take_active().unwrap();
    picker.query = "reconnect".into();
    ui.pickers.open(picker);
    ui.pickers.painted_rows(100, 20, ui.themes.theme());
    assert!(
        matches!(ui.pickers.selected_value(), Some(PickerValue::Mcp(McpChoice::Reconnect(name))) if name == "offline")
    );
    tools.shutdown().await.unwrap();
    refresh(&fixture.control, &mut ui);
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
        Action::None
    ));
    ui.pickers.painted_rows(100, 20, ui.themes.theme());
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
        Action::Pick(PickerValue::Mcp(McpChoice::Details(name))) if name == "offline")
    );
    assert!(!fixture.control.is_busy());
    assert!(fixture.control.state().mcp_reconnect.is_none());
    assert_eq!(fixture.control.session().view().unwrap().entries, before);
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("preserve", 4)
    );
    fixture.control.shutdown().await;
}

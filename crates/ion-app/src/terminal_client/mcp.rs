//! Human MCP inspection and explicit actions over shared Host control.
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

use anyhow::{Context, Result};
use ion_host::{McpServerSnapshot, McpServerStatus, SessionControl};

use super::{EditorDraft, Frontend, Picker, PickerValue, picker::PickerItem};

#[derive(Clone, PartialEq, Eq)]
pub(super) enum McpChoice {
    Server { name: String, reconnect: bool },
    Details(String),
    Reconnect(String),
    List,
}

fn servers(control: &SessionControl<EditorDraft>) -> Result<Vec<McpServerSnapshot>> {
    control
        .mcp_servers()
        .context("This application has no admitted MCP management service")
}

fn status(value: McpServerStatus) -> &'static str {
    match value {
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Reconnecting => "reconnecting",
        McpServerStatus::CleanupFailed => "cleanup failed; replacement blocked",
        McpServerStatus::Closing => "closing; cleanup may still be running",
    }
}

fn details(server: &McpServerSnapshot) -> String {
    format!(
        "MCP server {}\nState: {}\nPublished tools: {} direct · {} deferred · {} hidden\nResource access: {}\nCaptured availability, not a remote health check. Counts precede application loadout selection. Configuration and trust are not reloaded.",
        server.name,
        status(server.status),
        server.tools.direct,
        server.tools.deferred,
        server.tools.hidden,
        if server.resources {
            "advertised"
        } else {
            "not available"
        }
    )
}

fn list(control: &SessionControl<EditorDraft>, reconnect: bool) -> Result<Picker> {
    let items = servers(control)?
        .into_iter()
        .map(|server| PickerItem {
            label: format!(
                "{} · {} · {} direct / {} deferred / {} hidden",
                server.name,
                status(server.status),
                server.tools.direct,
                server.tools.deferred,
                server.tools.hidden
            ),
            detail: Some(details(&server)),
            value: PickerValue::Mcp(McpChoice::Server {
                name: server.name,
                reconnect,
            }),
        })
        .collect();
    Ok(Picker {
        title: if reconnect {
            "Reconnect MCP server"
        } else {
            "MCP servers"
        },
        query: String::new(),
        selected: 0,
        items,
    })
}

fn owned(control: &SessionControl<EditorDraft>, name: &str) -> Result<McpServerSnapshot> {
    servers(control)?
        .into_iter()
        .find(|server| server.name == name)
        .context("MCP server is no longer admitted")
}

fn actions(server: &McpServerSnapshot) -> Picker {
    let name = server.name.as_str();
    let mut items = vec![PickerItem {
        label: "Show connection details".into(),
        detail: Some(details(server)),
        value: PickerValue::Mcp(McpChoice::Details(name.into())),
    }];
    if matches!(
        server.status,
        McpServerStatus::Available | McpServerStatus::Unavailable
    ) {
        items.push(PickerItem { label: "Reconnect shared server".into(),
            detail: Some("Stops and joins this connection's in-flight requests, including child requests, before replacement. Does not stop whole Turns or replay calls. Failure leaves this server unavailable.".into()),
            value: PickerValue::Mcp(McpChoice::Reconnect(name.into())) });
    }
    items.push(PickerItem {
        label: "Back to MCP servers".into(),
        detail: None,
        value: PickerValue::Mcp(McpChoice::List),
    });
    Picker {
        title: "MCP server actions",
        query: String::new(),
        selected: 0,
        items,
    }
}

fn reconnect(
    control: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    name: &str,
    source: EditorDraft,
) {
    match control.reconnect_mcp(name, source) {
        Ok(_) => ui.status = format!("Reconnecting MCP server {name}…"),
        Err((source, error)) => {
            ui.restore_editor(source);
            ui.status = format!("MCP reconnect was not started: {error:#}");
        }
    }
}

pub(super) fn command(
    control: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    args: &str,
    source: EditorDraft,
) {
    let mut parts = args.split_whitespace();
    let result = match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => list(control, false),
        (Some("reconnect"), None, None) => list(control, true),
        (Some("reconnect"), Some(name), None) => {
            reconnect(control, ui, name, source);
            return;
        }
        _ => Err(anyhow::anyhow!("Use /mcp [reconnect [SERVER]]")),
    };
    match result {
        Ok(picker) => ui.pickers.open(picker),
        Err(error) => {
            ui.restore_editor(source);
            ui.status = format!("{error:#}");
        }
    }
}

pub(super) fn apply(
    control: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    choice: McpChoice,
) -> Result<()> {
    match choice {
        McpChoice::Server {
            name,
            reconnect: true,
        }
        | McpChoice::Reconnect(name) => {
            let source = EditorDraft::at_end(format!("/mcp reconnect {name}"));
            reconnect(control, ui, &name, source);
        }
        McpChoice::Server {
            name,
            reconnect: false,
        } => ui.pickers.open(actions(&owned(control, &name)?)),
        McpChoice::Details(name) => {
            let server = owned(control, &name)?;
            ui.print(details(&server));
            ui.pickers.open(actions(&server));
        }
        McpChoice::List => ui.pickers.open(list(control, false)?),
    }
    Ok(())
}

/// Refresh only presentation, preserving target/action identity rather than row
/// indices. Removed reconnect actions fall back to inspection, never another effect.
pub(super) fn refresh(control: &SessionControl<EditorDraft>, ui: &mut Frontend) {
    let Some(previous) = ui.pickers.active() else {
        return;
    };
    let Some(PickerValue::Mcp(first)) = previous.items.first().map(|item| &item.value) else {
        return;
    };
    let result = match first {
        McpChoice::Server { reconnect, .. } => list(control, *reconnect),
        McpChoice::Details(name) => owned(control, name).map(|server| actions(&server)),
        _ => return,
    };
    let Ok(mut next) = result else {
        ui.pickers.close();
        ui.status = "MCP management view is no longer available".into();
        return;
    };
    let selected = previous
        .matches(false, None)
        .get(previous.selected)
        .and_then(|index| {
            if let PickerValue::Mcp(choice) = &previous.items[*index].value {
                Some(choice)
            } else {
                None
            }
        });
    next.query.clone_from(&previous.query);
    let matching = next.matches(false, None);
    next.selected = matching.iter().position(|index|
        matches!(&next.items[*index].value, PickerValue::Mcp(choice) if Some(choice) == selected)).unwrap_or(0);
    if matching.is_empty() {
        next.query.clear();
    }
    ui.pickers.open(next);
}

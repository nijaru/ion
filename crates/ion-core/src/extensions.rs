//! Subprocess extensions (DESIGN.md §24, Step 9).
//!
//! An extension is a subprocess publishing tools over the shared stdio
//! JSON-RPC client - language-neutral by construction: any runtime
//! that speaks the initialize/tools-list/tools-call shape works. Each
//! extension owns an [`ToolCatalog`] scope (`ext:<name>`); unloading
//! tears down the scope structurally (§24.4).
//!
//! Contributions start closed: tools only (§24.3). Commands, skills,
//! and hooks are future contribution types with their own semantics.
//!
//! Trust (§24.5): executable configuration from the project directory
//! is only honored when the caller passes an explicit trust grant;
//! user-level configuration is trusted by being user-authored.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::rpc::{HANDSHAKE_TIMEOUT, PeerDef, StdioRpc};
use crate::tool::{CatalogService, Tool, ToolCatalog, ToolOutcome};
use rmcp::model::PrimitiveSchemaDefinition;

/// One configured extension (settings or trusted project manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Starts extension subprocesses and publishes their tool and UI
/// contributions. The [`ToolCatalog`] owns the spawned supervisors and
/// drains them when its lifetime closes (the supervisor role in the
/// lifecycle hierarchy, §25.1).
///
/// The UI side is presentation-only: a hub fans typed events to
/// frontends, and a command registry routes `/name args` back to the
/// owning peer. Neither touches the store or the lane.
#[derive(Default, Clone)]
pub struct ExtensionService {
    hub: ExtensionUiHub,
    commands: std::sync::Arc<std::sync::RwLock<Vec<ExtensionCommand>>>,
    peers: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<StdioRpc>>>,
    >,
}

impl ExtensionService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Typed UI event stream for frontends (status/widget/footer pushes,
    /// dialogs, peer death).
    #[must_use]
    pub fn ui_hub(&self) -> ExtensionUiHub {
        self.hub.clone()
    }

    /// Commands registered so far, in discovery order with collision
    /// suffixes applied (pi `/review:1` / `/review:2` semantics).
    #[must_use]
    pub fn commands(&self) -> Vec<ExtensionCommand> {
        self.commands
            .read()
            .expect("extension command registry poisoned")
            .clone()
    }

    /// Start `defs` and register their tools under `ext:<name>` scopes.
    /// A failing extension logs a warning and is skipped: one broken
    /// extension never blocks startup.
    pub async fn start_into(&self, defs: &[ExtensionDef], catalog: &ToolCatalog) {
        for def in defs {
            let (ready_tx, ready_rx) = oneshot::channel();
            let def = def.clone();
            let service = catalog.service_handle();
            let name = def.name.clone();
            // The configured extension owns this structural identity before
            // its first successful tools/list. Live generations may come and
            // go without changing the lane's admitted scope.
            let scope = format!("ext:{name}");
            service.declare_scope(scope.clone());
            let peer_service = service.clone();
            let hub = self.hub.clone();
            let commands = std::sync::Arc::clone(&self.commands);
            let peers = std::sync::Arc::clone(&self.peers);
            let spawned = service.spawn(async move {
                supervise_extension_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    scope,
                    peer_service,
                    Some(ready_tx),
                    hub,
                    commands,
                    peers,
                )
                .await;
            });
            if !spawned {
                continue;
            }
            // Wait only for the first discovery attempt. Later retries are
            // owned by the service task and do not block other extensions.
            let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, ready_rx).await;
        }
    }
}

/// An extension tool through the ordinary [`Tool`] contract (§24.2):
/// policy, cancellation, and events behave exactly as for native and
/// MCP tools. A dead process yields a typed crash error naming the
/// extension; the runtime survives it.
struct ExtensionTool {
    connection: Arc<StdioRpc>,
    exposed_name: String,
    remote_name: String,
    spec: crate::tool::ToolSpec,
    extension_name: String,
}

impl Tool for ExtensionTool {
    fn spec(&self) -> crate::tool::ToolSpec {
        let mut spec = self.spec.clone();
        spec.name = self.exposed_name.clone();
        spec
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let call = self.connection.call_tool(&self.remote_name, arguments);
            tokio::select! {
                result = call => match result {
                    Ok(text) => ToolOutcome::text(text),
                    // Typed failure: the extension's own error vs. a
                    // dead process are distinguishable to the model.
                    Err(err)
                        if err.contains("server closed") || err.contains("Transport closed") =>
                    ToolOutcome::error(format!(
                        "extension `{}` crashed",
                        self.extension_name
                    )),
                    Err(err) => ToolOutcome::error(err),
                },
                () = cancel.cancelled() => ToolOutcome::error("cancelled"),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Extension UI contributions (pi `ctx.ui` parity over the wire).
//
// Pi's extensions run in-process and mutate the TUI through closures.
// Ion extensions are language-neutral subprocesses, so every ctx.ui
// surface maps onto the negotiated MCP transport instead:
//
//   setStatus/setWidget/setFooter → peer CustomNotification `ion/ui/*`
//   select/confirm/input          → standard elicitation/create (form)
//   registerCommand               → host CustomRequest `ion/commands/*`
//
// State stays data-driven and typed end to end; no closure ever
// crosses the wire. All of it is presentation-only: nothing here
// touches the store, the lane, or projection.
// ---------------------------------------------------------------------------

/// Cap an extension widget at pi's MAX_WIDGET_LINES so one peer cannot
/// flood the live band.
pub const MAX_EXTENSION_WIDGET_LINES: usize = 10;

/// Bound for one extension's `ion/commands/list` round trip. Peers
/// predating the command protocol silently ignore the method instead
/// of returning method-not-found, so a missing answer must mean "no
/// commands", never a stalled startup.
const COMMAND_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// One push-style UI update from an extension peer
/// (pi `ctx.ui.setStatus` / `setWidget` / `setFooter`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExtensionUiUpdate {
    /// Persistent footer status text under an owned key
    /// (pi `setStatus(key, text)`; `None` clears).
    Status { key: String, text: Option<String> },
    /// Text widget lines by key, above the composer by default
    /// (pi `setWidget(key, lines)`; `None` clears).
    Widget {
        key: String,
        lines: Option<Vec<String>>,
        below: bool,
    },
    /// Complete footer replacement (pi `setFooter`; `None` restores).
    Footer { text: Option<Vec<String>> },
}

/// A dialog request parked for the user (pi `ctx.ui.select` /
/// `confirm` / `input`). The form schema is the protocol's own
/// primitive-property object, which covers every pi dialog shape:
/// select = one enum property, confirm = one boolean, input = one
/// string.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionDialog {
    /// Peer that asked; labels the prompt and scopes state reset.
    pub extension: String,
    pub message: String,
    /// One property per schema entry, in schema order; enum/boolean
    /// properties render as pickers, others as a single-line editor.
    pub properties: Vec<DialogProperty>,
}

/// One elicitation schema property, flattened for the reducer.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogProperty {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: DialogPropertyKind,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DialogPropertyKind {
    /// Fixed choice set (select).
    Choice(Vec<String>),
    Boolean {
        title_yes: Option<String>,
        title_no: Option<String>,
    },
    Text {
        placeholder: Option<String>,
        default: Option<String>,
    },
    Number {
        default: Option<f64>,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
}

impl DialogProperty {
    /// Flatten one primitive elicitation property from its wire form.
    /// The schema nests enum values differently per shape (untitled
    /// `enum`, titled `oneOf`/`anyOf`), so the decode serializes the
    /// rmcp schema to JSON once and reads it shape-generically instead
    /// of matching rmcp's nested schema enums: robust to schema
    /// variants, and it keeps the rmcp dependency surface small.
    pub(crate) fn from_wire(
        name: String,
        schema: &PrimitiveSchemaDefinition,
        required: bool,
    ) -> Option<Self> {
        let wire = serde_json::to_value(schema).ok()?;
        let Value::Object(map) = &wire else {
            return None;
        };
        let kind = if let Some(values) = enum_values_of(map) {
            DialogPropertyKind::Choice(values)
        } else {
            match map.get("type").and_then(Value::as_str) {
                Some("boolean") => DialogPropertyKind::Boolean {
                    title_yes: None,
                    title_no: None,
                },
                Some("number") | Some("integer") => DialogPropertyKind::Number {
                    default: map.get("default").and_then(Value::as_f64),
                    minimum: map.get("minimum").and_then(Value::as_f64),
                    maximum: map.get("maximum").and_then(Value::as_f64),
                },
                // Spec-compliant peers always tag; an untagged property
                // is treated as free text rather than dropped so a
                // slightly-off peer still gets its dialog.
                _ => DialogPropertyKind::Text {
                    placeholder: map
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    default: map
                        .get("default")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            }
        };
        Some(Self {
            name,
            title: map.get("title").and_then(Value::as_str).map(str::to_owned),
            description: map
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            kind,
            required,
        })
    }
}

/// Enum values live under `enum` (untitled) or the `const` entries of
/// `oneOf`/`anyOf` (titled shapes).
fn enum_values_of(map: &serde_json::Map<String, Value>) -> Option<Vec<String>> {
    if let Some(Value::Array(values)) = map.get("enum") {
        let values = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        return Some(values);
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(entries)) = map.get(key) {
            let values = entries
                .iter()
                .filter_map(|entry| entry.get("const"))
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if !values.is_empty() {
                return Some(values);
            }
        }
    }
    None
}

/// The user's answer to a dialog, as elicitation content.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DialogAnswer {
    /// Property name → primitive value; absent optional properties were
    /// skipped by the user.
    pub values: std::collections::BTreeMap<String, serde_json::Value>,
    /// Esc pressed: the peer sees a declined dialog, not a cancelled
    /// operation (pi dialog semantics).
    pub declined: bool,
}

/// Everything an extension peer can contribute to the UI surface,
/// typed once at the wire boundary and broadcast to frontends.
#[derive(Debug, Clone)]
pub enum ExtensionUiEvent {
    /// A push update arrived (`ion/ui/*` custom notification).
    Update {
        extension: String,
        update: ExtensionUiUpdate,
    },
    /// A dialog needs an answer (elicitation/create). The shared
    /// responder closes the round trip; all copies dropped or unused
    /// answers Decline, so a lagging frontend cannot stall a peer.
    Dialog {
        extension: String,
        dialog: ExtensionDialog,
        respond:
            std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<DialogAnswer>>>>,
    },
    /// The peer left (crash, restart, shutdown): every UI contribution
    /// it owned disappears (pi `resetExtensionUI` semantics scoped to
    /// one extension).
    PeerDown { extension: String },
    /// The command registry changed (discovery finished or a
    /// re-discovery replaced a set); carries the full snapshot so
    /// frontends never reconstruct ordering themselves.
    Commands { commands: Vec<ExtensionCommand> },
}

impl ExtensionUiEvent {
    /// Answer a dialog round trip. The first answer wins; later or
    /// dropped responders are no-ops that report loss honestly.
    pub async fn answer_dialog(
        respond: &std::sync::Arc<
            tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<DialogAnswer>>>,
        >,
        answer: DialogAnswer,
    ) -> bool {
        let mut guard = respond.lock().await;
        match guard.take() {
            Some(sender) => sender.send(answer).is_ok(),
            None => false,
        }
    }
}

/// One command an extension registers (pi `registerCommand`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExtensionCommand {
    /// Invocation name without the leading slash, possibly suffixed by
    /// the hub when another extension claimed the same name first
    /// (pi `/review:1` / `/review:2`).
    pub name: String,
    pub description: String,
    /// Peer that owns the handler; routes `/name args` back.
    pub extension: String,
}

/// Materialized extension UI state (statuses, widgets, footer): the
/// hub updates it on every push so a frontend attaching after the
/// pushes — the TUI subscribes at `run()` while extensions push during
/// startup discovery — still seeds the current surface. Broadcast
/// never replays; the snapshot does.
#[derive(Debug, Default, Clone)]
pub struct ExtensionUiSnapshot {
    pub statuses: std::collections::BTreeMap<(String, String), String>,
    pub widgets: std::collections::BTreeMap<(String, String), (Vec<String>, bool)>,
    /// Owning extension plus its lines; the footer is one global slot
    /// (pi setFooter replaces, clearing restores).
    pub footer: Option<(String, Vec<String>)>,
}

impl ExtensionUiSnapshot {
    /// Replay the current surface as push events: a late frontend
    /// applies these and is in sync with live subscribers.
    #[must_use]
    pub fn updates(&self) -> Vec<ExtensionUiEvent> {
        let mut events = Vec::new();
        for ((extension, key), text) in &self.statuses {
            events.push(ExtensionUiEvent::Update {
                extension: extension.clone(),
                update: ExtensionUiUpdate::Status {
                    key: key.clone(),
                    text: Some(text.clone()),
                },
            });
        }
        for ((extension, key), (lines, below)) in &self.widgets {
            events.push(ExtensionUiEvent::Update {
                extension: extension.clone(),
                update: ExtensionUiUpdate::Widget {
                    key: key.clone(),
                    lines: Some(lines.clone()),
                    below: *below,
                },
            });
        }
        if let Some((extension, lines)) = &self.footer {
            events.push(ExtensionUiEvent::Update {
                extension: extension.clone(),
                update: ExtensionUiUpdate::Footer {
                    text: Some(lines.clone()),
                },
            });
        }
        events
    }
}

/// Host-side fan-out for extension UI events. Peers push through the
/// stdio handler; frontends subscribe. The hub never blocks on a
/// frontend: a full ring means that frontend lags, not that the
/// extension stalls.
#[derive(Clone)]
pub struct ExtensionUiHub {
    events: std::sync::Arc<tokio::sync::broadcast::Sender<ExtensionUiEvent>>,
    state: std::sync::Arc<std::sync::RwLock<ExtensionUiSnapshot>>,
}

impl Default for ExtensionUiHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionUiHub {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = tokio::sync::broadcast::channel(256);
        Self {
            events: std::sync::Arc::new(events),
            state: std::sync::Arc::new(std::sync::RwLock::new(ExtensionUiSnapshot::default())),
        }
    }

    /// Subscribe for rendering; lagging receivers get the standard
    /// broadcast `Lagged` error and resync from state.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ExtensionUiEvent> {
        self.events.subscribe()
    }

    /// The current materialized surface for late-attaching frontends.
    #[must_use]
    pub fn snapshot(&self) -> ExtensionUiSnapshot {
        self.state
            .read()
            .expect("extension UI snapshot poisoned")
            .clone()
    }

    pub(crate) fn publish(&self, event: ExtensionUiEvent) {
        // Materialize pushes into the snapshot first: it is the
        // authoritative replay surface for late frontends.
        {
            let mut state = self.state.write().expect("extension UI snapshot poisoned");
            match &event {
                ExtensionUiEvent::Update { extension, update } => match update {
                    ExtensionUiUpdate::Status { key, text } => match text {
                        Some(text) => {
                            state
                                .statuses
                                .insert((extension.clone(), key.clone()), text.clone());
                        }
                        None => {
                            state.statuses.remove(&(extension.clone(), key.clone()));
                        }
                    },
                    ExtensionUiUpdate::Widget { key, lines, below } => match lines {
                        Some(lines) => {
                            state
                                .widgets
                                .insert((extension.clone(), key.clone()), (lines.clone(), *below));
                        }
                        None => {
                            state.widgets.remove(&(extension.clone(), key.clone()));
                        }
                    },
                    ExtensionUiUpdate::Footer { text } => match text {
                        Some(lines) => {
                            state.footer = Some((extension.clone(), lines.clone()));
                        }
                        None => {
                            state.footer = None;
                        }
                    },
                },
                ExtensionUiEvent::PeerDown { extension } => {
                    state.statuses.retain(|(owner, _), _| owner != extension);
                    state.widgets.retain(|(owner, _), _| owner != extension);
                    if state
                        .footer
                        .as_ref()
                        .is_some_and(|(owner, _)| owner == extension)
                    {
                        state.footer = None;
                    }
                }
                ExtensionUiEvent::Dialog { .. } | ExtensionUiEvent::Commands { .. } => {}
            }
        }
        let _ = self.events.send(event);
    }
}

// ---------------------------------------------------------------------------
// Wire handler: the extension-side client of the MCP connection.
// ---------------------------------------------------------------------------

/// One `ion/ui/*` notification decoded, or the reason it was rejected.
enum UiWire {
    Update(ExtensionUiUpdate),
    Malformed(&'static str),
}

/// Decode `ion/ui/status`, `ion/ui/widget`, `ion/ui/footer` custom
/// notifications. Malformed payloads are dropped with a logged reason,
/// never propagated as protocol errors: a broken extension must not
/// disrupt its own tool traffic.
fn decode_ui_notification(method: &str, params: Option<&Value>) -> UiWire {
    let Some(params) = params else {
        return UiWire::Malformed("missing params");
    };
    let Some(object) = params.as_object() else {
        return UiWire::Malformed("params not an object");
    };
    let key = || object.get("key").and_then(Value::as_str).map(str::to_owned);
    match method {
        "ion/ui/status" => {
            let Some(key) = key() else {
                return UiWire::Malformed("status missing key");
            };
            let text = match object.get("text") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => Some(text.clone()),
                Some(_) => return UiWire::Malformed("status text not a string"),
            };
            UiWire::Update(ExtensionUiUpdate::Status { key, text })
        }
        "ion/ui/widget" => {
            let Some(key) = key() else {
                return UiWire::Malformed("widget missing key");
            };
            let lines = match object.get("lines") {
                None | Some(Value::Null) => None,
                Some(Value::Array(lines)) => {
                    let mut texts = Vec::with_capacity(lines.len());
                    for line in lines {
                        let Some(text) = line.as_str() else {
                            return UiWire::Malformed("widget line not a string");
                        };
                        texts.push(text.to_owned());
                    }
                    Some(texts)
                }
                Some(_) => return UiWire::Malformed("widget lines not an array"),
            };
            let below = matches!(object.get("placement"), Some(Value::String(placement)) if placement == "belowEditor");
            UiWire::Update(ExtensionUiUpdate::Widget { key, lines, below })
        }
        "ion/ui/footer" => {
            let lines = match object.get("lines") {
                None | Some(Value::Null) => None,
                Some(Value::Array(lines)) => {
                    let mut texts = Vec::with_capacity(lines.len());
                    for line in lines {
                        let Some(text) = line.as_str() else {
                            return UiWire::Malformed("footer line not a string");
                        };
                        texts.push(text.to_owned());
                    }
                    Some(texts)
                }
                Some(_) => return UiWire::Malformed("footer lines not an array"),
            };
            UiWire::Update(ExtensionUiUpdate::Footer { text: lines })
        }
        _ => UiWire::Malformed("unknown method"),
    }
}

/// The extension process seen from the host: parses UI pushes, parks
/// dialogs in the hub, and answers elicitation rounds. One handler
/// instance owns one live peer connection (a restart builds a new one
/// under the same extension name).
pub(crate) struct ExtensionUiHandler {
    hub: ExtensionUiHub,
    extension: String,
    info: rmcp::model::ClientInfo,
}

impl ExtensionUiHandler {
    pub(crate) fn new(hub: ExtensionUiHub, extension: String) -> Self {
        Self {
            hub,
            extension,
            info: crate::rpc::client_info(),
        }
    }
}

impl rmcp::ClientHandler for ExtensionUiHandler {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        self.info.clone()
    }

    fn on_custom_notification(
        &self,
        notification: rmcp::model::CustomNotification,
        _context: rmcp::service::NotificationContext<rmcp::service::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let event = match decode_ui_notification(&notification.method, notification.params.as_ref())
        {
            UiWire::Update(update) => Some(ExtensionUiEvent::Update {
                extension: self.extension.clone(),
                update,
            }),
            UiWire::Malformed(reason) => {
                tracing::warn!(
                    extension = %self.extension,
                    method = %notification.method,
                    reason,
                    "dropping malformed extension UI notification"
                );
                None
            }
        };
        async move {
            if let Some(event) = event {
                self.hub.publish(event);
            }
        }
    }

    fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleClient>,
    ) -> impl Future<Output = Result<rmcp::model::ElicitResult, rmcp::ErrorData>> + Send + '_ {
        use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
        use std::collections::BTreeSet;

        // Flatten the form schema into typed properties. Url-mode
        // requests are declined: Ion's terminal cannot complete them.
        let dialog = match &request {
            ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => {
                let required: BTreeSet<String> = requested_schema
                    .required
                    .iter()
                    .flatten()
                    .cloned()
                    .collect();
                let mut properties = Vec::new();
                let names: Vec<String> = requested_schema
                    .property_order
                    .clone()
                    .unwrap_or_else(|| requested_schema.properties.keys().cloned().collect());
                for name in names {
                    let Some(schema) = requested_schema.properties.get(&name) else {
                        continue;
                    };
                    match DialogProperty::from_wire(name.clone(), schema, required.contains(&name))
                    {
                        Some(property) => properties.push(property),
                        None => {
                            tracing::warn!(
                                extension = %self.extension,
                                property = %name,
                                "skipping undecodable dialog property"
                            );
                        }
                    }
                }
                if properties.is_empty() {
                    None
                } else {
                    Some(ExtensionDialog {
                        extension: self.extension.clone(),
                        message: message.clone(),
                        properties,
                    })
                }
            }
            ElicitRequestParams::UrlElicitationParams { .. } => None,
            _ => None,
        };

        let hub = self.hub.clone();
        let extension = self.extension.clone();
        async move {
            let Some(dialog) = dialog else {
                return Err(rmcp::ErrorData::invalid_params(
                    "unsupported elicitation shape",
                    None,
                ));
            };
            let (tx, rx) = tokio::sync::oneshot::channel::<DialogAnswer>();
            let respond = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));
            hub.publish(ExtensionUiEvent::Dialog {
                extension: extension.clone(),
                dialog,
                respond,
            });
            // The peer asked for user input; waiting for the user is the
            // point. No timeout: the user answers when they answer, and
            // the peer cancels through MCP cancellation if it must.
            match rx.await {
                Ok(answer) if answer.declined => Ok(ElicitResult::new(ElicitationAction::Decline)),
                Ok(answer) => Ok(ElicitResult::new(ElicitationAction::Accept)
                    .with_content(serde_json::to_value(&answer.values).unwrap_or(Value::Null))),
                Err(_) => {
                    // Frontend went away (shutdown, session switch): a
                    // dropped dialog is a decline, never a stall.
                    Ok(ElicitResult::new(ElicitationAction::Decline))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Extension supervision: tool peers plus the UI channel.
// ---------------------------------------------------------------------------

/// Extension-specific supervision on top of the shared tool-peer
/// loop shape: the peer is spawned with the UI handler, commands are
/// discovered once per live generation, peer death clears its UI state
/// (pi resetExtensionUI scoped to one extension), and the live
/// connection is tracked for `/command` routing.
pub(crate) async fn supervise_extension_peer(
    def: PeerDef,
    scope: String,
    service: CatalogService,
    mut ready: Option<oneshot::Sender<()>>,
    hub: ExtensionUiHub,
    commands: std::sync::Arc<std::sync::RwLock<Vec<ExtensionCommand>>>,
    peers: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<StdioRpc>>>>,
) {
    use crate::rpc::{HANDSHAKE_TIMEOUT, spawn_with_handler};

    let extension = def.name.clone();
    let mut failures = 0u32;
    loop {
        let Some(lifetime) = service.lifetime() else {
            return;
        };
        let (closed_tx, mut closed_rx) = tokio::sync::watch::channel(false);
        let callback_scope = scope.clone();
        let callback_service = service.clone();
        let peer_down_hub = hub.clone();
        let peer_down_name = extension.clone();
        let on_closed: crate::rpc::CloseHandler = Arc::new(move || {
            callback_service.remove_scope(&callback_scope);
            // The peer's UI contributions died with it; frontends drop
            // every status/widget/footer/dialog it owned.
            peer_down_hub.publish(ExtensionUiEvent::PeerDown {
                extension: peer_down_name.clone(),
            });
            let _ = closed_tx.send(true);
        });
        let handler = ExtensionUiHandler::new(hub.clone(), extension.clone());
        let rpc = match tokio::select! {
            () = lifetime.cancelled() => return,
            result = spawn_with_handler(&def, HANDSHAKE_TIMEOUT, on_closed, handler) => result,
        } {
            Ok(rpc) => Arc::new(rpc),
            Err(err) => {
                tracing::warn!(peer = %def.name, error = %err, "extension failed to start");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                if !crate::rpc::schedule_restart(&def.name, &lifetime, &mut failures, "extension")
                    .await
                {
                    return;
                }
                continue;
            }
        };

        let tools = match tokio::select! {
            () = lifetime.cancelled() => {
                rpc.close().await;
                return;
            }
            result = tokio::time::timeout(HANDSHAKE_TIMEOUT, rpc.list_tools()) => result,
        } {
            Ok(Ok(tools)) => tools,
            Ok(Err(err)) => {
                tracing::warn!(peer = %def.name, error = %err, "extension tools/list failed");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                rpc.close().await;
                if !crate::rpc::schedule_restart(&def.name, &lifetime, &mut failures, "extension")
                    .await
                {
                    return;
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(peer = %def.name, "extension tools/list timed out");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                rpc.close().await;
                if !crate::rpc::schedule_restart(&def.name, &lifetime, &mut failures, "extension")
                    .await
                {
                    return;
                }
                continue;
            }
        };

        // Discovery finished: tools register under the admitted scope,
        // commands register globally with collision suffixes, and the
        // live connection becomes invocable. A generation restarting
        // replaces its peer entry; commands are idempotent by
        // (extension, base name) so a re-discovery after restart does
        // not double-register.
        let scoped: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .map(|spec| {
                Arc::new(ExtensionTool {
                    connection: Arc::clone(&rpc),
                    exposed_name: format!("{extension}__{}", spec.name),
                    remote_name: spec.name.clone(),
                    spec,
                    extension_name: extension.clone(),
                }) as Arc<dyn Tool>
            })
            .collect();
        peers
            .lock()
            .expect("extension peer registry poisoned")
            .insert(extension.clone(), Arc::clone(&rpc));
        tracing::info!(peer = %def.name, tools = scoped.len(), "extension ready");
        service.register_scope(scope.clone(), scoped);
        // Startup never blocks on command discovery: a peer that does
        // not implement `ion/commands/list` — or answers slowly —
        // contributes no commands rather than stalling the handshake
        // window (pre-existing tool peers are the common case).
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }
        if let Some(discovered) = tokio::time::timeout(
            COMMAND_DISCOVERY_TIMEOUT,
            discover_commands(&rpc, &extension),
        )
        .await
        .ok()
        .flatten()
        {
            register_discovered_commands(&commands, extension.clone(), discovered);
            hub.publish(ExtensionUiEvent::Commands {
                commands: commands
                    .read()
                    .expect("extension command registry poisoned")
                    .clone(),
            });
        }
        if rpc.is_closed() {
            service.remove_scope(&scope);
            peers
                .lock()
                .expect("extension peer registry poisoned")
                .remove(&extension);
        } else {
            tokio::select! {
                result = closed_rx.changed() => {
                    if result.is_err() {
                        service.remove_scope(&scope);
                    }
                    peers
                        .lock()
                        .expect("extension peer registry poisoned")
                        .remove(&extension);
                }
                () = lifetime.cancelled() => {
                    service.remove_scope(&scope);
                    peers
                        .lock()
                        .expect("extension peer registry poisoned")
                        .remove(&extension);
                }
            }
        }
        rpc.close().await;
        if lifetime.is_cancelled() {
            return;
        }
        if !crate::rpc::schedule_restart(&def.name, &lifetime, &mut failures, "extension").await {
            return;
        }
    }
}

/// Merge one extension's discovered commands into the global
/// registry (pi `registerCommand` collision semantics): a re-discovery
/// after restart replaces the extension's previous set; a name
/// registered by several extensions gets numeric suffixes `:1`, `:2`
/// in discovery order for every copy.
fn register_discovered_commands(
    commands: &std::sync::Arc<std::sync::RwLock<Vec<ExtensionCommand>>>,
    extension: String,
    discovered: Vec<ExtensionCommand>,
) {
    let mut registry = commands
        .write()
        .expect("extension command registry poisoned");
    registry.retain(|command| command.extension != extension);
    for command in discovered {
        let collisions = registry
            .iter()
            .filter(|existing| existing.name == command.name)
            .count();
        let name = if collisions > 0 {
            // Pi collision semantics: every same-named copy gets a
            // 1-based occurrence suffix in discovery order, including
            // the copies already registered.
            let mut index = 1;
            for existing in registry
                .iter_mut()
                .filter(|existing| existing.name == command.name)
            {
                existing.name = format!("{}:{index}", command.name);
                index += 1;
            }
            format!("{}:{index}", command.name)
        } else {
            command.name
        };
        registry.push(ExtensionCommand {
            name,
            description: command.description,
            extension: command.extension,
        });
    }
}

/// Ask one live peer for its commands (`ion/commands/list`). A peer
/// that does not implement the method contributes no commands; the
/// pre-existing tool transport keeps working unchanged.
async fn discover_commands(rpc: &StdioRpc, extension: &str) -> Option<Vec<ExtensionCommand>> {
    use rmcp::model::{ClientRequest, CustomRequest};
    let request = ClientRequest::CustomRequest(CustomRequest::new("ion/commands/list", None));
    let response = rpc.send_raw(request).await.ok()?;
    let Value::Array(entries) = &response else {
        tracing::warn!(extension, "ion/commands/list returned non-array");
        return None;
    };
    let mut commands = Vec::with_capacity(entries.len());
    for entry in entries {
        let Value::Object(map) = entry else {
            continue;
        };
        let Some(Value::String(name)) = map.get("name") else {
            continue;
        };
        let description = match map.get("description") {
            Some(Value::String(text)) => text.clone(),
            _ => String::new(),
        };
        commands.push(ExtensionCommand {
            name: name.clone(),
            description,
            extension: extension.to_owned(),
        });
    }
    Some(commands)
}

impl ExtensionService {
    /// Invoke one extension command (`ion/command/run`). The peer's
    /// reply may carry a `message` string, which the caller submits to
    /// the session as the command's outcome (pi registerCommand
    /// handlers own their LLM interaction; the plain-message return is
    /// the subprocess equivalent of pi.sendMessage from a handler).
    pub async fn run_command(&self, name: &str, args: &str) -> Result<Option<String>, String> {
        let extension = {
            let registry = self
                .commands
                .read()
                .expect("extension command registry poisoned");
            let command = registry
                .iter()
                .find(|command| command.name == name)
                .ok_or_else(|| format!("unknown extension command: /{name}"))?;
            command.extension.clone()
        };
        let peer = {
            let peers = self.peers.lock().expect("extension peer registry poisoned");
            peers
                .get(&extension)
                .cloned()
                .ok_or_else(|| format!("extension `{extension}` is not connected"))?
        };
        use rmcp::model::{ClientRequest, CustomRequest};
        let params = serde_json::json!({ "command": name, "args": args });
        let request =
            ClientRequest::CustomRequest(CustomRequest::new("ion/command/run", Some(params)));
        let response = peer
            .send_raw(request)
            .await
            .map_err(|err| format!("extension `{extension}` command failed: {err}"))?;
        let message = response
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(message)
    }
}

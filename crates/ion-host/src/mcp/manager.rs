//! Admitted definitions, frozen generations and retained reconnect custody.
use std::{
    path::Path,
    sync::{Arc, RwLock as SyncRwLock, atomic::Ordering},
};

use anyhow::{Context, Result, anyhow, ensure};
use futures_util::future::join_all;
use ion_ai::BoxFuture;
use ion_core::{
    CodingToolSource, ToolActivityKind, ToolDefinition, ToolExposure, ToolPresentation,
    ToolRegistration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{
    McpConfig, McpExposure, McpServer, Server,
    catalog::{Catalog, fetch_tools, publish_tools, register_tools},
    connection::{self, combine_cleanup},
    conversion,
    invocation::McpExecutor,
    resource_tools,
};

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

pub struct McpTools {
    definitions: std::collections::BTreeMap<String, McpServer>,
    cwd: std::path::PathBuf,
    state: SyncRwLock<ManagerState>,
    catalog: Arc<SyncRwLock<Catalog>>,
    refresh_gate: Mutex<()>,
    closing: CancellationToken,
}
#[derive(Default)]
struct ManagerState {
    servers: Vec<Arc<Server>>,
    reconnects: std::collections::BTreeMap<String, Arc<ReconnectWork>>,
    cleanup_failures: std::collections::BTreeMap<String, String>,
}
impl ManagerState {
    fn retain_failure(&mut self, name: &str, failure: &ReconnectFailure) {
        if failure.cleanup_failed {
            self.cleanup_failures
                .entry(name.to_owned())
                .or_insert_with(|| failure.message.clone());
        }
    }
}
#[derive(Default)]
pub struct McpStartup {
    /// Admitted definitions remain manageable even if no initial peer connects.
    /// Availability comes from registrations, not the presence of this service.
    pub tools: Option<Arc<McpTools>>,
    pub diagnostics: Vec<String>,
}
/// Discovery warnings do not change successful replacement availability.
#[derive(Clone, Debug, serde::Serialize)]
pub struct McpReconnectReport {
    pub diagnostics: Vec<String>,
}
/// Retained reconnect custody. Drop requests cancellation; explicit joining
/// retrieves its outcome. The manager also joins this work on controlled exit.
pub struct McpReconnect {
    work: Arc<ReconnectWork>,
}
#[derive(Clone, Debug)]
struct ReconnectFailure {
    message: String,
    cleanup_failed: bool,
}
impl From<anyhow::Error> for ReconnectFailure {
    fn from(error: anyhow::Error) -> Self {
        Self {
            cleanup_failed: error.downcast_ref::<connection::CleanupFailure>().is_some(),
            message: format!("{error:#}"),
        }
    }
}
type ReconnectResult = std::result::Result<McpReconnectReport, ReconnectFailure>;
struct ReconnectJoin {
    task: tokio::task::JoinHandle<()>,
    result: Option<std::result::Result<(), String>>,
}
struct ReconnectWork {
    stop: CancellationToken,
    completion: Arc<SyncRwLock<Option<ReconnectResult>>>,
    join: Mutex<ReconnectJoin>,
}
impl ReconnectWork {
    async fn joined(&self) -> std::result::Result<ReconnectResult, String> {
        let mut join = self.join.lock().await;
        if join.result.is_none() {
            join.result = Some((&mut join.task).await.map_err(|error| {
                format!("MCP reconnect task failed; effects may be unknown: {error}")
            }));
        }
        join.result
            .as_ref()
            .expect("joined reconnect task")
            .clone()?;
        Ok(self
            .completion
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("completed reconnect has an outcome")
            .clone())
    }
}
impl McpReconnect {
    pub fn request_stop(&self) {
        self.work.stop.cancel();
    }
    pub fn is_stopped(&self) -> bool {
        self.work.stop.is_cancelled()
    }
    pub async fn join(&mut self) -> Result<McpReconnectReport> {
        self.work
            .joined()
            .await
            .map_err(anyhow::Error::msg)?
            .map_err(|error| anyhow::Error::msg(error.message))
    }
}
impl Drop for McpReconnect {
    fn drop(&mut self) {
        self.request_stop();
    }
}

impl McpTools {
    pub async fn connect(config: &McpConfig, cwd: &Path, stop: CancellationToken) -> McpStartup {
        let (saved, mut diagnostics) = match config.load_startup() {
            Ok(saved) => saved,
            Err(error) => {
                return McpStartup {
                    diagnostics: vec![format!("{error:#}")],
                    ..McpStartup::default()
                };
            }
        };
        // Capture admitted definitions even when their initial transport fails.
        // Explicit reconnect is not configuration or trust reload.
        let definitions: std::collections::BTreeMap<_, _> = saved
            .into_iter()
            .filter(|(_, definition)| definition.enabled())
            .collect();
        let catalog = Arc::new(SyncRwLock::new(Catalog::new()));
        let mut state = ManagerState::default();
        let attempts = definitions.iter().map(|(name, definition)| {
            let catalog = catalog.clone();
            let stop = stop.clone();
            async move {
                (
                    name,
                    Server::connect(name.clone(), definition.clone(), cwd, catalog, stop).await,
                )
            }
        });
        for (name, attempt) in join_all(attempts).await {
            let failure = match attempt {
                Ok((server, _)) if stop.is_cancelled() => Some(
                    combine_cleanup::<()>(
                        Err(anyhow!("MCP startup cancelled")),
                        server.shutdown().await,
                    )
                    .unwrap_err(),
                ),
                Ok((server, discovered)) => {
                    match publish_tools(&catalog, &server.name, discovered.tools) {
                        Ok(()) => {
                            diagnostics.extend(discovered.diagnostics);
                            if let Some(notice) = server
                                .stderr
                                .as_ref()
                                .and_then(|log| log.notice(&server.name))
                            {
                                diagnostics.push(notice);
                            }
                            state.servers.push(Arc::new(server));
                            None
                        }
                        Err(error) => Some(
                            combine_cleanup::<()>(Err(error), server.shutdown().await).unwrap_err(),
                        ),
                    }
                }
                Err(error) => Some(error),
            };
            if let Some(error) = failure {
                let failure = ReconnectFailure::from(error);
                state.retain_failure(name, &failure);
                diagnostics.push(failure.message);
            }
        }
        let cancelled = stop.is_cancelled();
        if cancelled {
            // Cancellation may arrive while closing another failed startup.
            // Join accumulated successes before discarding their manager.
            for server in &state.servers {
                server.retire();
            }
            for result in join_all(state.servers.iter().map(|server| server.shutdown())).await {
                if let Err(error) = result {
                    diagnostics.push(format!("{error:#}"));
                }
            }
            state.servers.clear();
        }
        let tools = (!definitions.is_empty() && !cancelled).then(|| {
            Arc::new(Self {
                definitions,
                cwd: cwd.to_owned(),
                state: SyncRwLock::new(state),
                catalog,
                refresh_gate: Mutex::new(()),
                closing: CancellationToken::new(),
            })
        });
        McpStartup { tools, diagnostics }
    }

    pub(super) fn servers(&self) -> Vec<Arc<Server>> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .servers
            .clone()
    }

    /// Admit an explicit replacement of a captured enabled/trusted definition.
    /// Values are captured afresh; simultaneous replacement of the same server
    /// is rejected before effects. The returned handle owns cancellation intent.
    pub fn reconnect(self: &Arc<Self>, name: &str) -> Result<McpReconnect> {
        self.reconnect_with_stop(name, self.closing.clone())
    }

    pub(crate) fn reconnect_with_stop(
        self: &Arc<Self>,
        name: &str,
        parent_stop: CancellationToken,
    ) -> Result<McpReconnect> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(!self.closing.is_cancelled(), "MCP manager is closed");
        ensure!(
            !state.cleanup_failures.contains_key(name),
            "MCP server {name} has an unresolved cleanup failure; replacement refused"
        );
        let definition = self
            .definitions
            .get(name)
            .with_context(|| format!("MCP server {name} was not admitted at startup"))?
            .clone();
        if let Some(work) = state.reconnects.get(name) {
            let completed = work
                .completion
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            completed.as_ref().with_context(|| {
                format!("MCP server {name} already has a reconnect in progress")
            })?;
        }
        let stop = parent_stop.child_token();
        let completion = Arc::new(SyncRwLock::new(None));
        let owner = self.clone();
        let server = name.to_owned();
        let task_stop = stop.clone();
        let task_completion = completion.clone();
        let task = tokio::spawn(async move {
            let result = owner
                .replace(&server, definition, task_stop)
                .await
                .map_err(ReconnectFailure::from);
            if let Err(failure) = &result {
                owner
                    .state
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain_failure(&server, failure);
            }
            *task_completion
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        });
        let work = Arc::new(ReconnectWork {
            stop,
            completion,
            join: Mutex::new(ReconnectJoin { task, result: None }),
        });
        state.reconnects.insert(name.to_owned(), work.clone());
        Ok(McpReconnect { work })
    }

    async fn replace(
        &self,
        name: &str,
        definition: McpServer,
        stop: CancellationToken,
    ) -> Result<McpReconnectReport> {
        ensure!(!stop.is_cancelled(), "MCP reconnect cancelled");
        let old = self
            .servers()
            .into_iter()
            .find(|server| server.name == name);
        if let Some(old) = old {
            // Retire before waiting for refresh so its request releases the old
            // connection lease. This does not stop any Session or whole Turn.
            old.retire();
            old.shutdown()
                .await
                .map_err(connection::mark_cleanup_failure)?;
        }
        let _gate = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(anyhow!("MCP reconnect cancelled")),
            gate = self.refresh_gate.lock() => gate,
        };
        let (server, discovered) = Server::connect(
            name.to_owned(),
            definition,
            &self.cwd,
            self.catalog.clone(),
            stop.clone(),
        )
        .await?;
        let server = Arc::new(server);
        let publication = {
            // Snapshots use the same lock order. Shutdown fencing and publication
            // of the generation and its definitions are one synchronous boundary.
            let mut state = self
                .state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if stop.is_cancelled() || self.closing.is_cancelled() {
                Err(anyhow!("MCP reconnect cancelled"))
            } else {
                publish_tools(&self.catalog, name, discovered.tools).map(|()| {
                    if let Some(previous) = state.servers.iter_mut().find(|old| old.name == name) {
                        *previous = server.clone();
                    } else {
                        state.servers.push(server.clone());
                        state.servers.sort_by(|a, b| a.name.cmp(&b.name));
                    }
                    let mut diagnostics = discovered.diagnostics;
                    if let Some(notice) = server.stderr.as_ref().and_then(|log| log.notice(name)) {
                        diagnostics.push(notice);
                    }
                    McpReconnectReport { diagnostics }
                })
            }
        };
        match publication {
            Ok(report) => Ok(report),
            Err(error) => combine_cleanup(Err(error), server.shutdown().await),
        }
    }

    async fn refresh_changed(&self, stop: CancellationToken) -> Vec<String> {
        let mut diagnostics = Vec::new();
        let _gate = tokio::select! {
            () = stop.cancelled() => return diagnostics,
            guard = self.refresh_gate.lock() => guard,
        };
        for server in self.servers() {
            let version = server.tool_list_version.load(Ordering::Acquire);
            if version == server.refreshed_version.load(Ordering::Acquire) {
                continue;
            }
            let client = tokio::select! {
                guard = server.client.read() => guard,
                () = stop.cancelled() => break,
            };
            let Some(client) = client.as_ref() else {
                diagnostics.push(format!("MCP server {} is closed", server.name));
                server.refreshed_version.store(version, Ordering::Release);
                continue;
            };
            let listed = tokio::select! {
                () = server.retiring.cancelled() => break,
                () = stop.cancelled() => break,
                result = fetch_tools(&server.name, &client.client) => result,
            };
            let updated = match listed {
                Ok(tools) => {
                    let name = server.name.clone();
                    // Once listing is observed, keep the compiler join and read
                    // lease outside cancellation races. Shutdown joins it too.
                    conversion::run(move || register_tools(&name, tools)).await
                }
                Err(error) => Err(error),
            };
            if stop.is_cancelled() || server.retiring.is_cancelled() {
                break;
            }
            match updated {
                Ok(updated) => match publish_tools(&self.catalog, &server.name, updated.tools) {
                    Ok(()) => diagnostics.extend(updated.diagnostics),
                    Err(error) => diagnostics.push(format!("{error:#}")),
                },
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
            // A failed listing keeps the previous snapshot and warns once.
            // A notification that arrived during this listing has a newer
            // version and will still be refreshed at the next boundary.
            server.refreshed_version.store(version, Ordering::Release);
        }
        diagnostics
    }

    /// Fence replacement publication, then join retained workers and connections.
    pub async fn shutdown(&self) -> Result<()> {
        let reconnects = {
            let state = self
                .state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.closing.cancel();
            for work in state.reconnects.values() {
                work.stop.cancel();
            }
            for server in &state.servers {
                server.retire();
            }
            state.reconnects.values().cloned().collect::<Vec<_>>()
        };
        let mut errors = Vec::new();
        for work in reconnects {
            // A failed/cancelled operation has its own retained outcome; joining
            // the task is separate from turning it into a shutdown failure.
            if let Err(error) = work.joined().await {
                errors.push(error);
            }
        }
        errors.extend(
            self.state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cleanup_failures
                .values()
                .cloned(),
        );
        let servers = self.servers();
        let results = join_all(servers.iter().map(|server| server.shutdown())).await;
        errors.extend(
            results
                .into_iter()
                .filter_map(Result::err)
                .map(|error| format!("{error:#}")),
        );
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}

impl CodingToolSource for McpTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        let mut registrations = Vec::new();
        // Snapshot the complete source under one guard, not independently
        // changing server projections. The same guard protects publication.
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let catalog = self
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for server in &state.servers {
            if server.retiring.is_cancelled() {
                continue;
            }
            let Some(tools) = catalog.get(&server.name) else {
                continue;
            };
            for tool in tools {
                let exposure = match server
                    .tool_exposure
                    .get(&tool.original_name)
                    .copied()
                    .or_else(|| {
                        server.tool_exposure.iter().find_map(|(pattern, exposure)| {
                            ion_core::tool_name_matches(pattern, &tool.original_name)
                                .then_some(*exposure)
                        })
                    })
                    .unwrap_or(server.exposure)
                {
                    McpExposure::Direct => ToolExposure::Direct,
                    McpExposure::Deferred => ToolExposure::Deferred,
                    McpExposure::Hidden => continue,
                };
                let mut spec = tool.spec.clone();
                if let Some(description) = &server.description {
                    spec.description
                        .push_str(&format!("\n\nMCP server {}: {description}", server.name));
                }
                registrations.push(ToolRegistration::new(
                    ToolDefinition {
                        spec,
                        result_schema: tool.output_schema.as_ref().map(|schema| schema.hint()),
                        presentation: ToolPresentation::static_target(
                            ToolActivityKind::External,
                            tool.original_name.clone(),
                        ),
                        exposure,
                    },
                    Arc::new(McpExecutor {
                        server: server.clone(),
                        original_name: tool.original_name.clone(),
                        output_schema: tool.output_schema.clone(),
                    }),
                ));
            }
        }
        drop(catalog);
        let live = state
            .servers
            .iter()
            .filter(|server| !server.retiring.is_cancelled())
            .cloned()
            .collect::<Vec<_>>();
        registrations.extend(resource_tools::registrations(&live));
        registrations
    }

    fn refresh<'a>(&'a self, stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(self.refresh_changed(stop))
    }
}

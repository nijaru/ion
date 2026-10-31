//! One immutable connection generation: discovery, retirement and joined cleanup.
use std::{
    path::Path,
    sync::{Arc, RwLock as SyncRwLock, atomic::AtomicU64},
};

use anyhow::{Context, Result, anyhow, ensure};
use ion_core::ToolOutput;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use super::{
    ChangeHandler, McpExposure, McpServer, Server,
    catalog::{Catalog, DiscoveredTools, fetch_tools, register_tools},
    config::{validate_name, validate_server},
    connection::{Connection, combine_cleanup},
    conversion,
};

pub(super) struct Retirement {
    work: tokio::task::JoinHandle<std::result::Result<(), String>>,
    result: Option<std::result::Result<(), String>>,
}

impl Server {
    pub(super) fn exposure_for(&self, name: &str) -> McpExposure {
        self.tool_exposure
            .get(name)
            .copied()
            .or_else(|| {
                self.tool_exposure.iter().find_map(|(pattern, exposure)| {
                    ion_core::tool_name_matches(pattern, name).then_some(*exposure)
                })
            })
            .unwrap_or(self.exposure)
    }

    pub(super) async fn connect(
        name: String,
        definition: McpServer,
        cwd: &Path,
        catalog: Arc<SyncRwLock<Catalog>>,
        stop: CancellationToken,
    ) -> Result<(Server, DiscoveredTools)> {
        ensure!(!stop.is_cancelled(), "MCP startup cancelled");
        validate_name(&name)?;
        validate_server(&definition)?;
        let changed = Arc::new(AtomicU64::new(0));
        let handler = ChangeHandler(changed.clone());
        let description = definition
            .description()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_owned);
        let exposure = definition.exposure();
        let tool_exposure = definition.tool_exposure().clone();
        let client = Connection::start(&name, definition, cwd, handler, stop.clone()).await?;
        let peer = client.client.peer_info();
        let resources = exposure != McpExposure::Hidden
            && peer
                .as_ref()
                .is_some_and(|info| info.capabilities.resources.is_some());
        let advertises_tools = peer
            .as_ref()
            .is_none_or(|info| info.capabilities.tools.is_some());
        let discovered = if advertises_tools {
            let listed = tokio::select! {
                biased;
                () = stop.cancelled() => Err(anyhow!("MCP startup cancelled")),
                result = fetch_tools(&name, &client.client) => result,
            };
            match listed {
                Ok(tools) => {
                    let name = name.clone();
                    conversion::run(move || register_tools(&name, tools)).await
                }
                Err(error) => Err(error),
            }
        } else {
            Ok(DiscoveredTools::default())
        };
        let discovered = discovered.and_then(|tools| {
            ensure!(!stop.is_cancelled(), "MCP startup cancelled");
            Ok(tools)
        });
        let stderr = client.stderr_log();
        match discovered {
            Ok(tools) => Ok((
                Server {
                    name,
                    description,
                    exposure,
                    tool_exposure,
                    resources,
                    client: Arc::new(RwLock::new(Some(client))),
                    stderr,
                    catalog,
                    tool_list_version: changed,
                    refreshed_version: AtomicU64::new(0),
                    retiring: CancellationToken::new(),
                    retirement: Mutex::new(None),
                },
                tools,
            )),
            Err(error) => {
                // A listing failure can be explained only on the server's
                // stderr. Join capture before reading it: stdout and stderr
                // have no cross-stream ordering, so the final bounded prefix
                // is the honest reference for this startup diagnostic.
                let shutdown = client.shutdown().await;
                let error = match stderr.as_ref().and_then(|log| log.notice(&name)) {
                    Some(notice) => error.context(notice),
                    None => error,
                };
                combine_cleanup(Err(error), shutdown)
            }
        }
    }

    pub(super) fn attach_stderr(&self, output: &mut ToolOutput) {
        if let Some(snapshot) = self.stderr.as_ref().and_then(|log| log.snapshot()) {
            output.value["server_stderr"][&self.name] = snapshot;
        }
    }

    pub(super) fn retire(&self) {
        self.retiring.cancel();
    }

    pub(super) async fn shutdown(&self) -> Result<()> {
        // Direct failure/cancellation paths use the same retirement boundary.
        // A request must release its read lease before joining this close.
        self.retire();
        // Retain a running close task, not a future owned only by its waiter.
        // Cancelling shutdown cannot drop a taken connection or its reader joins.
        let mut retirement = self.retirement.lock().await;
        let retirement = retirement.get_or_insert_with(|| {
            let client = self.client.clone();
            let catalog = self.catalog.clone();
            let name = self.name.clone();
            Retirement {
                work: tokio::spawn(async move {
                    let result = match client.write().await.take() {
                        Some(client) => client.shutdown().await,
                        None => Ok(()),
                    };
                    catalog
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&name);
                    result
                        .with_context(|| format!("MCP server {name} shutdown failed"))
                        .map_err(|error| format!("{error:#}"))
                }),
                result: None,
            }
        });
        if retirement.result.is_none() {
            let result = match (&mut retirement.work).await {
                Ok(result) => result,
                Err(error) => Err(format!(
                    "MCP cleanup task failed; effects may be unknown: {error}"
                )),
            };
            retirement.result = Some(result);
        }
        retirement
            .result
            .as_ref()
            .expect("joined retirement has an outcome")
            .clone()
            .map_err(anyhow::Error::msg)
    }
}

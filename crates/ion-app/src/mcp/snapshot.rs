//! Passive connection/catalogue observations, not health checks or dispatch leases.
use serde::Serialize;

use super::{McpExposure, McpTools};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    Available,
    Unavailable,
    Reconnecting,
    CleanupFailed,
    /// Admission is closed; this does not prove cleanup has finished.
    Closing,
}

/// Published valid tools before an application's further loadout selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct McpToolCounts {
    pub direct: usize,
    pub deferred: usize,
    pub hidden: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct McpServerSnapshot {
    pub name: String,
    pub status: McpServerStatus,
    pub tools: McpToolCounts,
    /// Resource access advertised by the published, non-hidden generation.
    pub resources: bool,
}

impl McpTools {
    /// Sample admitted servers in name order without I/O, refresh, configuration
    /// reload, effects or result transfer. Available means a published generation
    /// has not been retired locally, not that a remote health probe succeeded.
    pub fn snapshot(&self) -> Vec<McpServerSnapshot> {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let catalog = self
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.definitions
            .keys()
            .map(|name| {
                let live = state
                    .servers
                    .get(name)
                    .filter(|server| !server.retiring.is_cancelled());
                let status = if self.closing.is_cancelled() {
                    McpServerStatus::Closing
                } else if state.cleanup_failures.contains_key(name) {
                    McpServerStatus::CleanupFailed
                } else if state.reconnects.get(name).is_some_and(|work| {
                    work.completion
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none()
                }) {
                    McpServerStatus::Reconnecting
                } else if live.is_some() {
                    McpServerStatus::Available
                } else {
                    McpServerStatus::Unavailable
                };
                let mut tools = McpToolCounts::default();
                if let Some(server) = live {
                    for tool in catalog.get(name).into_iter().flatten() {
                        match server.exposure_for(&tool.original_name) {
                            McpExposure::Direct => tools.direct += 1,
                            McpExposure::Deferred => tools.deferred += 1,
                            McpExposure::Hidden => tools.hidden += 1,
                        }
                    }
                }
                McpServerSnapshot {
                    name: name.clone(),
                    status,
                    tools,
                    resources: live.is_some_and(|server| server.resources),
                }
            })
            .collect()
    }
}

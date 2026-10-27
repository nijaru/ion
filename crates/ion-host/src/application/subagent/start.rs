//! Coding start policy and complete pre-admission recipe capture.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, ensure};
use ion_ai::{Message, ModelRef, Reasoning};
use ion_core::{DelegationTarget, ForkPoint};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    ChildApplication, DEFAULT_EXECUTION_TIMEOUT_MS, DEFAULT_JOIN_MS, MAX_EXECUTION_TIMEOUT_MS,
    MAX_WAIT_MS, Subagent,
};
use crate::delegation::preparation::check_stop;
use crate::{
    DelegationPreparation, DelegationPreparationError, PreparedDelegation, SessionDelegation,
};

pub(super) const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 30_000;
pub(super) const MAX_STARTUP_TIMEOUT_MS: u64 = 300_000;

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Foreground,
    Background,
}
#[derive(Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Context {
    #[default]
    Fresh,
    Fork {
        #[serde(default)]
        point: Option<ForkPoint>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Start {
    task: String,
    #[serde(default)]
    mode: Mode,
    #[serde(default, deserialize_with = "explicit_join_timeout")]
    join_timeout_ms: Option<u64>,
    context: Option<Context>,
    resume: Option<String>,
    cwd: Option<PathBuf>,
    model: Option<String>,
    reasoning: Option<String>,
    #[serde(default = "default_startup_timeout_ms")]
    pub(super) startup_timeout_ms: u64,
    #[serde(default = "default_execution_timeout_ms")]
    execution_timeout_ms: u64,
}
pub(super) struct PreparedStart {
    pub(super) task: PreparedDelegation,
    pub(super) join_timeout_ms: Option<u64>,
}
impl Start {
    pub(super) async fn prepare(
        self,
        host: &Subagent,
        inherited_reasoning: Reasoning,
        turn: u64,
        stop: &CancellationToken,
    ) -> Result<PreparedStart> {
        ensure!(
            self.join_timeout_ms
                .is_none_or(|ms| (1..=MAX_WAIT_MS).contains(&ms)),
            "join_timeout_ms must be 1..=600000"
        );
        ensure!(
            !matches!(self.mode, Mode::Background) || self.join_timeout_ms.is_none(),
            "join_timeout_ms applies only to foreground start"
        );
        ensure!(
            (1..=MAX_STARTUP_TIMEOUT_MS).contains(&self.startup_timeout_ms),
            "startup_timeout_ms must be 1..=300000"
        );
        ensure!(
            (1..=MAX_EXECUTION_TIMEOUT_MS).contains(&self.execution_timeout_ms),
            "execution_timeout_ms must be 1..=7200000"
        );
        ensure!(
            !self.task.trim().is_empty() && self.task.len() <= 65536,
            "subagent task must be nonblank and at most 65536 UTF-8 bytes"
        );
        ensure!(
            self.resume.is_none() || self.context.is_none(),
            "resume and context are mutually exclusive"
        );
        if let Some(path) = &self.resume {
            ensure!(
                !path.is_empty() && path.len() <= 4096,
                "resume must be 1..=4096 UTF-8 bytes"
            );
        }
        if let Some(path) = &self.cwd {
            ensure!(
                !path.as_os_str().is_empty() && path.as_os_str().as_encoded_bytes().len() <= 4096,
                "cwd must be 1..=4096 UTF-8 bytes"
            );
        }
        let reasoning = self
            .reasoning
            .map(|value| value.parse().map_err(anyhow::Error::msg))
            .transpose()?
            .unwrap_or(inherited_reasoning);
        let identity = self
            .model
            .map(|value| {
                let (provider, model) = value
                    .split_once('/')
                    .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("subagent model must be an exact provider/model identifier")
                    })?;
                Ok::<_, anyhow::Error>(ModelRef {
                    provider: provider.into(),
                    model: model.into(),
                })
            })
            .transpose()?;
        let join_timeout_ms = matches!(self.mode, Mode::Foreground)
            .then(|| self.join_timeout_ms.unwrap_or(DEFAULT_JOIN_MS));
        let inherited_application = host.application.clone();
        let parent_cwd = host.cwd.clone();
        let directory = host.directory.clone();
        let authority = host.authority.clone();
        let mut preparation =
            DelegationPreparation::start(Some(self.startup_timeout_ms), stop, move |stop| {
                check_stop(&stop)?;
                let application = if let Some(identity) = identity {
                    // Same identity retains the captured route, including unsaved
                    // overrides. Alternate identity is resolved exactly once here.
                    let selected = if identity == inherited_application.selected.identity() {
                        inherited_application.selected.clone()
                    } else {
                        inherited_application
                            .coding
                            .host
                            .models()
                            .resolve_identity(&identity)?
                    };
                    Arc::new(ChildApplication {
                        coding: inherited_application.coding.clone(),
                        selected,
                    })
                } else {
                    inherited_application
                };
                check_stop(&stop)?;
                let target = if let Some(path) = self.resume {
                    DelegationTarget::resume(parent_cwd.join(path))?
                } else {
                    match self.context.unwrap_or_default() {
                        Context::Fresh => DelegationTarget::Fresh { directory },
                        Context::Fork { point } => DelegationTarget::Fork {
                            directory,
                            point: point.unwrap_or(ForkPoint::BeforeTurn(turn)),
                        },
                    }
                };
                check_stop(&stop)?;
                let cwd = self
                    .cwd
                    .map_or_else(|| parent_cwd.clone(), |path| parent_cwd.join(path));
                SessionDelegation {
                    target,
                    cwd: Some(cwd),
                    model: Some(application.selected.identity()),
                    application,
                    reasoning,
                    startup_timeout_ms: Some(self.startup_timeout_ms),
                    execution_timeout_ms: Some(self.execution_timeout_ms),
                    authority,
                }
                .preflight(Message::user_input(self.task, []), &stop)
            })?;
        Ok(PreparedStart {
            task: preparation.join().await?,
            join_timeout_ms,
        })
    }
}

pub(super) fn failure(error: anyhow::Error, timeout_ms: u64) -> ion_core::CodingToolOutput {
    let timeout = match error.downcast_ref::<DelegationPreparationError>() {
        Some(DelegationPreparationError::TimedOut { elapsed_ms, .. }) => Some(*elapsed_ms),
        _ => None,
    };
    super::output(
        json!({"error":format!("{error:#}"),"startup_timeout_ms":timeout_ms,"startup_timed_out":timeout.is_some(),"elapsed_ms":timeout}),
        true,
    )
}

// Missing means policy; explicit null is not an integer budget.
fn explicit_join_timeout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}
fn default_startup_timeout_ms() -> u64 {
    DEFAULT_STARTUP_TIMEOUT_MS
}
fn default_execution_timeout_ms() -> u64 {
    DEFAULT_EXECUTION_TIMEOUT_MS
}

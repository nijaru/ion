//! Opt-in coding consumer of ordinary foreground Session delegation.
use std::{path::Path, sync::Arc};

use anyhow::{Result, ensure};
use ion_ai::{BoxFuture, Message, ModelRef, Reasoning, ToolSpec};
use ion_core::{
    AuthorityScope, CodingToolOutput, CodingToolSource, DelegationContext, DelegationOutcome,
    RequestReceipt, ToolActivityKind, ToolDefinition, ToolExecutor, ToolInvocation,
    ToolPresentation, ToolRegistration, TurnEndReason,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;

use super::{Application, ApplicationRuntime, CodingApplication};
use crate::{Resources, Selection, SessionDelegation};

pub(super) struct Subagent {
    recipe: SessionDelegation,
}

/// Preserve the actual selected route, including invocation-local overrides.
/// The child recipe deliberately does not enable another built-in subagent.
struct ChildApplication {
    coding: CodingApplication,
    selected: Selection,
}

impl Application for ChildApplication {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        self.coding.resources(cwd)
    }

    fn runtime(&self, cwd: Option<&Path>, model: Option<&ModelRef>) -> Result<ApplicationRuntime> {
        ensure!(
            model.is_none_or(|model| *model == self.selected.identity()),
            "child model differs from the prepared coding route"
        );
        self.coding.runtime_with_selection(
            cwd.ok_or_else(|| anyhow::anyhow!("coding requires a working directory"))?,
            &self.selected,
        )
    }
}

impl Subagent {
    pub(super) fn new(parent: &CodingApplication, cwd: &Path, selected: &Selection) -> Self {
        let coding = CodingApplication::new(parent.host.clone(), parent.external_tools.clone())
            .with_code_mode(parent.code_mode);
        Self {
            recipe: SessionDelegation {
                context: DelegationContext::Fresh,
                // Keep automatic root-session continuation independent of a
                // newer managed child; explicit child paths remain usable.
                directory: parent
                    .host
                    .sessions(cwd.to_path_buf())
                    .directory()
                    .join("children"),
                cwd: Some(cwd.to_path_buf()),
                application: Arc::new(ChildApplication {
                    coding,
                    selected: selected.clone(),
                }),
                model: Some(selected.identity()),
                reasoning: Reasoning::ProviderDefault,
                authority: AuthorityScope::new("ion.coding.subagent.v1").expect("constant scope"),
            },
        }
    }
}

impl CodingToolSource for Subagent {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        let mut definition = ToolDefinition::external(ToolSpec {
            name: "subagent".into(),
            description: "Delegate one self-contained coding task to a fresh foreground child Session. It uses this selected model route with provider-default reasoning, project instructions, native/MCP tools and workspace, without automatically enabling another built-in subagent. Supply all needed task context: parent conversation is not inherited. The call joins child execution and cleanup before returning its answer and typed child/outcome locator. The answer is the child's claim, not independent verification. Same-workspace tools have host permissions, not sandboxing or conflict isolation. No background transfer, model/workspace selection, automatic retry or unknown-effect replay.".into(),
            input_schema: json!({"type":"object","additionalProperties":false,"required":["task"],"properties":{"task":{"type":"string","minLength":1,"maxLength":65536}}}),
        });
        definition.presentation =
            ToolPresentation::static_target(ToolActivityKind::External, "Coding task");
        vec![ToolRegistration::new(definition, self)]
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    task: String,
}

fn failure(error: impl std::fmt::Display) -> CodingToolOutput {
    CodingToolOutput {
        value: json!({"error":error.to_string()}),
        images: Vec::new(),
        is_error: true,
    }
}

impl ToolExecutor for Subagent {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            if invocation.origin().is_none() {
                return failure("subagent requires an admitted invocation");
            }
            if invocation.stop().is_cancelled() {
                return failure("stopped before subagent preparation");
            }
            let input: Input = match serde_json::from_value(invocation.call().arguments.clone()) {
                Ok(input) => input,
                Err(error) => return failure(error),
            };
            if input.task.trim().is_empty() || input.task.len() > 65536 {
                return failure("subagent task must be nonblank and at most 65536 UTF-8 bytes");
            }
            let (send, mut answer) = oneshot::channel();
            let task = match self.recipe.prepare_with_answer(
                Message::user_input(input.task, []),
                move |text| {
                    let _ = send.send(text);
                },
            ) {
                Ok(task) => task,
                Err(error) => return failure(error),
            };
            let receipt = match invocation.delegate(task).await {
                Ok(receipt) => receipt,
                Err(error) => return failure(error),
            };
            let completed = matches!(
                &receipt.outcome,
                Some(DelegationOutcome::Joined {
                    receipt: Some(RequestReceipt {
                        end: Some(TurnEndReason::Completed),
                        ..
                    }),
                    error: None
                })
            );
            // The actual callback has joined before this receipt. A passive exact
            // reconciliation does not recreate an abandoned answer observation.
            let answer = if completed {
                answer.try_recv().ok()
            } else {
                None
            };
            let missing = completed && answer.is_none();
            CodingToolOutput {
                value: json!({
                    "child_session":receipt.record.child,
                    "path":receipt.record.path,
                    "outcome":receipt.outcome,
                    "answer":answer,
                    "notice":missing.then_some("No local answer observation; inspect the child Session. Reconciliation never reruns work."),
                }),
                images: Vec::new(),
                is_error: !completed || missing,
            }
        })
    }
}

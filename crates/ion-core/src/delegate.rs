//! Bounded child delegation (DESIGN.md §20, Step 7).
//!
//! A child is the same primitive as a root session - a full
//! `SessionRuntime` with its own durable store record - never separate
//! runtime code. [`DelegateTool`] is the model-facing surface: it
//! spawns read-only children with an explicit objective, a runtime
//! budget, and durable lineage, then returns each child's compact
//! result. The full child transcript stays in its own session for
//! inspection; nothing is injected into the parent automatically.
//!
//! Delegation is a structural capability like `compact`: the gate does
//! not require a grant, because every effect a child can produce is
//! individually gated inside the child (§20.4). Nesting is disabled
//! structurally: child catalogs are read-only and never contain a
//! delegate tool.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::ids::SessionId;
use crate::provider::Provider;
use crate::runtime::{Runtime, RuntimeBudget};
use crate::store::SessionStore;
use crate::tool::{Tool, ToolOutcome, ToolSpec};

/// Conservative default bounds for children (§20.5): exact numbers are
/// host configuration; these exist so hosts that do not tune budgets
/// still cannot loop forever.
#[must_use]
pub fn child_budget_default() -> RuntimeBudget {
    RuntimeBudget {
        max_model_steps: Some(16),
        max_tool_calls: Some(64),
    }
}

/// One requested child in a delegation call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    pub objective: String,
    /// Explicit context seed appended after the objective (§20.3):
    /// never an implicit copy of parent state.
    pub context_seed: Option<String>,
}

/// Configuration and bounds for children spawned by one delegate tool.
pub struct DelegateConfig<P> {
    pub store: SessionStore,
    pub make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    /// Maximum concurrently running children (§20.5); further children
    /// wait for a permit.
    pub max_active_children: usize,
    /// Budget applied to every child.
    pub child_budget: RuntimeBudget,
}

/// The model-facing delegation tool. Registered under a dedicated
/// scope by hosts that enable children.
pub struct DelegateTool<P> {
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
}

impl<P> DelegateTool<P> {
    #[must_use]
    pub fn new(config: DelegateConfig<P>, parent_id: SessionId) -> Self {
        Self {
            config: Arc::new(config),
            parent_id,
        }
    }
}

impl<P: Provider + 'static> Tool for DelegateTool<P> {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate".to_owned(),
            description: "Run bounded research children with read-only tools. \
Each child gets an explicit objective and cannot widen capabilities; \
their results return as text. Use for parallel investigation."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "children": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "objective": { "type": "string" },
                                "context": {
                                    "type": "string",
                                    "description": "optional context seed"
                                }
                            },
                            "required": ["objective"]
                        },
                        "minItems": 1,
                        "description": "children to run concurrently"
                    }
                },
                "required": ["children"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(children) = parse_children(&arguments) else {
                return ToolOutcome::error(
                    "malformed arguments: `children` must be a non-empty array of \
                     {objective, context?} objects",
                );
            };
            let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_active_children));
            let mut handles = Vec::with_capacity(children.len());
            for spec in children {
                let semaphore = Arc::clone(&semaphore);
                let config = Arc::clone(&self.config);
                let parent_id = self.parent_id;
                let cancel = cancel.child_token();
                handles.push(tokio::spawn(async move {
                    let _permit = semaphore.acquire().await;
                    run_child(config, parent_id, spec, cancel).await
                }));
            }
            // Parent cancellation cancels descendants (§20.6): the
            // child token above fires, each child's operation cancels,
            // and the results report it - the parent turn continues.
            let mut output = String::new();
            for handle in handles {
                let result = handle
                    .await
                    .unwrap_or_else(|err| format!("child task failed: {err}"));
                if !output.is_empty() {
                    output.push_str("\n\n");
                }
                output.push_str(&result);
            }
            if cancel.is_cancelled() {
                return ToolOutcome::error("cancelled");
            }
            ToolOutcome::text(output)
        })
    }
}

fn parse_children(arguments: &Value) -> Option<Vec<ChildSpec>> {
    let entries = arguments.get("children")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let mut specs = Vec::with_capacity(entries.len());
    for entry in entries {
        let objective = entry.get("objective")?.as_str()?.trim();
        if objective.is_empty() {
            return None;
        }
        specs.push(ChildSpec {
            objective: objective.to_owned(),
            context_seed: entry
                .get("context")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        });
    }
    Some(specs)
}

/// Run one child to its terminal outcome and render the compact
/// result: final assistant text plus the child session reference.
async fn run_child<P>(
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
    spec: ChildSpec,
    cancel: CancellationToken,
) -> String
where
    P: Provider,
{
    // Absolute root: relative cwd would make every child path
    // resolution depend on the host process's working directory.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let catalog = crate::tool::ToolCatalog::read_only(cwd);
    let runtime = Runtime::start_child(
        (config.make_provider)(),
        catalog,
        config.store.clone(),
        Arc::new(crate::policy::DefaultPolicy),
        config.child_budget,
        parent_id,
    );
    let child_id = runtime.session_id();
    let session = runtime.session();

    // Subscribe before submit: live events predate subscribers.
    let Ok((_snapshot, mut events)) = session.subscribe().await else {
        return format!("child failed: could not subscribe ({child_id})");
    };
    let prompt = match &spec.context_seed {
        Some(seed) => format!("{}\n\nContext:\n{seed}", spec.objective),
        None => spec.objective.clone(),
    };
    let Ok(operation_id) = session.submit(prompt).await else {
        return format!("child failed: submit rejected ({child_id})");
    };

    let terminal = tokio::select! {
        outcome = pump_child(&mut events, operation_id) => outcome,
        () = cancel.cancelled() => {
            // §20.6: cancelling the parent cancels descendants; the
            // child settles durably as cancelled on its own.
            let _ = session.cancel(operation_id).await;
            pump_child(&mut events, operation_id).await
        }
    };

    let _ = session.close().await;
    match terminal {
        ChildTerminal::Completed(text) => {
            format!("{text}\n\n[child session: {child_id}]")
        }
        ChildTerminal::Failed(message) => {
            format!("child failed: {message} [child session: {child_id}]")
        }
        ChildTerminal::Cancelled => {
            format!("child cancelled [child session: {child_id}]")
        }
    }
}

enum ChildTerminal {
    Completed(String),
    Failed(String),
    Cancelled,
}

/// Drain child events until the operation terminates, keeping the last
/// assistant draft as the compact result.
async fn pump_child(
    events: &mut crate::runtime::EventSubscription,
    operation_id: crate::ids::OperationId,
) -> ChildTerminal {
    let mut draft = String::new();
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(crate::RuntimeError::SubscriptionLagged) => {
                // The compact result must not present silently
                // incomplete deltas as the child's answer (§21.4).
                return ChildTerminal::Failed("child event stream lagged".to_owned());
            }
            Err(_) => return ChildTerminal::Failed("event stream closed".to_owned()),
        };
        if event.operation_id() != Some(operation_id) {
            continue;
        }
        match event {
            crate::RuntimeEvent::AssistantTextDelta { text, .. } => {
                draft.push_str(&text);
            }
            // Thinking and tool previews are parent-display-only; a
            // child's terminal draft is its final assistant text.
            crate::RuntimeEvent::ThinkingDelta { .. } => {}
            crate::RuntimeEvent::OperationFinished { .. } => {
                let result = if draft.is_empty() {
                    "(no output)".to_owned()
                } else {
                    draft
                };
                return ChildTerminal::Completed(result);
            }
            crate::RuntimeEvent::OperationCancelled { .. } => {
                return ChildTerminal::Cancelled;
            }
            crate::RuntimeEvent::OperationFailed { message, .. } => {
                return ChildTerminal::Failed(message);
            }
            crate::RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                return ChildTerminal::Failed(format!(
                    "approval required for `{tool}` (read-only child)"
                ));
            }
            crate::RuntimeEvent::ToolStarted { .. }
            | crate::RuntimeEvent::ToolSettled { .. }
            | crate::RuntimeEvent::OperationStarted { .. }
            | crate::RuntimeEvent::SessionClosed { .. } => {}
        }
    }
}

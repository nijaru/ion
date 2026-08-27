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

use crate::context::{ContextMessage, ContextPlan, TrustedResource, project};
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

/// How a child receives parent context. `Fresh` is the safe default; the
/// parent transcript is never copied unless the caller explicitly selects
/// `ForkContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildContextMode {
    Fresh,
    ForkContext,
}

/// One requested child in a delegation call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    pub objective: String,
    /// Explicit context seed appended after the objective (§20.3):
    /// never an implicit copy of parent state.
    pub context_seed: Option<String>,
    /// Explicit parent-context projection mode (§20.3).
    pub context_mode: ChildContextMode,
    /// Optional host-resolved model for this child only.
    pub model_override: Option<String>,
}

/// Configuration and bounds for children spawned by one delegate tool.
pub struct DelegateConfig<P> {
    pub store: SessionStore,
    pub make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    /// Optional resolver for explicit per-call model overrides. Unsupported
    /// overrides fail visibly instead of silently using the launch model.
    pub make_provider_for_model: Option<Arc<dyn Fn(String) -> P + Send + Sync>>,
    /// Maximum concurrently running children (§20.5); further children
    /// wait for a permit.
    pub max_active_children: usize,
    /// Budget applied to every child.
    pub child_budget: RuntimeBudget,
    /// Explicitly inherited project resources; empty when the host did not
    /// grant project trust.
    pub trusted_resources: Vec<TrustedResource>,
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
                                },
                                "context_mode": {
                                    "type": "string",
                                    "enum": ["fresh", "fork_context"],
                                    "description": "fresh by default; explicitly fork durable parent context"
                                },
                                "model_override": {
                                    "type": "string",
                                    "description": "optional host-resolved model for this child"
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
            let children = match parse_children(&arguments) {
                Ok(children) => children,
                Err(message) => return ToolOutcome::error(message),
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

fn parse_children(arguments: &Value) -> Result<Vec<ChildSpec>, String> {
    let entries = arguments
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "malformed arguments: `children` must be a non-empty array of objects".to_owned()
        })?;
    if entries.is_empty() {
        return Err("malformed arguments: `children` cannot be empty".to_owned());
    }
    let mut specs = Vec::with_capacity(entries.len());
    for entry in entries {
        let objective = entry
            .get("objective")
            .and_then(Value::as_str)
            .ok_or_else(|| "malformed child: `objective` must be a string".to_owned())?
            .trim();
        if objective.is_empty() {
            return Err("malformed child: `objective` cannot be empty".to_owned());
        }
        let context_mode = match entry.get("context_mode").and_then(Value::as_str) {
            None | Some("fresh") => ChildContextMode::Fresh,
            Some("fork_context") => ChildContextMode::ForkContext,
            Some(other) => {
                return Err(format!(
                    "malformed child: unsupported `context_mode` {other:?}"
                ));
            }
        };
        let model_override = match entry.get("model_override") {
            None => None,
            Some(value) => {
                let model = value
                    .as_str()
                    .ok_or_else(|| "malformed child: `model_override` must be a string".to_owned())?
                    .trim();
                if model.is_empty() {
                    return Err("malformed child: `model_override` cannot be empty".to_owned());
                }
                Some(model.to_owned())
            }
        };
        specs.push(ChildSpec {
            objective: objective.to_owned(),
            context_seed: entry
                .get("context")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            context_mode,
            model_override,
        });
    }
    Ok(specs)
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
    let provider = match spec.model_override.as_deref() {
        Some(model_ref) => {
            let Some(make_provider) = config.make_provider_for_model.as_ref() else {
                return format!(
                    "child failed: model override `{model_ref}` is unavailable [child parent: {parent_id}]"
                );
            };
            make_provider(model_ref.to_owned())
        }
        None => (config.make_provider)(),
    };
    let fork_context_result = match spec.context_mode {
        ChildContextMode::Fresh => Ok(None),
        ChildContextMode::ForkContext => fork_context(&config.store, parent_id).await,
    };
    let fork_context = match fork_context_result {
        Ok(context) => context,
        Err(err) => return format!("child failed: {err} [child parent: {parent_id}]"),
    };
    let prompt = compose_child_prompt(&spec, fork_context.as_deref());
    let runtime = Runtime::start_child_with_resources(
        provider,
        catalog.clone(),
        config.store.clone(),
        Arc::new(crate::policy::DefaultPolicy),
        config.child_budget,
        parent_id,
        config.trusted_resources.clone(),
    );
    let child_id = runtime.session_id();
    let session = runtime.session();

    // Subscribe before submit: live events predate subscribers.
    let (_snapshot, mut events) = match session.subscribe().await {
        Ok(subscription) => subscription,
        Err(_) => {
            let _ = session.close().await;
            let _ = runtime.join().await;
            let catalog_error = catalog.close().await.err();
            return match catalog_error {
                Some(err) => format!(
                    "child failed: could not subscribe; catalog close error: {err} ({child_id})"
                ),
                None => format!("child failed: could not subscribe ({child_id})"),
            };
        }
    };
    let operation_id = match session.submit_if_idle(prompt).await {
        Ok(operation_id) => operation_id,
        Err(_) => {
            let _ = session.close().await;
            let _ = runtime.join().await;
            let catalog_error = catalog.close().await.err();
            return match catalog_error {
                Some(err) => format!(
                    "child failed: submit rejected; catalog close error: {err} ({child_id})"
                ),
                None => format!("child failed: submit rejected ({child_id})"),
            };
        }
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

    let close_result = session.close().await;
    let join_result = runtime.join().await;
    if let Err(err) = close_result {
        return format!("child failed: close error: {err} [child session: {child_id}]");
    }
    if let Err(err) = join_result {
        return format!("child failed: runtime join error: {err} [child session: {child_id}]");
    }
    if let Err(err) = catalog.close().await {
        return format!("child failed: catalog close error: {err} [child session: {child_id}]");
    }
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

async fn fork_context(
    store: &SessionStore,
    parent_id: SessionId,
) -> Result<Option<String>, String> {
    let loaded = store
        .load(parent_id)
        .await
        .map_err(|err| format!("could not load parent context: {err}"))?;
    let first_seq = loaded.entries.first().map_or(1, |(seq, _)| *seq);
    let entries: Vec<_> = loaded.entries.into_iter().map(|(_, entry)| entry).collect();
    if entries.is_empty() {
        return Ok(None);
    }
    let plan = project(&entries, first_seq);
    Ok(Some(render_fork_context(&plan)))
}

fn compose_child_prompt(spec: &ChildSpec, fork: Option<&str>) -> String {
    let mut prompt = spec.objective.clone();
    if let Some(fork) = fork {
        prompt.push_str("\n\n[Explicit fork of parent semantic context]\n");
        prompt.push_str(fork);
    }
    if let Some(seed) = &spec.context_seed {
        prompt.push_str("\n\n[Explicit child context seed]\n");
        prompt.push_str(seed);
    }
    prompt
}

fn render_fork_context(plan: &ContextPlan) -> String {
    const MAX_BYTES: usize = 16 * 1024;
    let mut rendered = String::new();
    for message in &plan.messages {
        match message {
            ContextMessage::User { content } => {
                rendered.push_str("User:\n");
                rendered.push_str(content);
                rendered.push('\n');
            }
            ContextMessage::Assistant {
                content,
                tool_calls,
            } => {
                rendered.push_str("Assistant:\n");
                rendered.push_str(content);
                for call in tool_calls {
                    rendered.push_str("\n[tool call ");
                    rendered.push_str(&call.name);
                    rendered.push_str("] ");
                    rendered.push_str(&call.arguments.to_string());
                }
                rendered.push('\n');
            }
            ContextMessage::Tool { call_id, content } => {
                rendered.push_str("Tool result ");
                rendered.push_str(&call_id.to_string());
                rendered.push_str(":\n");
                rendered.push_str(content);
                rendered.push('\n');
            }
        }
    }
    truncate_context(&rendered, MAX_BYTES)
}

fn truncate_context(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let marker = "\n[… parent context truncated …]\n";
    let budget = max_bytes.saturating_sub(marker.len());
    let head_limit = budget / 2;
    let head_end = text
        .char_indices()
        .take_while(|(index, ch)| *index + ch.len_utf8() <= head_limit)
        .map(|(index, ch)| index + ch.len_utf8())
        .last()
        .unwrap_or(0);
    let tail_start_limit = text.len().saturating_sub(budget - head_end);
    let tail_start = text
        .char_indices()
        .find(|(index, _)| *index >= tail_start_limit)
        .map_or(text.len(), |(index, _)| index);
    format!("{}{}{}", &text[..head_end], marker, &text[tail_start..])
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
            crate::RuntimeEvent::OperationIndeterminate { message, .. } => {
                return ChildTerminal::Failed(format!("indeterminate operation: {message}"));
            }
            crate::RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                return ChildTerminal::Failed(format!(
                    "approval required for `{tool}` (read-only child)"
                ));
            }
            // Children are non-interactive and read-only: a parked
            // approval cannot occur, so surface it as a failure if it
            // ever does (defense, not a normal path).
            crate::RuntimeEvent::ApprovalPending { tool, .. } => {
                return ChildTerminal::Failed(format!(
                    "approval pending for `{tool}` (read-only child)"
                ));
            }
            crate::RuntimeEvent::ToolStarted { .. }
            | crate::RuntimeEvent::ToolSettled { .. }
            | crate::RuntimeEvent::UsageUpdate { .. }
            | crate::RuntimeEvent::OperationStarted { .. }
            | crate::RuntimeEvent::SessionClosed { .. } => {}
        }
    }
}

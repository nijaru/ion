//! Model-visible tool definitions, semantic presentation, and request-bound routing.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::tool_result::ToolOutput;

mod discovery;
mod presentation;
mod selection;

pub use presentation::{ToolActivity, ToolActivityKind, ToolPresentation, ToolPresentationTarget};
pub use selection::{ToolSelection, tool_name_matches};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExposure {
    /// Declared directly to the model for this request.
    Direct,
    /// Callable by the harness but omitted from the default model loadout.
    Deferred,
}

/// A standalone JSON Schema describing expected successful data at an object-key
/// path within `ToolOutput::value`. Empty paths describe that whole value.
/// This is discovery metadata, not runtime validation or authority. Keeping the
/// schema separate from its location preserves its local reference scope.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultSchema {
    pub value_path: Vec<String>,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub spec: ToolSpec,
    pub presentation: ToolPresentation,
    pub exposure: ToolExposure,
    pub result_schema: Option<ToolResultSchema>,
}

impl ToolDefinition {
    pub fn external(spec: ToolSpec) -> Self {
        Self {
            spec,
            presentation: ToolPresentation::external(),
            exposure: ToolExposure::Direct,
            result_schema: None,
        }
    }

    /// Annotate the complete successful `ToolOutput::value`, without changing
    /// the executor, provider request, observed value or error handling.
    pub fn with_result_schema(mut self, schema: Value) -> Self {
        self.result_schema = Some(ToolResultSchema {
            value_path: Vec::new(),
            schema,
        });
        self
    }

    pub fn deferred(mut self) -> Self {
        self.exposure = ToolExposure::Deferred;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ToolExecution {
    Observed {
        output: ToolOutput,
        /// Deferred capabilities to add to the next request's declared loadout.
        activate: Vec<ToolSpec>,
    },
    NotDispatched {
        reason: String,
    },
}

impl ToolExecution {
    pub(crate) fn output(output: ToolOutput) -> Self {
        Self::Observed {
            output,
            activate: Vec::new(),
        }
    }
}

pub(crate) enum ToolDispatch<'a> {
    Rejected(String),
    Execution(BoxFuture<'a, ToolExecution>),
    Composition(&'a Arc<dyn crate::CodeRuntime>, crate::CodeLimits),
    /// Session-owned historical evidence retrieval. The active Session is
    /// supplied by the dispatcher; this route never resolves one itself.
    History,
}

impl ToolDispatch<'_> {
    fn rejected(error: impl Into<String>) -> Self {
        Self::Rejected(error.into())
    }
}

const TOOL_SEARCH_NAME: &str = "tool_search";
const DEFAULT_TOOL_SEARCH_LIMIT: usize = 5;
const MAX_TOOL_SEARCH_LIMIT: usize = 10;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolSearchInput {
    query: String,
    limit: Option<usize>,
}

fn tool_search_definition() -> ToolDefinition {
    ToolDefinition {
        result_schema: None,
        spec: ToolSpec {
            name: TOOL_SEARCH_NAME.into(),
            description: "Search tools that are callable by the harness but not declared in this request. Matching tools are loaded into the next model request.".into(),
            input_schema: serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "required":["query"],
                "properties":{
                    "query":{"type":"string","minLength":1},
                    "limit":{"type":"integer","minimum":1,"maximum":MAX_TOOL_SEARCH_LIMIT}
                }
            }),
        },
        presentation: ToolPresentation::argument(ToolActivityKind::Search, "query"),
        exposure: ToolExposure::Direct,
    }
}

/// A concrete operation bound at registration time. Execution must not resolve
/// the call again through a source's mutable registry. A captured remote
/// connection may fail, but must not be silently replaced or replay the call.
pub trait ToolExecutor: Send + Sync {
    /// Respond to `stop`, settle owned work and return its observed outcome.
    /// The Turn awaits this future through cancellation; dropping it cannot
    /// establish settlement or undo unobserved external effects.
    fn execute<'a>(&'a self, invocation: crate::ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput>;
}

/// An inseparable definition and executable identity, published for a future
/// request. Refresh replaces registrations, not already-issued capabilities.
pub struct ToolRegistration {
    pub definition: ToolDefinition,
    pub executor: Arc<dyn ToolExecutor>,
}

impl ToolRegistration {
    pub fn new(definition: ToolDefinition, executor: Arc<dyn ToolExecutor>) -> Self {
        Self {
            definition,
            executor,
        }
    }
}

/// Discovery/lifecycle owner of registrations, never a name-dispatched executor.
pub trait ToolSource: Send + Sync {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration>;

    /// Whether a future publication may add deferred tools. A request samples
    /// this before registrations so concurrent publication cannot hide search.
    fn deferred_pending(&self) -> bool {
        false
    }

    /// Wait for deferred inventory. Dropping this wait must not cancel owned work.
    fn wait_for_deferred(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn refresh<'a>(&'a self, _stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async { Vec::new() })
    }
}

/// Composition owner for independent sources. Later registrations replace
/// earlier ones with the same model-visible name.
pub struct ToolSet {
    sources: Vec<Arc<dyn ToolSource>>,
    composition: Option<(Arc<dyn crate::CodeRuntime>, crate::CodeLimits)>,
    policy: Option<Arc<dyn crate::ToolPolicy>>,
    history: bool,
    selection: ToolSelection,
}

impl ToolSet {
    pub fn new(sources: impl IntoIterator<Item = Arc<dyn ToolSource>>) -> Self {
        Self {
            sources: sources.into_iter().collect(),
            composition: None,
            policy: None,
            history: false,
            selection: ToolSelection::all(),
        }
    }

    /// Capture an optional veto with each future catalog. Issued requests keep
    /// their original policy and routes through refresh or binding replacement.
    pub fn with_policy(mut self, policy: Arc<dyn crate::ToolPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Narrow the model-visible loadout without changing source registration or
    /// captured executor authority.
    pub fn with_selection(mut self, selection: ToolSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Opt in to a Session-owned composed call alongside the direct tools.
    pub fn with_code_mode(
        mut self,
        runtime: Arc<dyn crate::CodeRuntime>,
        limits: crate::CodeLimits,
    ) -> Self {
        self.composition = Some((runtime, limits));
        self
    }

    /// Opt in to Core's read-only committed-history tool. This is a coding
    /// preset choice, not ambient authority for every restricted application.
    pub fn with_history(mut self) -> Self {
        self.history = true;
        self
    }

    pub async fn refresh(&self, stop: CancellationToken) -> Vec<String> {
        let mut diagnostics = Vec::new();
        for source in &self.sources {
            if stop.is_cancelled() {
                break;
            }
            diagnostics.extend(source.refresh(stop.clone()).await);
        }
        diagnostics
    }

    /// Freeze one coherent executable catalog using only directly exposed tools
    /// in the model loadout.
    pub fn snapshot(&self) -> ToolCatalog {
        self.snapshot_with_previous(&[])
    }

    /// Freeze one coherent executable catalog and restore previously declared
    /// deferred tools only when their provider-neutral definition is unchanged.
    pub fn snapshot_with_previous(&self, previous: &[ToolSpec]) -> ToolCatalog {
        self.build_catalog(previous, false)
    }

    pub(crate) fn request_catalog(&self, previous: &[ToolSpec]) -> ToolCatalog {
        self.build_catalog(previous, true)
    }

    fn build_catalog(&self, previous: &[ToolSpec], allow_wait: bool) -> ToolCatalog {
        let discovery = (allow_wait && self.selection.allows(TOOL_SEARCH_NAME))
            .then(|| {
                self.sources
                    .iter()
                    .filter(|source| source.deferred_pending())
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|sources| !sources.is_empty())
            .map(|pending| PendingDiscovery {
                pending,
                sources: self.sources.clone(),
            });
        let (registrations, mut positions) =
            Self::source_registrations(&self.sources, &self.selection);
        let mut entries = registrations
            .into_iter()
            .map(|registration| {
                RoutedTool::new(
                    registration.definition,
                    ToolRoute::Executor(registration.executor),
                )
            })
            .collect::<Vec<_>>();
        if let Some((runtime, limits)) = &self.composition {
            let definition = crate::composition::definition(*limits);
            let name = definition.spec.name.clone();
            if self.selection.allows(&name) {
                let routed =
                    RoutedTool::new(definition, ToolRoute::Composition(runtime.clone(), *limits));
                match positions.get(&name).copied() {
                    Some(index) => entries[index] = routed,
                    None => {
                        positions.insert(name, entries.len());
                        entries.push(routed);
                    }
                }
            }
        }
        if self.history {
            let definition = crate::session::history::definition();
            let name = definition.spec.name.clone();
            if self.selection.allows(&name) {
                let routed = RoutedTool::new(definition, ToolRoute::History);
                match positions.get(&name).copied() {
                    Some(index) => entries[index] = routed,
                    None => {
                        positions.insert(name, entries.len());
                        entries.push(routed);
                    }
                }
            }
        }
        ToolCatalog::from_entries(
            entries,
            previous,
            self.selection.clone(),
            self.policy.clone(),
            discovery,
        )
    }

    fn source_registrations(
        sources: &[Arc<dyn ToolSource>],
        selection: &ToolSelection,
    ) -> (Vec<ToolRegistration>, HashMap<String, usize>) {
        let mut entries = Vec::new();
        let mut positions = HashMap::new();
        for source in sources {
            for registration in source.clone().registrations() {
                let name = registration.definition.spec.name.clone();
                if !selection.allows(&name) {
                    continue;
                }
                match positions.get(&name).copied() {
                    Some(index) => entries[index] = registration,
                    None => {
                        positions.insert(name, entries.len());
                        entries.push(registration);
                    }
                }
            }
        }
        (entries, positions)
    }
}

struct PendingDiscovery {
    pending: Vec<Arc<dyn ToolSource>>,
    sources: Vec<Arc<dyn ToolSource>>,
}

#[derive(Clone)]
enum ToolRoute {
    Executor(Arc<dyn ToolExecutor>),
    Search,
    Composition(Arc<dyn crate::CodeRuntime>, crate::CodeLimits),
    History,
}

#[derive(Clone)]
struct RoutedTool {
    definition: ToolDefinition,
    route: ToolRoute,
    // Rebuilding exposure shares this exact compiled contract, including an
    // explicit unavailable contract for unsupported external definitions.
    input_schema: Arc<Result<crate::json_schema::CompiledSchema, String>>,
}

impl RoutedTool {
    fn new(definition: ToolDefinition, route: ToolRoute) -> Self {
        let input_schema = Arc::new(crate::json_schema::CompiledSchema::compile(
            &definition.spec.input_schema,
        ));
        Self {
            definition,
            route,
            input_schema,
        }
    }
}

/// Immutable tool schema/presentation/route view used by one model request and
/// the tool calls produced from that request.
pub struct ToolCatalog {
    policy: Option<Arc<dyn crate::ToolPolicy>>,
    entries: Vec<RoutedTool>,
    positions: HashMap<String, usize>,
    declared: HashSet<String>,
    selection: ToolSelection,
    discovery: Option<PendingDiscovery>,
}

impl ToolCatalog {
    fn from_entries(
        mut entries: Vec<RoutedTool>,
        previous: &[ToolSpec],
        selection: ToolSelection,
        policy: Option<Arc<dyn crate::ToolPolicy>>,
        discovery: Option<PendingDiscovery>,
    ) -> Self {
        let mut positions = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| (entry.definition.spec.name.clone(), i))
            .collect::<HashMap<_, _>>();
        let previous = previous
            .iter()
            .map(|spec| (spec.name.as_str(), spec))
            .collect::<HashMap<_, _>>();
        let mut declared = entries
            .iter()
            .filter(|entry| entry.input_schema.is_ok())
            .filter(|entry| {
                entry.definition.exposure == ToolExposure::Direct
                    || previous
                        .get(entry.definition.spec.name.as_str())
                        .is_some_and(|spec| **spec == entry.definition.spec)
            })
            .map(|entry| entry.definition.spec.name.clone())
            .collect::<HashSet<_>>();

        if selection.allows(TOOL_SEARCH_NAME)
            && (discovery.is_some()
                || entries.iter().any(|entry| {
                    entry.input_schema.is_ok()
                        && entry.definition.exposure == ToolExposure::Deferred
                        && !declared.contains(&entry.definition.spec.name)
                }))
        {
            let definition = tool_search_definition();
            let name = definition.spec.name.clone();
            let routed = RoutedTool::new(definition, ToolRoute::Search);
            match positions.get(&name).copied() {
                Some(index) => entries[index] = routed,
                None => {
                    positions.insert(name.clone(), entries.len());
                    entries.push(routed);
                }
            }
            declared.insert(name);
        }

        ToolCatalog {
            policy,
            entries,
            positions,
            declared,
            selection,
            discovery,
        }
    }

    /// Rebuild exposure without consulting sources or replacing frozen executors.
    pub(crate) fn with_previous(&self, previous: &[ToolSpec]) -> Self {
        Self::from_entries(
            self.entries
                .iter()
                .filter(|entry| !matches!(entry.route, ToolRoute::Search))
                .cloned()
                .collect(),
            previous,
            self.selection.clone(),
            self.policy.clone(),
            None,
        )
    }

    /// Callable declarations, not opaque executor identity. Discovery's temporary
    /// search route changes model exposure, not application authority.
    pub(crate) fn authority_manifest(&self) -> Vec<(&ToolSpec, bool, Option<crate::CodeLimits>)> {
        self.callable_entries()
            .filter_map(|entry| {
                let limits = match &entry.route {
                    ToolRoute::Search => return None,
                    ToolRoute::Composition(_, limits) => Some(*limits),
                    ToolRoute::Executor(_) | ToolRoute::History => None,
                };
                Some((
                    &entry.definition.spec,
                    entry.definition.exposure == ToolExposure::Deferred,
                    limits,
                ))
            })
            .collect()
    }
    /// Every callable definition in this frozen catalog, including deferred
    /// capabilities that are not declared directly to the model.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.callable_entries()
            .map(|entry| entry.definition.spec.clone())
            .collect()
    }

    /// Provider-facing loadout for this request.
    pub fn declared_specs(&self) -> Vec<ToolSpec> {
        self.entries
            .iter()
            .filter(|entry| self.declared.contains(&entry.definition.spec.name))
            .map(|entry| entry.definition.spec.clone())
            .collect()
    }

    pub fn is_declared(&self, name: &str) -> bool {
        self.declared.contains(name)
    }

    fn callable_entries(&self) -> impl Iterator<Item = &RoutedTool> {
        self.entries
            .iter()
            .filter(|entry| entry.input_schema.is_ok())
    }

    pub(crate) fn definitions(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.callable_entries().map(|entry| &entry.definition)
    }

    /// Selected registrations unavailable for model dispatch, with local diagnostics.
    pub fn unavailable(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|entry| {
            let reason = entry.input_schema.as_ref().as_ref().err()?;
            Some((entry.definition.spec.name.as_str(), reason.as_str()))
        })
    }

    pub(crate) fn callable_definition(&self, name: &str) -> Option<&ToolDefinition> {
        let entry = self.entry(name)?;
        entry.input_schema.is_ok().then_some(&entry.definition)
    }

    fn entry(&self, name: &str) -> Option<&RoutedTool> {
        self.positions
            .get(name)
            .and_then(|index| self.entries.get(*index))
    }

    pub fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.entry(name).map(|entry| &entry.definition)
    }

    /// Persist exact discovery results, never a promise that a name will
    /// resolve to the same definition when the next request is built.
    pub(crate) fn context_after_discovery(&self, additional: &[ToolSpec]) -> Vec<ToolSpec> {
        let mut specs = self
            .callable_entries()
            .filter(|entry| {
                self.declared.contains(&entry.definition.spec.name)
                    && !matches!(entry.route, ToolRoute::Search)
            })
            .map(|entry| entry.definition.spec.clone())
            .collect::<Vec<_>>();
        specs.extend_from_slice(additional);
        if let Some(search) = self
            .entry(TOOL_SEARCH_NAME)
            .filter(|entry| matches!(entry.route, ToolRoute::Search))
            && (self.discovery.is_some()
                || self.callable_entries().any(|entry| {
                    let definition = &entry.definition;
                    definition.exposure == ToolExposure::Deferred
                        && !self.declared.contains(&definition.spec.name)
                        && !additional.contains(&definition.spec)
                }))
        {
            specs.push(search.definition.spec.clone());
        }
        specs
    }

    /// Search callable definitions with the same ranking as the model's
    /// deferred discovery tool. Intrinsic dispatch routes are not guest tools.
    pub(crate) fn discover_callable(&self, query: &str, limit: usize) -> Vec<&ToolDefinition> {
        discovery::rank(
            query,
            self.callable_entries()
                .filter(|entry| {
                    !matches!(entry.route, ToolRoute::Search | ToolRoute::Composition(..))
                })
                .map(|entry| &entry.definition),
            limit,
        )
    }

    pub(crate) fn is_intrinsic(&self, name: &str) -> bool {
        self.positions
            .get(name)
            .and_then(|index| self.entries.get(*index))
            .is_some_and(|entry| {
                matches!(entry.route, ToolRoute::Search | ToolRoute::Composition(..))
            })
    }

    pub fn activity(&self, call: &ToolCall) -> ToolActivity {
        self.definition(&call.name).map_or_else(
            || ToolActivity::external(&call.name),
            |definition| definition.presentation.resolve(call),
        )
    }

    /// Invoke a frozen capability, including a deferred one. This low-level
    /// operation enforces the registered input schema but does not admit a Turn
    /// or publish Session facts; coding execution
    /// must remain inside the Turn's commit/settlement boundary.
    /// Supply an untracked invocation for low-level use, optionally with an
    /// explicit artifact sink. An invocation is not an admission receipt.
    pub fn execute<'a>(
        &'a self,
        invocation: crate::ToolInvocation<'a>,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            match self.dispatch(invocation).await {
                ToolDispatch::Rejected(reason) => crate::ToolOutcome::NotDispatched { reason }
                    .inspection_output()
                    .into_owned(),
                ToolDispatch::Execution(execution) => match execution.await {
                    ToolExecution::Observed { output, .. } => output,
                    ToolExecution::NotDispatched { reason } => {
                        crate::ToolOutcome::NotDispatched { reason }
                            .inspection_output()
                            .into_owned()
                    }
                },
                ToolDispatch::Composition(..) => ToolOutput {
                    artifacts: Vec::new(),
                    value: serde_json::json!({"error":"code_mode requires the Session effect gateway"}),
                    images: Vec::new(),
                    is_error: true,
                },
                ToolDispatch::History => ToolOutput {
                    artifacts: Vec::new(),
                    value: serde_json::json!({"error":"history requires an active Session"}),
                    images: Vec::new(),
                    is_error: true,
                },
            }
        })
    }

    /// Execute a model-issued call only when its definition was part of this
    /// request's declared loadout.
    pub(crate) async fn execute_model_call<'a>(
        &'a self,
        invocation: crate::ToolInvocation<'a>,
    ) -> ToolDispatch<'a> {
        let call = invocation.call();
        if !self.is_declared(&call.name) {
            return ToolDispatch::rejected(format!(
                "tool was not declared for this request: {}",
                call.name
            ));
        }
        self.dispatch(invocation).await
    }

    pub(crate) async fn dispatch<'a>(
        &'a self,
        invocation: crate::ToolInvocation<'a>,
    ) -> ToolDispatch<'a> {
        let call = invocation.call();
        if call.raw_arguments.is_some() || !call.arguments.is_object() {
            return ToolDispatch::rejected(
                "tool arguments were not a valid JSON object; submit a corrected call",
            );
        }
        let Some(entry) = self.entry(&call.name) else {
            return ToolDispatch::rejected(format!("unknown tool: {}", call.name));
        };
        // All routes (including composition/history and composed children) pass
        // this one frozen-schema gate before asking policy or starting effects.
        let validator = match entry.input_schema.as_ref() {
            Ok(validator) => validator,
            Err(reason) => {
                return ToolDispatch::rejected(format!("tool input schema unavailable: {reason}"));
            }
        };
        if let Err(error) = validator.validate(&call.arguments) {
            return ToolDispatch::rejected(format!(
                "tool input schema check failed ({error}); submit a corrected call"
            ));
        }
        if let Some(policy) = &self.policy {
            let Some(origin) = invocation.origin() else {
                return ToolDispatch::rejected("tool policy requires an admitted invocation");
            };
            if invocation.stop().is_cancelled() {
                return ToolDispatch::rejected("stopped before policy dispatch");
            }
            let stop = invocation.stop().child_token();
            let decision = policy
                .decide(
                    crate::ToolPolicyRequest {
                        call,
                        definition: &entry.definition,
                        origin,
                    },
                    stop.clone(),
                )
                .await;
            if stop.is_cancelled() {
                return ToolDispatch::rejected("stopped before tool dispatch");
            }
            match decision {
                Ok(crate::ToolDecision::Allow) => {}
                Ok(crate::ToolDecision::Deny { reason }) => return ToolDispatch::Rejected(reason),
                Err(error) => {
                    return ToolDispatch::rejected(format!("tool policy failed: {error}"));
                }
            }
        }
        if invocation.stop().is_cancelled() {
            return ToolDispatch::rejected("stopped before tool dispatch");
        }
        match &entry.route {
            ToolRoute::Search => ToolDispatch::Execution(Box::pin(async move {
                self.search_deferred(call, invocation.stop()).await
            })),
            ToolRoute::Executor(executor) => {
                // Selecting a frozen route grants no host authority. Even an
                // executor's synchronous constructor runs only when polled.
                ToolDispatch::Execution(Box::pin(async move {
                    if invocation.stop().is_cancelled() {
                        return ToolExecution::NotDispatched {
                            reason: "stopped before executor dispatch".into(),
                        };
                    }
                    ToolExecution::output(executor.execute(invocation).await)
                }))
            }
            ToolRoute::Composition(runtime, limits) => ToolDispatch::Composition(runtime, *limits),
            ToolRoute::History => ToolDispatch::History,
        }
    }

    async fn search_deferred(&self, call: &ToolCall, stop: &CancellationToken) -> ToolExecution {
        let input: ToolSearchInput = match serde_json::from_value(call.arguments.clone()) {
            Ok(input) => input,
            Err(error) => {
                return ToolExecution::output(ToolOutput {
                    artifacts: Vec::new(),
                    value: serde_json::json!({
                        "error": format!("invalid tool_search arguments: {error}")
                    }),
                    images: Vec::new(),
                    is_error: true,
                });
            }
        };
        let query = input.query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return ToolExecution::output(ToolOutput {
                artifacts: Vec::new(),
                value: serde_json::json!({"error":"tool_search query is empty"}),
                images: Vec::new(),
                is_error: true,
            });
        }
        let limit = input.limit.unwrap_or(DEFAULT_TOOL_SEARCH_LIMIT);
        if !(1..=MAX_TOOL_SEARCH_LIMIT).contains(&limit) {
            return ToolExecution::output(ToolOutput {
                artifacts: Vec::new(),
                value: serde_json::json!({
                    "error": format!("tool_search limit must be between 1 and {MAX_TOOL_SEARCH_LIMIT}")
                }),
                images: Vec::new(),
                is_error: true,
            });
        }

        // The discovery view is inert: these registrations never become routes
        // in the response that issued this search (including composed siblings).
        let not_loaded = |definition: &ToolDefinition| {
            definition.exposure == ToolExposure::Deferred
                && !self.declared.contains(&definition.spec.name)
        };
        let fresh;
        let matches = if let Some(discovery) = &self.discovery {
            for source in &discovery.pending {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => return ToolExecution::NotDispatched {
                        reason: "stopped while waiting for deferred tool inventory".into(),
                    },
                    () = source.wait_for_deferred() => {},
                }
            }
            fresh = ToolSet::source_registrations(&discovery.sources, &self.selection)
                .0
                .into_iter()
                .map(|registration| registration.definition)
                .collect::<Vec<_>>();
            discovery::rank(
                &query,
                fresh.iter().filter(|definition| {
                    not_loaded(definition)
                        && crate::json_schema::CompiledSchema::compile(
                            &definition.spec.input_schema,
                        )
                        .is_ok()
                }),
                limit,
            )
        } else {
            discovery::rank(
                &query,
                self.callable_entries()
                    .map(|entry| &entry.definition)
                    .filter(|definition| not_loaded(definition)),
                limit,
            )
        };

        let activate = matches
            .iter()
            .map(|definition| definition.spec.clone())
            .collect::<Vec<_>>();
        let tools = matches
            .iter()
            .map(|definition| {
                serde_json::json!({
                    "name": definition.spec.name,
                    "description": definition.spec.description,
                })
            })
            .collect::<Vec<_>>();
        ToolExecution::Observed {
            output: ToolOutput {
                artifacts: Vec::new(),
                value: serde_json::json!({
                    "loaded": tools,
                    "count": tools.len(),
                    "message": "Matching tools are declared in the next model request."
                }),
                images: Vec::new(),
                is_error: false,
            },
            activate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Stub {
        definition: ToolDefinition,
        result: &'static str,
    }

    impl Stub {
        fn new(name: &'static str, result: &'static str) -> Self {
            Self {
                definition: ToolDefinition::external(ToolSpec {
                    name: name.into(),
                    description: result.into(),
                    input_schema: json!({"type":"object"}),
                }),
                result,
            }
        }
    }

    impl ToolSource for Stub {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            vec![ToolRegistration::new(self.definition.clone(), self)]
        }
    }

    impl ToolExecutor for Stub {
        fn execute<'a>(
            &'a self,
            _invocation: crate::ToolInvocation<'a>,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async move {
                ToolOutput {
                    artifacts: Vec::new(),
                    value: json!(self.result),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn unavailable_deferred_registration_cannot_be_discovered_or_reactivated() {
        let mut unavailable = Stub::new("broken", "unavailable");
        unavailable.definition.exposure = ToolExposure::Deferred;
        unavailable.definition.spec.input_schema = json!({"$ref":"#/missing"});
        let previous = unavailable.definition.spec.clone();
        let mut deferred = Stub::new("working", "working");
        deferred.definition.exposure = ToolExposure::Deferred;
        let tools = ToolSet::new([
            Arc::new(unavailable) as Arc<dyn ToolSource>,
            Arc::new(deferred) as Arc<dyn ToolSource>,
        ]);
        let catalog = tools.snapshot_with_previous(&[previous]);
        assert!(!catalog.is_declared("broken"));
        assert!(catalog.is_declared(TOOL_SEARCH_NAME));
        assert!(
            catalog
                .declared_specs()
                .iter()
                .all(|spec| spec.name != "broken")
        );
        let call = ToolCall {
            id: "find".into(),
            name: TOOL_SEARCH_NAME.into(),
            arguments: json!({"query":"broken working"}),
            raw_arguments: None,
        };
        let output = catalog
            .execute(crate::ToolInvocation::untracked(
                &call,
                CancellationToken::new(),
            ))
            .await;
        let results = output.value["loaded"].as_array().expect("search result");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["name"], "working");
    }

    struct ChangingHost(AtomicBool);

    impl ToolSource for ChangingHost {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            let (name, result) = if self.0.load(Ordering::Acquire) {
                ("new", "changing:new")
            } else {
                ("read", "changing:read")
            };
            vec![ToolRegistration::new(
                ToolDefinition::external(ToolSpec {
                    name: name.into(),
                    description: "changing".into(),
                    input_schema: json!({"type":"object"}),
                }),
                Arc::new(Stub::new(name, result)),
            )]
        }
    }

    #[tokio::test]
    async fn replacement_preserves_unrelated_tools_and_frozen_routes() {
        let changing = Arc::new(ChangingHost(AtomicBool::new(false)));
        let tools = ToolSet::new([
            Arc::new(Stub::new("read", "builtin")) as Arc<dyn ToolSource>,
            Arc::new(Stub::new("custom", "added")),
            changing.clone(),
        ]);
        let before = tools.snapshot();
        assert_eq!(before.specs().len(), 2);
        assert_eq!(
            before.definition("read").unwrap().spec.description,
            "changing"
        );

        changing.0.store(true, Ordering::Release);
        let after = tools.snapshot();
        assert_eq!(after.specs().len(), 3);
        assert_eq!(
            after.definition("read").unwrap().spec.description,
            "builtin"
        );
        assert!(after.definition("new").is_some());

        let call = |name: &str| ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: json!({}),
            raw_arguments: None,
        };
        for catalog in [&before, &after] {
            assert_eq!(
                catalog
                    .execute(crate::ToolInvocation::untracked(
                        &call("custom"),
                        CancellationToken::new()
                    ))
                    .await
                    .value,
                json!("added"),
            );
        }
        assert_eq!(
            before
                .execute(crate::ToolInvocation::untracked(
                    &call("read"),
                    CancellationToken::new()
                ))
                .await
                .value,
            json!("changing:read")
        );
        assert_eq!(
            after
                .execute(crate::ToolInvocation::untracked(
                    &call("read"),
                    CancellationToken::new()
                ))
                .await
                .value,
            json!("builtin")
        );
        assert_eq!(
            after
                .execute(crate::ToolInvocation::untracked(
                    &call("new"),
                    CancellationToken::new()
                ))
                .await
                .value,
            json!("changing:new")
        );
    }

    struct ChangingSchema(AtomicBool);
    impl ToolSource for ChangingSchema {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            let version = if self.0.load(Ordering::Acquire) { 2 } else { 1 };
            let mut stub = Stub::new("versioned", "observed");
            stub.definition.spec.input_schema = json!({
                "type":"object", "required":["version"],
                "properties":{"version":{"const":version}}
            });
            stub.definition.exposure = ToolExposure::Deferred;
            Arc::new(stub).registrations()
        }
    }

    #[tokio::test]
    async fn schema_contract_stays_with_frozen_route_through_refresh_and_exposure_rebuild() {
        let source = Arc::new(ChangingSchema(AtomicBool::new(false)));
        let tools = ToolSet::new([source.clone() as Arc<dyn ToolSource>]);
        let issued = tools.snapshot();
        let declared = issued.with_previous(&issued.specs());
        source.0.store(true, Ordering::Release);
        let refreshed = tools.snapshot();
        for (catalog, accepted) in [(&issued, 1), (&declared, 1), (&refreshed, 2)] {
            for version in [1, 2] {
                let call = ToolCall {
                    id: "version".into(),
                    name: "versioned".into(),
                    arguments: json!({"version":version}),
                    raw_arguments: None,
                };
                let output = catalog
                    .execute(crate::ToolInvocation::untracked(
                        &call,
                        CancellationToken::new(),
                    ))
                    .await;
                assert_eq!(!output.is_error, version == accepted, "{}", output.value);
            }
        }
        assert!(declared.is_declared("versioned"));
        assert!(!refreshed.is_declared("versioned"));
    }

    #[tokio::test]
    async fn deferred_capability_is_callable_but_not_model_declared() {
        let catalog = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>]).snapshot();
        assert_eq!(
            catalog
                .declared_specs()
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["direct", TOOL_SEARCH_NAME]
        );
        assert!(!catalog.is_declared("deferred"));

        let call = ToolCall {
            id: "1".into(),
            name: "deferred".into(),
            arguments: json!({}),
            raw_arguments: None,
        };
        assert_eq!(
            catalog
                .execute(crate::ToolInvocation::untracked(
                    &call,
                    CancellationToken::new()
                ))
                .await
                .value,
            json!("deferred")
        );
        let ToolDispatch::Rejected(model_result) = catalog
            .execute_model_call(crate::ToolInvocation::untracked(
                &call,
                CancellationToken::new(),
            ))
            .await
        else {
            panic!("an undeclared model call must be rejected");
        };
        assert_eq!(
            model_result,
            "tool was not declared for this request: deferred"
        );
    }

    #[tokio::test]
    async fn tool_search_loads_matching_deferred_capabilities_for_next_request() {
        let catalog = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>]).snapshot();
        let call = ToolCall {
            id: "search".into(),
            name: TOOL_SEARCH_NAME.into(),
            arguments: json!({"query":"deferred"}),
            raw_arguments: None,
        };
        let ToolDispatch::Execution(execution) = catalog
            .execute_model_call(crate::ToolInvocation::untracked(
                &call,
                CancellationToken::new(),
            ))
            .await
        else {
            panic!("tool search cannot select the composition gateway");
        };
        let ToolExecution::Observed { output, activate } = execution.await else {
            panic!("search was rejected")
        };
        assert!(!output.is_error);
        assert_eq!(activate[0].name, "deferred");
        assert_eq!(output.value["count"], 1);

        let names = catalog
            .context_after_discovery(&activate)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["direct", "deferred"]);
    }

    struct PendingSource(tokio::sync::watch::Receiver<Option<Arc<Stub>>>);

    impl ToolSource for PendingSource {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            self.0
                .borrow()
                .clone()
                .map_or_else(Vec::new, |stub| stub.registrations())
        }

        fn deferred_pending(&self) -> bool {
            self.0.borrow().is_none()
        }

        fn wait_for_deferred(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let _ = self.0.clone().wait_for(Option::is_some).await;
            })
        }
    }

    #[tokio::test]
    async fn late_discovery_persists_an_exact_spec_without_granting_a_route() {
        let (inventory, receive) = tokio::sync::watch::channel(None);
        let tools = ToolSet::new([Arc::new(PendingSource(receive)) as Arc<dyn ToolSource>]);
        let fixed = tools.snapshot();
        let issued = tools.request_catalog(&[]);
        assert!(!fixed.is_declared(TOOL_SEARCH_NAME));
        assert!(issued.is_declared(TOOL_SEARCH_NAME));
        let call = ToolCall {
            id: "search".into(),
            name: TOOL_SEARCH_NAME.into(),
            arguments: json!({"query":"lookup"}),
            raw_arguments: None,
        };
        let stop = CancellationToken::new();
        let ToolDispatch::Execution(wait) = issued
            .execute_model_call(crate::ToolInvocation::untracked(&call, stop.clone()))
            .await
        else {
            panic!("expected search execution");
        };
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        stop.cancel();
        assert!(matches!(wait.await, ToolExecution::NotDispatched { .. }));

        let mut lookup = Stub::new("lookup", "original");
        lookup.definition.exposure = ToolExposure::Deferred;
        let spec = lookup.definition.spec.clone();
        inventory.send_replace(Some(Arc::new(lookup)));
        let ToolDispatch::Execution(search) = issued
            .execute_model_call(crate::ToolInvocation::untracked(
                &call,
                CancellationToken::new(),
            ))
            .await
        else {
            panic!("expected search execution");
        };
        let ToolExecution::Observed { activate, .. } = search.await else {
            panic!("expected discovery result");
        };
        assert_eq!(activate, [spec]);
        assert!(issued.definition("lookup").is_none());
        assert!(
            fixed
                .with_previous(&activate)
                .definition("lookup")
                .is_none()
        );
        let saved = issued.context_after_discovery(&activate);
        assert!(tools.request_catalog(&saved).is_declared("lookup"));

        let mut changed = Stub::new("lookup", "replacement");
        changed.definition.exposure = ToolExposure::Deferred;
        inventory.send_replace(Some(Arc::new(changed)));
        assert!(!tools.request_catalog(&saved).is_declared("lookup"));
    }

    #[test]
    fn compatible_deferred_loadout_restores_but_redefinition_does_not() {
        let tools = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>]);
        let initial = tools.snapshot();
        let deferred = initial
            .specs()
            .into_iter()
            .find(|spec| spec.name == "deferred")
            .unwrap();

        let restored = tools.snapshot_with_previous(std::slice::from_ref(&deferred));
        assert!(restored.is_declared("direct"));
        assert!(restored.is_declared("deferred"));

        let mut changed = deferred;
        changed.description.push_str(" changed");
        let incompatible = tools.snapshot_with_previous(&[changed]);
        assert!(incompatible.is_declared("direct"));
        assert!(!incompatible.is_declared("deferred"));
    }

    struct MixedForRestore;

    impl ToolSource for MixedForRestore {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            let definitions = vec![
                ToolDefinition::external(ToolSpec {
                    name: "direct".into(),
                    description: "direct".into(),
                    input_schema: json!({"type":"object"}),
                }),
                ToolDefinition::external(ToolSpec {
                    name: "deferred".into(),
                    description: "deferred".into(),
                    input_schema: json!({"type":"object"}),
                })
                .deferred(),
            ];
            definitions
                .into_iter()
                .map(|definition| ToolRegistration::new(definition, self.clone()))
                .collect()
        }
    }

    impl ToolExecutor for MixedForRestore {
        fn execute<'a>(
            &'a self,
            invocation: crate::ToolInvocation<'a>,
        ) -> BoxFuture<'a, ToolOutput> {
            let call = invocation.call();
            Box::pin(async move {
                ToolOutput {
                    artifacts: Vec::new(),
                    value: json!(call.name),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[test]
    fn selection_patterns_and_exclusions_narrow_the_model_loadout() {
        assert!(tool_name_matches("*a", "*ba"));
        assert!(tool_name_matches("read", "read"));
        assert!(tool_name_matches("mcp__*", "mcp__8_demo__probe"));
        assert!(tool_name_matches("*__probe", "mcp__8_demo__probe"));
        assert!(tool_name_matches("*probe*", "mcp__8_demo__probe"));
        assert!(!tool_name_matches("read", "write"));

        let included = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>])
            .with_selection(ToolSelection::include(["direct".into()]))
            .snapshot();
        assert!(included.is_declared("direct"));
        assert!(!included.is_declared("deferred"));
        assert!(!included.is_declared("tool_search"));

        let excluded = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>])
            .with_selection(ToolSelection::all().with_exclude(["deferred".into()]))
            .snapshot();
        assert!(excluded.is_declared("direct"));
        assert!(!excluded.is_declared("deferred"));
        assert!(!excluded.is_declared("tool_search"));

        let without_search = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>])
            .with_selection(
                ToolSelection::include(["direct".into(), "deferred".into()])
                    .with_exclude(["tool_search".into()]),
            )
            .snapshot();
        assert!(without_search.is_declared("direct"));
        assert!(!without_search.is_declared("deferred"));
        assert!(!without_search.is_declared("tool_search"));
    }

    #[tokio::test]
    async fn history_is_explicit_selected_and_never_resolves_an_untracked_session() {
        let plain = ToolSet::new([]).snapshot();
        assert!(plain.definition("history").is_none());
        let catalog = ToolSet::new([]).with_history().snapshot();
        assert!(catalog.is_declared("history"));
        // Guest-callable rather than an opaque intrinsic: Code Mode's frozen
        // namespace can retrieve history, while tool_search/code_mode stay out.
        assert!(!catalog.is_intrinsic("history"));
        let excluded = ToolSet::new([])
            .with_history()
            .with_selection(ToolSelection::include(["read".into()]))
            .snapshot();
        assert!(excluded.definition("history").is_none());
        let call = ToolCall {
            id: "history".into(),
            name: "history".into(),
            arguments: json!({"action":"list"}),
            raw_arguments: None,
        };
        let output = catalog
            .execute(crate::ToolInvocation::untracked(
                &call,
                CancellationToken::new(),
            ))
            .await;
        assert!(output.is_error);
        assert!(output.value["error"].as_str().unwrap().contains("Session"));
    }

    #[test]
    fn activity_resolves_a_semantic_subject_without_terminal_formatting() {
        let mut source = Stub::new("read", "builtin");
        source.definition.presentation = ToolPresentation::argument(ToolActivityKind::Read, "path");
        let catalog = ToolSet::new([Arc::new(source) as Arc<dyn ToolSource>]).snapshot();
        let activity = catalog.activity(&ToolCall {
            id: "1".into(),
            name: "read".into(),
            arguments: json!({"path":"src/main.rs"}),
            raw_arguments: None,
        });
        assert_eq!(activity.kind, ToolActivityKind::Read);
        assert_eq!(activity.subject.as_deref(), Some("src/main.rs"));
    }
}

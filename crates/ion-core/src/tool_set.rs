//! Model-visible tool definitions, semantic presentation, and request-bound routing.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::tool_result::ToolOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityKind {
    Read,
    List,
    Search,
    Edit,
    Write,
    Command,
    Ask,
    Subagent,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPresentationTarget {
    None,
    ToolName,
    Argument(String),
    Static(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPresentation {
    pub kind: ToolActivityKind,
    pub target: ToolPresentationTarget,
}

impl ToolPresentation {
    pub fn argument(kind: ToolActivityKind, key: impl Into<String>) -> Self {
        Self {
            kind,
            target: ToolPresentationTarget::Argument(key.into()),
        }
    }

    pub fn static_target(kind: ToolActivityKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: ToolPresentationTarget::Static(target.into()),
        }
    }

    pub fn external() -> Self {
        Self {
            kind: ToolActivityKind::External,
            target: ToolPresentationTarget::ToolName,
        }
    }

    fn resolve(&self, call: &ToolCall) -> ToolActivity {
        let subject = match &self.target {
            ToolPresentationTarget::None => None,
            ToolPresentationTarget::ToolName => Some(call.name.clone()),
            ToolPresentationTarget::Argument(key) => call
                .arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned),
            ToolPresentationTarget::Static(value) => Some(value.clone()),
        };
        ToolActivity {
            kind: self.kind,
            subject,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolActivity {
    pub kind: ToolActivityKind,
    pub subject: Option<String>,
}

impl ToolActivity {
    pub fn external(name: impl Into<String>) -> Self {
        Self {
            kind: ToolActivityKind::External,
            subject: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExposure {
    /// Declared directly to the model for this request.
    Direct,
    /// Callable by the harness but omitted from the default model loadout.
    Deferred,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub spec: ToolSpec,
    pub presentation: ToolPresentation,
    pub exposure: ToolExposure,
}

impl ToolDefinition {
    pub fn external(spec: ToolSpec) -> Self {
        Self {
            spec,
            presentation: ToolPresentation::external(),
            exposure: ToolExposure::Direct,
        }
    }

    pub fn deferred(mut self) -> Self {
        self.exposure = ToolExposure::Deferred;
        self
    }
}

#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub output: ToolOutput,
    /// Deferred capabilities to add to the next request's declared loadout.
    pub activate: Vec<String>,
}

impl ToolExecution {
    fn output(output: ToolOutput) -> Self {
        Self {
            output,
            activate: Vec::new(),
        }
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
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput>;
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

    fn refresh<'a>(&'a self, _stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async { Vec::new() })
    }
}

/// Composition owner for independent sources. Later registrations replace
/// earlier ones with the same model-visible name.
pub struct ToolSet {
    sources: Vec<Arc<dyn ToolSource>>,
}

impl ToolSet {
    pub fn new(sources: impl IntoIterator<Item = Arc<dyn ToolSource>>) -> Self {
        Self {
            sources: sources.into_iter().collect(),
        }
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
        let mut entries: Vec<RoutedTool> = Vec::new();
        let mut positions = HashMap::new();
        for source in &self.sources {
            for registration in source.clone().registrations() {
                let name = registration.definition.spec.name.clone();
                let routed = RoutedTool {
                    definition: registration.definition,
                    route: ToolRoute::Executor(registration.executor),
                };
                match positions.get(&name).copied() {
                    Some(index) => entries[index] = routed,
                    None => {
                        positions.insert(name, entries.len());
                        entries.push(routed);
                    }
                }
            }
        }
        let previous = previous
            .iter()
            .map(|spec| (spec.name.as_str(), spec))
            .collect::<HashMap<_, _>>();
        let mut declared = entries
            .iter()
            .filter(|entry| {
                entry.definition.exposure == ToolExposure::Direct
                    || previous
                        .get(entry.definition.spec.name.as_str())
                        .is_some_and(|spec| **spec == entry.definition.spec)
            })
            .map(|entry| entry.definition.spec.name.clone())
            .collect::<HashSet<_>>();

        if entries.iter().any(|entry| {
            entry.definition.exposure == ToolExposure::Deferred
                && !declared.contains(&entry.definition.spec.name)
        }) {
            let definition = tool_search_definition();
            let name = definition.spec.name.clone();
            let routed = RoutedTool {
                definition,
                route: ToolRoute::Search,
            };
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
            entries,
            positions,
            declared,
        }
    }
}

#[derive(Clone)]
enum ToolRoute {
    Executor(Arc<dyn ToolExecutor>),
    Search,
}

#[derive(Clone)]
struct RoutedTool {
    definition: ToolDefinition,
    route: ToolRoute,
}

/// Immutable tool schema/presentation/route view used by one model request and
/// the tool calls produced from that request.
pub struct ToolCatalog {
    entries: Vec<RoutedTool>,
    positions: HashMap<String, usize>,
    declared: HashSet<String>,
}

impl ToolCatalog {
    /// Every callable definition in this frozen catalog, including deferred
    /// capabilities that are not declared directly to the model.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.entries
            .iter()
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

    pub fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.positions
            .get(name)
            .and_then(|index| self.entries.get(*index))
            .map(|entry| &entry.definition)
    }

    /// Declared specs after additionally activating names returned by the
    /// intrinsic discovery tool. Unknown/non-deferred names are ignored.
    pub fn declared_specs_with(
        &self,
        additional: &std::collections::BTreeSet<String>,
    ) -> Vec<ToolSpec> {
        let search_needed = self.entries.iter().any(|entry| {
            entry.definition.exposure == ToolExposure::Deferred
                && !self.declared.contains(&entry.definition.spec.name)
                && !additional.contains(&entry.definition.spec.name)
        });
        self.entries
            .iter()
            .filter(|entry| match &entry.route {
                ToolRoute::Search => search_needed,
                ToolRoute::Executor(_) => {
                    self.declared.contains(&entry.definition.spec.name)
                        || (entry.definition.exposure == ToolExposure::Deferred
                            && additional.contains(&entry.definition.spec.name))
                }
            })
            .map(|entry| entry.definition.spec.clone())
            .collect()
    }

    pub fn activity(&self, call: &ToolCall) -> ToolActivity {
        self.definition(&call.name).map_or_else(
            || ToolActivity::external(&call.name),
            |definition| definition.presentation.resolve(call),
        )
    }

    /// Invoke a frozen capability, including a deferred one. This low-level
    /// operation does not admit a Turn or publish Session facts; coding execution
    /// must remain inside the Turn's commit/settlement boundary.
    pub fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        let execution = self.dispatch(call, stop);
        Box::pin(async move { execution.await.output })
    }

    /// Execute a model-issued call only when its definition was part of this
    /// request's declared loadout.
    pub(crate) fn execute_model_call<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolExecution> {
        if !self.is_declared(&call.name) {
            return Box::pin(async move {
                ToolExecution::output(ToolOutput {
                    value: serde_json::json!({
                        "error": format!("tool was not declared for this request: {}", call.name)
                    }),
                    images: Vec::new(),
                    is_error: true,
                })
            });
        }
        self.dispatch(call, stop)
    }

    fn dispatch<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolExecution> {
        match self
            .positions
            .get(&call.name)
            .and_then(|index| self.entries.get(*index))
        {
            Some(RoutedTool {
                route: ToolRoute::Search,
                ..
            }) => {
                let execution = self.search_deferred(call);
                Box::pin(async move { execution })
            }
            Some(RoutedTool {
                route: ToolRoute::Executor(executor),
                ..
            }) => {
                let future = executor.execute(call, stop);
                Box::pin(async move { ToolExecution::output(future.await) })
            }
            None => Box::pin(async move {
                ToolExecution::output(ToolOutput {
                    value: serde_json::json!({"error":format!("unknown tool: {}",call.name)}),
                    images: Vec::new(),
                    is_error: true,
                })
            }),
        }
    }

    fn search_deferred(&self, call: &ToolCall) -> ToolExecution {
        let input: ToolSearchInput = match serde_json::from_value(call.arguments.clone()) {
            Ok(input) => input,
            Err(error) => {
                return ToolExecution::output(ToolOutput {
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
                value: serde_json::json!({"error":"tool_search query is empty"}),
                images: Vec::new(),
                is_error: true,
            });
        }
        let limit = input.limit.unwrap_or(DEFAULT_TOOL_SEARCH_LIMIT);
        if !(1..=MAX_TOOL_SEARCH_LIMIT).contains(&limit) {
            return ToolExecution::output(ToolOutput {
                value: serde_json::json!({
                    "error": format!("tool_search limit must be between 1 and {MAX_TOOL_SEARCH_LIMIT}")
                }),
                images: Vec::new(),
                is_error: true,
            });
        }

        let tokens = query.split_whitespace().collect::<Vec<_>>();
        let mut matches = self
            .entries
            .iter()
            .filter(|entry| {
                entry.definition.exposure == ToolExposure::Deferred
                    && !self.declared.contains(&entry.definition.spec.name)
            })
            .filter_map(|entry| {
                let name = entry.definition.spec.name.to_ascii_lowercase();
                let description = entry.definition.spec.description.to_ascii_lowercase();
                let mut score = 0usize;
                if name == query {
                    score += 10_000;
                } else if name.contains(&query) {
                    score += 2_000;
                }
                if description.contains(&query) {
                    score += 500;
                }
                for token in &tokens {
                    if name == *token {
                        score += 1_000;
                    } else if name.contains(token) {
                        score += 250;
                    }
                    if description.contains(token) {
                        score += 50;
                    }
                }
                (score > 0).then_some((score, entry))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.definition.spec.name.cmp(&right.definition.spec.name))
        });
        matches.truncate(limit);

        let activate = matches
            .iter()
            .map(|(_, entry)| entry.definition.spec.name.clone())
            .collect::<Vec<_>>();
        let tools = matches
            .iter()
            .map(|(_, entry)| {
                serde_json::json!({
                    "name": entry.definition.spec.name,
                    "description": entry.definition.spec.description,
                })
            })
            .collect::<Vec<_>>();
        ToolExecution {
            output: ToolOutput {
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

    struct Stub(&'static str, &'static str);

    impl ToolSource for Stub {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            let definitions = {
                vec![ToolDefinition::external(ToolSpec {
                    name: self.0.into(),
                    description: self.1.into(),
                    input_schema: json!({"type":"object"}),
                })]
            };
            definitions
                .into_iter()
                .map(|definition| ToolRegistration::new(definition, self.clone()))
                .collect()
        }
    }

    impl ToolExecutor for Stub {
        fn execute<'a>(
            &'a self,
            _call: &'a ToolCall,
            _stop: CancellationToken,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async move {
                ToolOutput {
                    value: json!(self.1),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn later_tool_replaces_one_name_without_removing_others() {
        let sources: Vec<Arc<dyn ToolSource>> = vec![
            Arc::new(Stub("read", "builtin")),
            Arc::new(Stub("custom", "added")),
            Arc::new(Stub("read", "override")),
        ];
        let tools = ToolSet::new(sources);
        let catalog = tools.snapshot();
        assert_eq!(catalog.specs().len(), 2);
        assert_eq!(catalog.specs()[0].description, "override");
        let result = catalog
            .execute(
                &ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    arguments: json!({}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.value, json!("override"));
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
                Arc::new(Stub(name, result)),
            )]
        }
    }

    #[tokio::test]
    async fn old_catalog_keeps_the_route_that_was_advertised() {
        let changing = Arc::new(ChangingHost(AtomicBool::new(false)));
        let tools = ToolSet::new([
            Arc::new(Stub("read", "builtin")) as Arc<dyn ToolSource>,
            changing.clone(),
        ]);
        let before = tools.snapshot();
        assert_eq!(before.specs().len(), 1);
        assert_eq!(before.specs()[0].description, "changing");

        changing.0.store(true, Ordering::Release);
        let after = tools.snapshot();
        assert_eq!(after.specs().len(), 2);
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
        assert_eq!(
            before
                .execute(&call("read"), CancellationToken::new())
                .await
                .value,
            json!("changing:read")
        );
        assert_eq!(
            after
                .execute(&call("read"), CancellationToken::new())
                .await
                .value,
            json!("builtin")
        );
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
            catalog.execute(&call, CancellationToken::new()).await.value,
            json!("deferred")
        );
        let model_result = catalog
            .execute_model_call(&call, CancellationToken::new())
            .await
            .output;
        assert!(model_result.is_error);
        assert_eq!(
            model_result.value["error"],
            "tool was not declared for this request: deferred"
        );
    }

    #[tokio::test]
    async fn tool_search_loads_matching_deferred_capabilities_for_next_request() {
        let catalog = ToolSet::new([Arc::new(MixedForRestore) as Arc<dyn ToolSource>]).snapshot();
        let execution = catalog
            .execute_model_call(
                &ToolCall {
                    id: "search".into(),
                    name: TOOL_SEARCH_NAME.into(),
                    arguments: json!({"query":"deferred"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(!execution.output.is_error);
        assert_eq!(execution.activate, ["deferred"]);
        assert_eq!(execution.output.value["count"], 1);

        let activate = execution.activate.into_iter().collect();
        let names = catalog
            .declared_specs_with(&activate)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["direct", "deferred"]);
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
            let definitions = {
                vec![
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
                ]
            };
            definitions
                .into_iter()
                .map(|definition| ToolRegistration::new(definition, self.clone()))
                .collect()
        }
    }

    impl ToolExecutor for MixedForRestore {
        fn execute<'a>(
            &'a self,
            call: &'a ToolCall,
            _stop: CancellationToken,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async move {
                ToolOutput {
                    value: json!(call.name),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[test]
    fn activity_resolves_a_semantic_subject_without_terminal_formatting() {
        let tools = ToolSet::new([Arc::new(Stub("read", "builtin")) as Arc<dyn ToolSource>]);
        let mut catalog = tools.snapshot();
        catalog.entries[0].definition.presentation =
            ToolPresentation::argument(ToolActivityKind::Read, "path");
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

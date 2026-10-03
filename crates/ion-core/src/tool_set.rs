//! Model-visible tool definitions, semantic presentation, and request-bound routing.
use std::{collections::HashMap, sync::Arc};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

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
pub struct ToolOutput {
    pub value: Value,
    pub images: Vec<ion_ai::ImageContent>,
    pub is_error: bool,
}

/// One owner of one or more tools. A host's definitions must remain coherent
/// until the next explicit refresh boundary.
pub trait ToolHost: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;

    fn refresh_definitions<'a>(&'a self, _stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async { Vec::new() })
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput>;
}

/// Composition owner for independent tool hosts. Later hosts replace earlier
/// definitions with the same model-visible name.
pub struct ToolSet {
    hosts: Vec<Arc<dyn ToolHost>>,
}

impl ToolSet {
    pub fn new(hosts: impl IntoIterator<Item = Arc<dyn ToolHost>>) -> Self {
        Self {
            hosts: hosts.into_iter().collect(),
        }
    }

    pub async fn refresh(&self, stop: CancellationToken) -> Vec<String> {
        let mut diagnostics = Vec::new();
        for host in &self.hosts {
            if stop.is_cancelled() {
                break;
            }
            diagnostics.extend(host.refresh_definitions(stop.clone()).await);
        }
        diagnostics
    }

    /// Freeze one coherent model-request view. The returned catalog owns the
    /// exact host route selected for every advertised definition.
    pub fn snapshot(&self) -> ToolCatalog {
        let mut entries: Vec<RoutedTool> = Vec::new();
        let mut positions = HashMap::new();
        for host in &self.hosts {
            for definition in host.definitions() {
                let name = definition.spec.name.clone();
                let routed = RoutedTool {
                    definition,
                    host: host.clone(),
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
        ToolCatalog { entries, positions }
    }
}

#[derive(Clone)]
struct RoutedTool {
    definition: ToolDefinition,
    host: Arc<dyn ToolHost>,
}

/// Immutable tool schema/presentation/route view used by one model request and
/// the tool calls produced from that request.
pub struct ToolCatalog {
    entries: Vec<RoutedTool>,
    positions: HashMap<String, usize>,
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
            .filter(|entry| entry.definition.exposure == ToolExposure::Direct)
            .map(|entry| entry.definition.spec.clone())
            .collect()
    }

    pub fn is_declared(&self, name: &str) -> bool {
        self.definition(name)
            .is_some_and(|definition| definition.exposure == ToolExposure::Direct)
    }

    pub fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.positions
            .get(name)
            .and_then(|index| self.entries.get(*index))
            .map(|entry| &entry.definition)
    }

    pub fn activity(&self, call: &ToolCall) -> ToolActivity {
        self.definition(&call.name).map_or_else(
            || ToolActivity::external(&call.name),
            |definition| definition.presentation.resolve(call),
        )
    }

    /// Execute any callable capability in this frozen catalog. Harness-side
    /// discovery/orchestration may use this for deferred tools.
    pub fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        match self
            .positions
            .get(&call.name)
            .and_then(|index| self.entries.get(*index))
        {
            Some(entry) => entry.host.execute(call, stop),
            None => Box::pin(async move {
                ToolOutput {
                    value: serde_json::json!({"error":format!("unknown tool: {}",call.name)}),
                    images: Vec::new(),
                    is_error: true,
                }
            }),
        }
    }

    /// Execute a model-issued call only when its definition was part of this
    /// request's declared loadout.
    pub fn execute_declared<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        if self.is_declared(&call.name) {
            return self.execute(call, stop);
        }
        Box::pin(async move {
            ToolOutput {
                value: serde_json::json!({
                    "error": format!("tool was not declared for this request: {}", call.name)
                }),
                images: Vec::new(),
                is_error: true,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Stub(&'static str, &'static str);

    impl ToolHost for Stub {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition::external(ToolSpec {
                name: self.0.into(),
                description: self.1.into(),
                input_schema: json!({"type":"object"}),
            })]
        }

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
        let hosts: Vec<Arc<dyn ToolHost>> = vec![
            Arc::new(Stub("read", "builtin")),
            Arc::new(Stub("custom", "added")),
            Arc::new(Stub("read", "override")),
        ];
        let tools = ToolSet::new(hosts);
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

    impl ToolHost for ChangingHost {
        fn definitions(&self) -> Vec<ToolDefinition> {
            let name = if self.0.load(Ordering::Acquire) {
                "new"
            } else {
                "read"
            };
            vec![ToolDefinition::external(ToolSpec {
                name: name.into(),
                description: "changing".into(),
                input_schema: json!({"type":"object"}),
            })]
        }

        fn execute<'a>(
            &'a self,
            call: &'a ToolCall,
            _stop: CancellationToken,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async move {
                ToolOutput {
                    value: json!(format!("changing:{}", call.name)),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn old_catalog_keeps_the_route_that_was_advertised() {
        let changing = Arc::new(ChangingHost(AtomicBool::new(false)));
        let tools = ToolSet::new([
            Arc::new(Stub("read", "builtin")) as Arc<dyn ToolHost>,
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
        struct Mixed;

        impl ToolHost for Mixed {
            fn definitions(&self) -> Vec<ToolDefinition> {
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
            }

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

        let catalog = ToolSet::new([Arc::new(Mixed) as Arc<dyn ToolHost>]).snapshot();
        assert_eq!(catalog.specs().len(), 2);
        assert_eq!(
            catalog
                .declared_specs()
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["direct"]
        );

        let call = ToolCall {
            id: "1".into(),
            name: "deferred".into(),
            arguments: json!({}),
            raw_arguments: None,
        };
        assert_eq!(
            catalog
                .execute(&call, CancellationToken::new())
                .await
                .value,
            json!("deferred")
        );
        let model_result = catalog
            .execute_declared(&call, CancellationToken::new())
            .await;
        assert!(model_result.is_error);
        assert_eq!(
            model_result.value["error"],
            "tool was not declared for this request: deferred"
        );
    }

    #[test]
    fn activity_resolves_a_semantic_subject_without_terminal_formatting() {
        let tools = ToolSet::new([Arc::new(Stub("read", "builtin")) as Arc<dyn ToolHost>]);
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

//! A single model-visible tool set assembled from independent tool owners.
use std::{collections::HashMap, sync::Arc};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use tokio_util::sync::CancellationToken;

use crate::agent::{ToolHost, ToolOutput};

/// Later hosts replace earlier tools with the same name. This lets an
/// embedder override a built-in deliberately while preserving other tools.
pub struct ToolSet {
    hosts: Vec<Arc<dyn ToolHost>>,
}

impl ToolSet {
    pub fn new(hosts: impl IntoIterator<Item = Arc<dyn ToolHost>>) -> Self {
        Self {
            hosts: hosts.into_iter().collect(),
        }
    }
}

impl ToolHost for ToolSet {
    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        let mut positions = HashMap::new();
        for host in &self.hosts {
            for spec in host.specs() {
                match positions.get(&spec.name) {
                    Some(&index) => specs[index] = spec,
                    None => {
                        positions.insert(spec.name.clone(), specs.len());
                        specs.push(spec);
                    }
                }
            }
        }
        specs
    }

    fn refresh_specs<'a>(&'a self, stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            let mut diagnostics = Vec::new();
            for host in &self.hosts {
                if stop.is_cancelled() {
                    break;
                }
                diagnostics.extend(host.refresh_specs(stop.clone()).await);
            }
            diagnostics
        })
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        match self
            .hosts
            .iter()
            .rev()
            .find(|host| host.specs().iter().any(|spec| spec.name == call.name))
        {
            Some(host) => host.execute(call, stop),
            None => Box::pin(async move {
                ToolOutput {
                    value: serde_json::json!({"error":format!("unknown tool: {}",call.name)}),
                    images: Vec::new(),
                    is_error: true,
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Stub(&'static str, &'static str);
    impl ToolHost for Stub {
        fn specs(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: self.0.into(),
                description: self.1.into(),
                input_schema: json!({"type":"object"}),
            }]
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
        assert_eq!(tools.specs().len(), 2);
        assert_eq!(tools.specs()[0].description, "override");
        let result = tools
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
        fn specs(&self) -> Vec<ToolSpec> {
            let name = if self.0.load(Ordering::Acquire) {
                "new"
            } else {
                "read"
            };
            vec![ToolSpec {
                name: name.into(),
                description: "changing".into(),
                input_schema: json!({"type":"object"}),
            }]
        }

        fn execute<'a>(
            &'a self,
            _call: &'a ToolCall,
            _stop: CancellationToken,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async {
                ToolOutput {
                    value: json!("changing"),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn refreshed_host_snapshot_changes_overrides_and_routes() {
        let changing = Arc::new(ChangingHost(AtomicBool::new(false)));
        let tools = ToolSet::new([
            Arc::new(Stub("read", "builtin")) as Arc<dyn ToolHost>,
            changing.clone(),
        ]);
        assert_eq!(tools.specs().len(), 1);
        changing.0.store(true, Ordering::Release);
        let specs = tools.specs();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].description, "builtin");
        assert_eq!(specs[1].name, "new");
        let call = |name: &str| ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: json!({}),
            raw_arguments: None,
        };
        assert_eq!(
            tools
                .execute(&call("read"), CancellationToken::new())
                .await
                .value,
            json!("builtin")
        );
        assert_eq!(
            tools
                .execute(&call("new"), CancellationToken::new())
                .await
                .value,
            json!("changing")
        );
    }
}

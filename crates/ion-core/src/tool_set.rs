//! A single model-visible tool set assembled from independent tool owners.
use std::{collections::HashMap, sync::Arc};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use tokio_util::sync::CancellationToken;

use crate::agent::{ToolHost, ToolOutput};

/// Later hosts replace earlier tools with the same name. This lets an
/// embedder override a built-in deliberately while preserving other tools.
pub struct ToolSet {
    specs: Vec<ToolSpec>,
    routes: HashMap<String, Arc<dyn ToolHost>>,
}

impl ToolSet {
    pub fn new(hosts: impl IntoIterator<Item = Arc<dyn ToolHost>>) -> Self {
        let mut specs = Vec::<ToolSpec>::new();
        let mut routes = HashMap::new();
        for host in hosts {
            for spec in host.specs() {
                if let Some(index) = specs.iter().position(|known| known.name == spec.name) {
                    specs[index] = spec.clone();
                } else {
                    specs.push(spec.clone());
                }
                routes.insert(spec.name.clone(), host.clone());
            }
        }
        Self { specs, routes }
    }
}

impl ToolHost for ToolSet {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput> {
        match self.routes.get(&call.name) {
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
}

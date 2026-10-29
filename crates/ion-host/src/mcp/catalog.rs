//! Frozen MCP definitions, offline output contracts and catalogue publication.
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use ion_ai::ToolSpec;
use rmcp::model::Tool;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Client, schema::OutputSchema};

pub(super) struct RegisteredTool {
    pub spec: ToolSpec,
    pub original_name: String,
    pub output_schema: Option<Arc<OutputSchema>>,
}

pub(super) type RegisteredTools = Vec<RegisteredTool>;
pub(super) type Catalog = BTreeMap<String, RegisteredTools>;

#[derive(Default)]
pub(super) struct DiscoveredTools {
    pub tools: RegisteredTools,
    pub diagnostics: Vec<String>,
}

pub(super) fn publish_tools(
    catalog: &RwLock<Catalog>,
    server: &str,
    tools: RegisteredTools,
) -> Result<()> {
    let mut catalog = catalog
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut names: HashSet<_> = catalog
        .iter()
        .filter(|(name, _)| name.as_str() != server)
        .flat_map(|(_, tools)| tools.iter().map(|tool| tool.spec.name.as_str()))
        .collect();
    for tool in &tools {
        ensure!(
            names.insert(&tool.spec.name),
            "MCP server {server} has colliding tool name {:?} (alias {}) in the complete MCP source",
            tool.original_name,
            tool.spec.name
        );
    }
    catalog.insert(server.to_owned(), tools);
    Ok(())
}

pub(super) async fn fetch_tools(name: &str, client: &Client) -> Result<Vec<Tool>> {
    tokio::time::timeout(Duration::from_secs(10), client.list_all_tools())
        .await
        .with_context(|| format!("MCP server {name} tool listing timed out"))?
        .with_context(|| format!("MCP server {name} tool listing failed"))
}

/// CPU work runs on an owned blocking worker after the listing is received.
/// Unsupported contracts warn but never withdraw an otherwise available tool.
pub(super) fn register_tools(name: &str, tools: Vec<Tool>) -> Result<DiscoveredTools> {
    const MAX_WARNINGS: usize = 8;
    let mut discovered = DiscoveredTools::default();
    let mut unchecked = 0usize;
    for tool in tools {
        let tool_name = tool.name.to_string();
        ensure!(
            !tool_name.is_empty(),
            "MCP server {name} listed an empty tool name"
        );
        let exposed = exposed_tool_name(name, &tool_name);
        let output_schema = tool.output_schema.map(|schema| {
            Arc::new(OutputSchema::capture(Value::Object(Arc::unwrap_or_clone(
                schema,
            ))))
        });
        if let Some(reason) = output_schema
            .as_ref()
            .and_then(|schema| schema.unchecked_reason())
        {
            unchecked += 1;
            if discovered.diagnostics.len() < MAX_WARNINGS {
                discovered.diagnostics.push(format!(
                    "MCP server {name} tool {exposed}: output schema is unchecked: {reason}"
                ));
            }
        }
        let mut description = tool.description.map_or_else(
            || format!("Tool {tool_name} from MCP server {name}"),
            |description| description.into_owned(),
        );
        if output_schema.is_some() {
            description.push_str("\n\nThe result value reports output_schema_validation for the raw structured reply. Check error and delivery/truncation markers before using data. A failed or unchecked schema check does not authorize replay.");
        }
        discovered.tools.push(RegisteredTool {
            spec: ToolSpec {
                name: exposed,
                description,
                input_schema: Value::Object(Arc::unwrap_or_clone(tool.input_schema)),
            },
            original_name: tool_name,
            output_schema,
        });
    }
    if unchecked > MAX_WARNINGS {
        discovered.diagnostics.push(format!(
            "MCP server {name}: {} further tools have unchecked output schemas; tools remain available",
            unchecked - MAX_WARNINGS
        ));
    }
    Ok(discovered)
}

pub(super) fn exposed_tool_name(server: &str, original: &str) -> String {
    const MAX_NAME_BYTES: usize = 64;
    const HASH_HEX_BYTES: usize = 16;
    // A length-delimited server component cannot consume a tool's separator.
    let mut exposed = format!("mcp__{}_{server}__", server.len());
    exposed.extend(original.chars().map(|character| {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            character
        } else {
            '_'
        }
    }));
    if exposed.len() <= MAX_NAME_BYTES
        && original
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return exposed;
    }

    let mut hasher = Sha256::new();
    hasher.update(server.as_bytes());
    hasher.update([0]);
    hasher.update(original.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    exposed.truncate(MAX_NAME_BYTES - HASH_HEX_BYTES - 1);
    exposed.push('_');
    exposed.push_str(&hash[..HASH_HEX_BYTES]);
    exposed
}

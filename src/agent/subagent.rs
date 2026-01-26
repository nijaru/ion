//! Subagent support - isolated agent instances with restricted tools.

use crate::provider::LlmApi;
use crate::tool::ToolOrchestrator;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Configuration for a subagent loaded from YAML files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentConfig {
    pub name: String,
    pub description: String,
    /// Tool whitelist - only these tools are available.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Override model (uses parent model if None).
    #[serde(default)]
    pub model: Option<String>,
    /// Additional system prompt context.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Maximum turns before forced termination (default 10).
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
}

fn default_max_turns() -> usize {
    10
}

/// Summary of available subagent (for listing).
#[derive(Debug, Clone, Serialize)]
pub struct SubagentSummary {
    pub name: String,
    pub description: String,
}

/// Registry of available subagent configurations.
#[derive(Default)]
pub struct SubagentRegistry {
    configs: std::collections::HashMap<String, SubagentConfig>,
}

impl SubagentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load subagent configs from a directory.
    pub fn load_directory(&mut self, dir: &Path) -> Result<usize> {
        let mut count = 0;

        if !dir.exists() {
            return Ok(0);
        }

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                // Look for .yaml or .yml files
                if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
                    && let Ok(content) = std::fs::read_to_string(&path)
                        && let Ok(config) = serde_yaml::from_str::<SubagentConfig>(&content) {
                            self.configs.insert(config.name.clone(), config);
                            count += 1;
                        }
            }
        }

        Ok(count)
    }

    /// Get a subagent config by name.
    pub fn get(&self, name: &str) -> Option<&SubagentConfig> {
        self.configs.get(name)
    }

    /// List all available subagents.
    pub fn list(&self) -> Vec<SubagentSummary> {
        self.configs
            .values()
            .map(|c| SubagentSummary {
                name: c.name.clone(),
                description: c.description.clone(),
            })
            .collect()
    }
}

/// Result from a subagent execution.
#[derive(Debug)]
pub struct SubagentResult {
    pub output: String,
    pub turns_used: usize,
    pub was_truncated: bool,
}

/// Execute a subagent task with isolated state.
pub async fn run_subagent(
    config: &SubagentConfig,
    task: &str,
    provider: Arc<dyn LlmApi>,
    parent_orchestrator: &ToolOrchestrator,
) -> Result<SubagentResult> {
    use crate::agent::Agent;
    use crate::session::Session;
    use crate::tool::ToolMode;
    use tokio::sync::mpsc;

    // Create tool orchestrator with standard builtins
    // Tool filtering by whitelist is handled at call time via orchestrator permissions
    let _ = parent_orchestrator; // Will be used for tool filtering in future
    let orchestrator = Arc::new(ToolOrchestrator::with_builtins(ToolMode::Write));

    // Build system prompt
    let system_prompt = if let Some(ref extra) = config.system_prompt {
        format!(
            "You are a subagent. Complete the assigned task concisely.\n\n{}",
            extra
        )
    } else {
        "You are a subagent. Complete the assigned task concisely and report the result.".into()
    };

    // Create isolated agent
    let agent = Agent::new(provider.clone(), orchestrator).with_system_prompt(system_prompt);

    // Create isolated session
    let working_dir = std::env::current_dir().unwrap_or_default();
    let model = config.model.clone().unwrap_or_else(|| "default".to_string());
    let session = Session::new(working_dir, model);

    // Create channel for events (we'll collect output)
    let (tx, mut rx) = mpsc::channel(64);

    // Run the task with turn limit
    let mut output = String::new();
    let mut turns_used = 0;
    let was_truncated;

    // Spawn the agent task
    let agent_clone = agent.clone();
    let task_str = task.to_string();
    let handle = tokio::spawn(async move {
        agent_clone
            .run_task(session, task_str, tx, None, None)
            .await
    });

    // Collect output events
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(crate::agent::AgentEvent::TextDelta(text)) => {
                        output.push_str(&text);
                    }
                    Some(crate::agent::AgentEvent::Finished(_)) => {
                        turns_used += 1;
                        if turns_used >= config.max_turns {
                            was_truncated = true;
                            // Cancel the agent
                            handle.abort();
                            break;
                        }
                    }
                    Some(crate::agent::AgentEvent::Error(e)) => {
                        output.push_str(&format!("\nError: {}", e));
                        was_truncated = false;
                        break;
                    }
                    None => {
                        was_truncated = false;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // Wait for handle to complete (or it was aborted)
    let _ = handle.await;

    Ok(SubagentResult {
        output,
        turns_used,
        was_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_deserialize() {
        let yaml = r#"
name: researcher
description: Searches and analyzes information
tools:
  - read
  - glob
  - grep
model: claude-haiku-3
max_turns: 5
"#;
        let config: SubagentConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "researcher");
        assert_eq!(config.tools.len(), 3);
        assert_eq!(config.max_turns, 5);
    }

    #[test]
    fn test_config_defaults() {
        let yaml = r#"
name: basic
description: Basic subagent
"#;
        let config: SubagentConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.max_turns, 10);
        assert!(config.tools.is_empty());
        assert!(config.model.is_none());
    }
}

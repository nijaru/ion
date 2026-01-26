use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// YAML frontmatter structure per agentskills.io spec.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    models: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Option<Vec<String>>,
    /// Model configuration for this skill:
    /// - None/empty: inherit from main agent
    /// - Single model: use that model
    /// - Multiple models: first is default, others are allowed alternatives
    pub models: Option<Vec<String>>,
    pub prompt: String,
}

impl Skill {
    /// Get the model to use, falling back to the provided default
    pub fn resolve_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.models
            .as_ref()
            .and_then(|m| m.first())
            .map(|s| s.as_str())
            .unwrap_or(default)
    }

    /// Check if a model is allowed for this skill
    pub fn is_model_allowed(&self, model: &str) -> bool {
        match &self.models {
            None => true, // No restriction, any model allowed
            Some(models) if models.is_empty() => true,
            Some(models) => models.iter().any(|m| m == model),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

#[derive(Default, Clone)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, skill: Skill) {
        self.skills.insert(skill.name.clone(), skill);
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn list(&self) -> Vec<SkillSummary> {
        self.skills
            .values()
            .map(|s| SkillSummary {
                name: s.name.clone(),
                description: s.description.clone(),
            })
            .collect()
    }
}

pub struct SkillLoader;

impl SkillLoader {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Vec<Skill>> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read SKILL.md at {:?}", path.as_ref()))?;

        Self::parse_skill_md(&content)
    }

    /// Parse a SKILL.md file. Supports both YAML frontmatter and legacy XML format.
    pub fn parse_skill_md(content: &str) -> Result<Vec<Skill>> {
        let trimmed = content.trim_start();

        // YAML frontmatter format (agentskills.io spec)
        if trimmed.starts_with("---") {
            return Self::parse_yaml_format(content);
        }

        // Legacy XML format
        Self::parse_xml_format(content)
    }

    /// Parse YAML frontmatter format per agentskills.io spec.
    /// Format:
    /// ```text
    /// ---
    /// name: skill-name
    /// description: A description
    /// allowed-tools:
    ///   - Bash(git:*)
    ///   - Read
    /// ---
    /// Prompt content here...
    /// ```
    fn parse_yaml_format(content: &str) -> Result<Vec<Skill>> {
        let trimmed = content.trim_start();

        // Find the frontmatter boundaries
        let after_first = trimmed
            .strip_prefix("---")
            .context("Missing frontmatter start")?;
        let end_idx = after_first
            .find("\n---")
            .context("Missing frontmatter end delimiter")?;

        let yaml_content = &after_first[..end_idx];
        let prompt_content = after_first[end_idx + 4..].trim();

        // Parse YAML frontmatter
        let frontmatter: SkillFrontmatter =
            serde_yaml::from_str(yaml_content).context("Failed to parse skill YAML frontmatter")?;

        // Merge model/models fields
        let models = match (frontmatter.model, frontmatter.models) {
            (Some(m), None) => Some(vec![m]),
            (None, Some(ms)) => Some(ms),
            (Some(m), Some(mut ms)) => {
                ms.insert(0, m);
                Some(ms)
            }
            (None, None) => None,
        };

        let skill = Skill {
            name: frontmatter.name,
            description: frontmatter.description,
            allowed_tools: frontmatter.allowed_tools,
            models,
            prompt: prompt_content.to_string(),
        };

        Ok(vec![skill])
    }

    /// Parse legacy XML format for backwards compatibility.
    fn parse_xml_format(content: &str) -> Result<Vec<Skill>> {
        let mut skills = Vec::new();
        let mut current_skill: Option<Skill> = None;
        let mut in_prompt = false;

        for line in content.lines() {
            let trimmed = line.trim();

            // Look for skill header: <skill>
            if trimmed == "<skill>" {
                if let Some(skill) = current_skill.take() {
                    skills.push(skill);
                }
                current_skill = Some(Skill {
                    name: String::new(),
                    description: String::new(),
                    allowed_tools: None,
                    models: None,
                    prompt: String::new(),
                });
                in_prompt = false;
                continue;
            }

            if let Some(ref mut skill) = current_skill {
                if trimmed == "</skill>" {
                    skills.push(current_skill.take().unwrap());
                    continue;
                }

                if trimmed.starts_with("<name>") && trimmed.ends_with("</name>") {
                    skill.name = trimmed[6..trimmed.len() - 7].to_string();
                } else if trimmed.starts_with("<description>")
                    && trimmed.ends_with("</description>")
                {
                    skill.description = trimmed[13..trimmed.len() - 14].to_string();
                } else if trimmed.starts_with("<model>") && trimmed.ends_with("</model>") {
                    // Single model: <model>claude-sonnet-4</model>
                    let model = trimmed[7..trimmed.len() - 8].trim().to_string();
                    skill.models = Some(vec![model]);
                } else if trimmed.starts_with("<models>") && trimmed.ends_with("</models>") {
                    // Multiple models: <models>claude-sonnet-4, deepseek-v4</models>
                    let models_str = trimmed[8..trimmed.len() - 9].trim();
                    let models: Vec<String> = models_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !models.is_empty() {
                        skill.models = Some(models);
                    }
                } else if let Some(rest) = trimmed.strip_prefix("<prompt>") {
                    in_prompt = true;
                    if !rest.is_empty() {
                        skill.prompt.push_str(rest);
                        skill.prompt.push('\n');
                    }
                } else if let Some(rest) = trimmed.strip_suffix("</prompt>") {
                    if !rest.is_empty() {
                        skill.prompt.push_str(rest);
                    }
                    in_prompt = false;
                } else if in_prompt {
                    skill.prompt.push_str(line);
                    skill.prompt.push('\n');
                }
            }
        }

        if let Some(skill) = current_skill {
            skills.push(skill);
        }

        Ok(skills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_md() {
        let content = r#"
<skill>
    <name>test-skill</name>
    <description>A test skill</description>
    <prompt>
    You are a test agent.
    Do test things.
    </prompt>
</skill>
"#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "test-skill");
        assert_eq!(skills[0].description, "A test skill");
        assert!(skills[0].prompt.contains("You are a test agent."));
    }

    #[test]
    fn test_skill_single_model() {
        let content = r#"
<skill>
    <name>fast-skill</name>
    <description>Uses a specific model</description>
    <model>claude-sonnet-4</model>
    <prompt>Do fast things.</prompt>
</skill>
"#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(skills[0].models, Some(vec!["claude-sonnet-4".to_string()]));
        assert_eq!(skills[0].resolve_model("default"), "claude-sonnet-4");
        assert!(skills[0].is_model_allowed("claude-sonnet-4"));
        assert!(!skills[0].is_model_allowed("other-model"));
    }

    #[test]
    fn test_skill_multiple_models() {
        let content = r#"
<skill>
    <name>flexible-skill</name>
    <description>Allows multiple models</description>
    <models>claude-sonnet-4, deepseek-v4, gpt-4o</models>
    <prompt>Do flexible things.</prompt>
</skill>
"#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(
            skills[0].models,
            Some(vec![
                "claude-sonnet-4".to_string(),
                "deepseek-v4".to_string(),
                "gpt-4o".to_string()
            ])
        );
        // First model is default
        assert_eq!(skills[0].resolve_model("default"), "claude-sonnet-4");
        // All listed models are allowed
        assert!(skills[0].is_model_allowed("claude-sonnet-4"));
        assert!(skills[0].is_model_allowed("deepseek-v4"));
        assert!(skills[0].is_model_allowed("gpt-4o"));
        assert!(!skills[0].is_model_allowed("other-model"));
    }

    #[test]
    fn test_skill_inherit_model() {
        let content = r#"
<skill>
    <name>inherit-skill</name>
    <description>Inherits main model</description>
    <prompt>Do inherited things.</prompt>
</skill>
"#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(skills[0].models, None);
        assert_eq!(skills[0].resolve_model("main-model"), "main-model");
        assert!(skills[0].is_model_allowed("any-model"));
    }

    #[test]
    fn test_yaml_frontmatter_basic() {
        let content = r#"---
name: yaml-skill
description: A YAML formatted skill
---
You are an agent using YAML format.
Do YAML things."#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "yaml-skill");
        assert_eq!(skills[0].description, "A YAML formatted skill");
        assert!(skills[0]
            .prompt
            .contains("You are an agent using YAML format."));
    }

    #[test]
    fn test_yaml_frontmatter_with_allowed_tools() {
        let content = r#"---
name: restricted-skill
description: Has tool restrictions
allowed-tools:
  - Bash(git:*)
  - Read
  - Glob
---
You can only use git commands and read files."#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(skills[0].name, "restricted-skill");
        assert_eq!(
            skills[0].allowed_tools,
            Some(vec![
                "Bash(git:*)".to_string(),
                "Read".to_string(),
                "Glob".to_string()
            ])
        );
    }

    #[test]
    fn test_yaml_frontmatter_with_model() {
        let content = r#"---
name: fast-yaml-skill
description: Uses a specific model
model: claude-haiku-3
---
Do fast things."#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(skills[0].models, Some(vec!["claude-haiku-3".to_string()]));
    }

    #[test]
    fn test_yaml_frontmatter_with_models_list() {
        let content = r#"---
name: multi-model-skill
description: Allows multiple models
models:
  - claude-sonnet-4
  - gpt-4o
---
Flexible model skill."#;
        let skills = SkillLoader::parse_skill_md(content).unwrap();
        assert_eq!(
            skills[0].models,
            Some(vec!["claude-sonnet-4".to_string(), "gpt-4o".to_string()])
        );
    }
}

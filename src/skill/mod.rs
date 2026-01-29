use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    #[must_use] 
    pub fn resolve_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.models
            .as_ref()
            .and_then(|m| m.first())
            .map_or(default, std::string::String::as_str)
    }

    /// Check if a model is allowed for this skill
    #[must_use] 
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

/// Entry in the skill registry - supports progressive loading.
#[derive(Debug, Clone)]
struct SkillEntry {
    summary: SkillSummary,
    source_path: Option<PathBuf>,
    full: Option<Skill>,
}

#[derive(Default, Clone)]
pub struct SkillRegistry {
    entries: HashMap<String, SkillEntry>,
}

impl SkillRegistry {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fully loaded skill (backwards compatible).
    pub fn register(&mut self, skill: Skill) {
        let entry = SkillEntry {
            summary: SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
            },
            source_path: None,
            full: Some(skill.clone()),
        };
        self.entries.insert(skill.name.clone(), entry);
    }

    /// Register a skill summary for lazy loading.
    pub fn register_summary(&mut self, summary: SkillSummary, source_path: PathBuf) {
        let name = summary.name.clone();
        let entry = SkillEntry {
            summary,
            source_path: Some(source_path),
            full: None,
        };
        self.entries.insert(name, entry);
    }

    /// Get a skill, loading it if necessary.
    pub fn get(&mut self, name: &str) -> Option<&Skill> {
        // Check if we need to load
        if let Some(entry) = self.entries.get(name)
            && entry.full.is_none() && entry.source_path.is_some() {
                // Need to load - clone path to avoid borrow issues
                let path = entry.source_path.clone().unwrap();
                if let Ok(skills) = SkillLoader::load_from_file(&path)
                    && let Some(skill) = skills.into_iter().find(|s| s.name == name)
                        && let Some(entry) = self.entries.get_mut(name) {
                            entry.full = Some(skill);
                        }
            }

        self.entries.get(name).and_then(|e| e.full.as_ref())
    }

    /// Get skill summary without loading full content.
    #[must_use] 
    pub fn get_summary(&self, name: &str) -> Option<&SkillSummary> {
        self.entries.get(name).map(|e| &e.summary)
    }

    /// List all skill summaries (no loading required).
    #[must_use] 
    pub fn list(&self) -> Vec<SkillSummary> {
        self.entries.values().map(|e| e.summary.clone()).collect()
    }

    /// Scan a directory for skills, loading only frontmatter.
    pub fn scan_directory(&mut self, dir: &Path) -> Result<usize> {
        let mut count = 0;

        if !dir.exists() {
            return Ok(0);
        }

        // Look for SKILL.md files in subdirectories (skill-name/SKILL.md)
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_file = path.join("SKILL.md");
                    if skill_file.exists()
                        && let Ok(summary) = SkillLoader::load_summary(&skill_file) {
                            self.register_summary(summary, skill_file);
                            count += 1;
                        }
                } else if path.extension().is_some_and(|e| e == "md") {
                    // Also check for standalone SKILL.md files
                    if let Ok(summary) = SkillLoader::load_summary(&path) {
                        self.register_summary(summary, path);
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
    }
}

pub struct SkillLoader;

impl SkillLoader {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Vec<Skill>> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read SKILL.md at {}", path.display()))?;

        Self::parse_skill_md(&content)
    }

    /// Load only the skill summary (name + description) for progressive disclosure.
    /// This is efficient for startup - avoids loading full prompt content.
    pub fn load_summary<P: AsRef<Path>>(path: P) -> Result<SkillSummary> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read SKILL.md at {}", path.display()))?;

        Self::parse_summary(&content)
    }

    /// Parse only the summary (frontmatter) without loading full prompt.
    pub fn parse_summary(content: &str) -> Result<SkillSummary> {
        let trimmed = content.trim_start();

        if trimmed.starts_with("---") {
            // YAML frontmatter - parse just the header
            let after_first = trimmed
                .strip_prefix("---")
                .context("Missing frontmatter start")?;
            let end_idx = after_first
                .find("\n---")
                .context("Missing frontmatter end delimiter")?;

            let yaml_content = &after_first[..end_idx];
            let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml_content)
                .context("Failed to parse skill YAML frontmatter")?;

            Ok(SkillSummary {
                name: frontmatter.name,
                description: frontmatter.description,
            })
        } else {
            // Legacy XML - need to parse name and description tags
            let mut name = String::new();
            let mut description = String::new();

            for line in content.lines() {
                let trimmed_line = line.trim();
                if trimmed_line.starts_with("<name>") && trimmed_line.ends_with("</name>") {
                    name = trimmed_line[6..trimmed_line.len() - 7].to_string();
                } else if trimmed_line.starts_with("<description>")
                    && trimmed_line.ends_with("</description>")
                {
                    description = trimmed_line[13..trimmed_line.len() - 14].to_string();
                }
                // Stop once we have both
                if !name.is_empty() && !description.is_empty() {
                    break;
                }
            }

            if name.is_empty() {
                anyhow::bail!("Skill missing name");
            }

            Ok(SkillSummary { name, description })
        }
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

    #[test]
    fn test_parse_summary_yaml() {
        let content = r#"---
name: summary-test
description: Test parsing just the summary
allowed-tools:
  - Read
---
This is a very long prompt that we don't want to load at startup.
It contains many lines of instructions.
We only want the name and description initially."#;

        let summary = SkillLoader::parse_summary(content).unwrap();
        assert_eq!(summary.name, "summary-test");
        assert_eq!(summary.description, "Test parsing just the summary");
    }

    #[test]
    fn test_parse_summary_xml() {
        let content = r#"
<skill>
    <name>xml-summary</name>
    <description>XML format summary test</description>
    <prompt>
    Long prompt content here...
    </prompt>
</skill>
"#;
        let summary = SkillLoader::parse_summary(content).unwrap();
        assert_eq!(summary.name, "xml-summary");
        assert_eq!(summary.description, "XML format summary test");
    }

    #[test]
    fn test_registry_lazy_loading() {
        let mut registry = SkillRegistry::new();

        // Register a summary without full content
        let summary = SkillSummary {
            name: "lazy-skill".to_string(),
            description: "A lazily loaded skill".to_string(),
        };
        registry.register_summary(summary, PathBuf::from("/nonexistent/path.md"));

        // Should have the summary
        let found = registry.get_summary("lazy-skill");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "lazy-skill");

        // List should include it
        let list = registry.list();
        assert!(list.iter().any(|s| s.name == "lazy-skill"));
    }
}

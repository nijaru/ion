use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Option<Vec<String>>,
    pub model_override: Option<String>,
    pub prompt: String,
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

    pub fn parse_skill_md(content: &str) -> Result<Vec<Skill>> {
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
                    model_override: None,
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
                } else if trimmed.starts_with("<prompt>") {
                    in_prompt = true;
                    if trimmed.len() > 8 {
                        skill.prompt.push_str(&trimmed[8..]);
                        skill.prompt.push('\n');
                    }
                } else if trimmed.ends_with("</prompt>") {
                    if trimmed.len() > 9 {
                        skill.prompt.push_str(&trimmed[..trimmed.len() - 9]);
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
}

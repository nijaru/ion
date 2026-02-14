use crate::agent::instructions::InstructionLoader;
use crate::provider::{Message, ToolDefinition};
use crate::skill::Skill;
use minijinja::{Environment, context};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

pub struct ContextManager {
    env: Environment<'static>,
    system_prompt_base: String,
    instruction_loader: Option<Arc<InstructionLoader>>,
    active_skill: Arc<Mutex<Option<Skill>>>,
    render_cache: Mutex<Option<RenderCache>>,
    working_dir: Option<PathBuf>,
    has_mcp_tools: AtomicBool,
}

#[derive(Clone)]
struct RenderCache {
    rendered: String,
    skill: Option<Skill>,
    has_mcp_tools: bool,
    model_id: Option<String>,
}

pub struct ContextAssembly {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

const DEFAULT_SYSTEM_TEMPLATE: &str = r#"{{ base_instructions }}
{% if model_hints %}

{{ model_hints }}
{% endif %}
{% if working_dir %}
## Environment

Working directory: {{ working_dir }}
Date: {{ date }}
Platform: {{ os }}{% if shell %}, {{ shell }}{% endif %}
{% endif %}
{% if has_mcp_tools %}
## MCP Tools

MCP tools are available via external servers. Use `mcp_tools` to search for relevant tools by keyword before falling back to shell commands. Only use shell commands if MCP tools for that system are not available.
{% endif %}
{% if instructions %}
## Project Instructions

{{ instructions }}
{% endif %}
{% if skill %}
## Active Skill: {{ skill.name }}

{{ skill.prompt }}
{% endif %}
"#;

/// Return model-specific prompt additions based on model ID.
fn model_hints(model_id: &str) -> Option<&'static str> {
    let lower = model_id.to_lowercase();
    let model_part = lower.split('/').next_back().unwrap_or(&lower);

    if model_part.starts_with("gpt-5") || model_part.contains("-codex") {
        Some(
            "Minimize verbose reasoning. Think efficiently and act quickly. \
             Keep analysis internal — output actions and results, not thought process.",
        )
    } else if model_part.contains("deepseek") {
        Some("Be direct. Show code changes, not lengthy explanations.")
    } else {
        None
    }
}

impl ContextManager {
    #[must_use]
    pub fn new(system_prompt_base: String) -> Self {
        let mut env = Environment::new();
        if let Err(e) = env.add_template("system", DEFAULT_SYSTEM_TEMPLATE) {
            tracing::error!("Failed to register system template: {}", e);
        }

        Self {
            env,
            system_prompt_base,
            instruction_loader: None,
            active_skill: Arc::new(Mutex::new(None)),
            render_cache: Mutex::new(None),
            working_dir: None,
            has_mcp_tools: AtomicBool::new(false),
        }
    }

    /// Set the instruction loader for AGENTS.md support.
    #[must_use]
    pub fn with_instruction_loader(mut self, loader: Arc<InstructionLoader>) -> Self {
        self.instruction_loader = Some(loader);
        self
    }

    /// Set the working directory for environment context in the system prompt.
    #[must_use]
    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    /// Set whether MCP tools are available (enables MCP guidance in system prompt).
    pub fn set_has_mcp_tools(&self, val: bool) {
        self.has_mcp_tools.store(val, Ordering::Relaxed);
    }

    pub async fn set_active_skill(&self, skill: Option<Skill>) {
        let mut active = self.active_skill.lock().await;
        *active = skill;
    }

    /// Check if instruction files have changed on disk.
    fn instructions_stale(&self) -> bool {
        self.instruction_loader
            .as_ref()
            .is_some_and(|l| l.is_stale())
    }

    /// Get just the system prompt (cached), without assembling messages.
    /// Uses cached render if available; does not include model-specific hints
    /// (those are only injected via `assemble()` which has the model ID).
    pub async fn get_system_prompt(&self) -> String {
        let active_skill = self.active_skill.lock().await;
        let mcp = self.has_mcp_tools.load(Ordering::Relaxed);

        let mut cache = self.render_cache.lock().await;
        if let Some(ref c) = *cache
            && c.skill.as_ref() == active_skill.as_ref()
            && c.has_mcp_tools == mcp
            && !self.instructions_stale()
        {
            return c.rendered.clone();
        }

        let skill = active_skill.clone();
        drop(active_skill); // Release lock before potentially slow render

        let rendered = self.render_system_prompt(skill.as_ref(), None);
        *cache = Some(RenderCache {
            rendered: rendered.clone(),
            skill,
            has_mcp_tools: mcp,
            model_id: None,
        });
        rendered
    }

    pub async fn assemble(
        &self,
        history: &[Message],
        memory_context: Option<&str>,
        available_tools: Vec<ToolDefinition>,
        model_id: &str,
    ) -> ContextAssembly {
        let active_skill = self.active_skill.lock().await;
        let mcp = self.has_mcp_tools.load(Ordering::Relaxed);

        // Check cache - compare by reference to avoid clone
        let mut cache = self.render_cache.lock().await;
        let (system_prompt, need_cache_update) = if let Some(ref c) = *cache {
            if c.skill.as_ref() == active_skill.as_ref()
                && c.has_mcp_tools == mcp
                && c.model_id.as_deref() == Some(model_id)
                && !self.instructions_stale()
            {
                (c.rendered.clone(), false)
            } else {
                let skill = active_skill.clone();
                drop(active_skill);
                (self.render_system_prompt(skill.as_ref(), Some(model_id)), true)
            }
        } else {
            let skill = active_skill.clone();
            drop(active_skill);
            (self.render_system_prompt(skill.as_ref(), Some(model_id)), true)
        };

        // Update cache if needed
        if need_cache_update {
            // Re-acquire skill for cache storage
            let skill = self.active_skill.lock().await.clone();
            *cache = Some(RenderCache {
                rendered: system_prompt.clone(),
                skill,
                has_mcp_tools: mcp,
                model_id: Some(model_id.to_string()),
            });
        }

        let mut messages = history.to_vec();
        if let Some(context) = memory_context {
            messages.push(Message {
                role: crate::provider::Role::User,
                content: Arc::new(vec![crate::provider::ContentBlock::Text {
                    text: format!("Context from codebase memory:\n{context}"),
                }]),
            });
        }

        ContextAssembly {
            system_prompt,
            messages,
            tools: available_tools,
        }
    }

    fn render_system_prompt(&self, skill: Option<&Skill>, model_id: Option<&str>) -> String {
        let template = match self.env.get_template("system") {
            Ok(template) => template,
            Err(e) => {
                tracing::error!("System template unavailable: {}", e);
                return self.system_prompt_base.clone();
            }
        };

        // Load instructions from AGENTS.md files
        let instructions = self
            .instruction_loader
            .as_ref()
            .and_then(|loader| loader.load_all());

        // Environment context
        let working_dir = self.working_dir.as_ref().map(|d| d.display().to_string());
        let date = if self.working_dir.is_some() {
            Some(chrono::Local::now().format("%Y-%m-%d").to_string())
        } else {
            None
        };
        let os = if self.working_dir.is_some() {
            Some(std::env::consts::OS)
        } else {
            None
        };
        let shell = std::env::var("SHELL").ok();

        let has_mcp_tools = self.has_mcp_tools.load(Ordering::Relaxed);

        // Model-specific hints
        let hints = model_id.and_then(model_hints);

        template
            .render(context! {
                base_instructions => self.system_prompt_base,
                working_dir => working_dir,
                date => date,
                os => os,
                shell => shell,
                instructions => instructions,
                skill => skill,
                has_mcp_tools => has_mcp_tools,
                model_hints => hints,
            })
            .unwrap_or_else(|e| {
                tracing::error!("Failed to render system prompt template: {}", e);
                self.system_prompt_base.clone()
            })
    }
}

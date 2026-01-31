use crate::agent::designer::Plan;
use crate::agent::instructions::InstructionLoader;
use crate::provider::{Message, ToolDefinition};
use crate::skill::Skill;
use minijinja::{Environment, context};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct ContextManager {
    env: Environment<'static>,
    system_prompt_base: String,
    instruction_loader: Option<Arc<InstructionLoader>>,
    active_skill: Arc<Mutex<Option<Skill>>>,
    render_cache: Mutex<Option<RenderCache>>,
}

#[derive(Clone)]
struct RenderCache {
    rendered: String,
    plan: Option<Plan>,
    skill: Option<Skill>,
}

pub struct ContextAssembly {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

const DEFAULT_SYSTEM_TEMPLATE: &str = r#"
{{ base_instructions }}

{% if instructions %}
{{ instructions }}
{% endif %}

{% if plan %}
--- CURRENT PLAN ---
Title: {{ plan.title }}
{% for task in plan.tasks -%}
{% if task.status == "Completed" %}[x]{% elif task.status == "InProgress" %}[>]{% elif task.status == "Failed" %}[!]{% else %}[ ]{% endif %} {{ task.id }} - {{ task.title }}
{% endfor %}
{% if current_task %}
FOCUS: You are currently working on {{ current_task.id }}. {{ current_task.description }}
VERIFICATION: After each tool call, verify if the output matches the requirements of this task.
{% endif %}
{% endif %}

{% if skill %}
Active Skill: {{ skill.name }}
Instructions:
{{ skill.prompt }}
{% endif %}
"#;

impl ContextManager {
    #[must_use]
    pub fn new(system_prompt_base: String) -> Self {
        let mut env = Environment::new();
        env.add_template("system", DEFAULT_SYSTEM_TEMPLATE)
            .expect("DEFAULT_SYSTEM_TEMPLATE must be valid minijinja syntax");

        Self {
            env,
            system_prompt_base,
            instruction_loader: None,
            active_skill: Arc::new(Mutex::new(None)),
            render_cache: Mutex::new(None),
        }
    }

    /// Set the instruction loader for AGENTS.md support.
    pub fn with_instruction_loader(mut self, loader: Arc<InstructionLoader>) -> Self {
        self.instruction_loader = Some(loader);
        self
    }

    pub async fn set_active_skill(&self, skill: Option<Skill>) {
        let mut active = self.active_skill.lock().await;
        *active = skill;
    }

    /// Get just the system prompt (cached), without assembling messages.
    pub async fn get_system_prompt(&self, plan: Option<&Plan>) -> String {
        let active_skill = self.active_skill.lock().await;
        let skill = active_skill.clone();

        let mut cache = self.render_cache.lock().await;
        if let Some(ref c) = *cache
            && c.plan.as_ref() == plan
            && c.skill == skill
        {
            return c.rendered.clone();
        }

        let rendered = self.render_system_prompt(plan, skill.as_ref());
        *cache = Some(RenderCache {
            rendered: rendered.clone(),
            plan: plan.cloned(),
            skill,
        });
        rendered
    }

    pub async fn assemble(
        &self,
        history: &[Message],
        memory_context: Option<&str>,
        available_tools: Vec<ToolDefinition>,
        plan: Option<&Plan>,
    ) -> ContextAssembly {
        let active_skill = self.active_skill.lock().await;
        let skill = active_skill.clone();

        // Check cache
        let mut cache = self.render_cache.lock().await;
        let system_prompt = if let Some(ref c) = *cache {
            if c.plan.as_ref() == plan && c.skill == skill {
                c.rendered.clone()
            } else {
                self.render_system_prompt(plan, skill.as_ref())
            }
        } else {
            self.render_system_prompt(plan, skill.as_ref())
        };

        // Update cache
        if cache.as_ref().map(|c| &c.rendered) != Some(&system_prompt) {
            *cache = Some(RenderCache {
                rendered: system_prompt.clone(),
                plan: plan.cloned(),
                skill,
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

    fn render_system_prompt(&self, plan: Option<&Plan>, skill: Option<&Skill>) -> String {
        let template = self
            .env
            .get_template("system")
            .expect("system template must exist - added in constructor");
        let current_task = plan.and_then(|p| p.current_task());

        // Load instructions from AGENTS.md files
        let instructions = self
            .instruction_loader
            .as_ref()
            .and_then(|loader| loader.load_all());

        template
            .render(context! {
                base_instructions => self.system_prompt_base,
                instructions => instructions,
                plan => plan,
                current_task => current_task,
                skill => skill,
            })
            .unwrap_or_else(|e| {
                tracing::error!("Failed to render system prompt template: {}", e);
                self.system_prompt_base.clone()
            })
    }
}

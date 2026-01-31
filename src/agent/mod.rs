pub mod context;
pub mod designer;
mod events;
pub mod explorer;
pub mod instructions;
mod retry;
mod stream;
pub mod subagent;
mod tools;

pub use events::AgentEvent;

use crate::agent::context::ContextManager;
use crate::agent::designer::{Designer, Plan};
use crate::agent::instructions::InstructionLoader;
use crate::compaction::{
    CompactionConfig, PruningTier, TokenCounter, check_compaction_needed, prune_messages,
};
use crate::provider::{ContentBlock, LlmApi, Message, Role, ThinkingConfig};
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::tool::ToolOrchestrator;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

#[derive(Clone)]
pub struct Agent {
    provider: Arc<dyn LlmApi>,
    orchestrator: Arc<ToolOrchestrator>,
    designer: Option<Arc<Designer>>,
    compaction_config: CompactionConfig,
    /// Dynamic context window size (updated when model changes)
    context_window: Arc<std::sync::atomic::AtomicUsize>,
    token_counter: TokenCounter,
    skills: Arc<tokio::sync::RwLock<SkillRegistry>>,
    context_manager: Arc<ContextManager>,
    active_plan: Arc<Mutex<Option<Plan>>>,
}

/// Create instruction loader from current directory.
fn create_instruction_loader() -> Option<Arc<InstructionLoader>> {
    std::env::current_dir()
        .ok()
        .map(|cwd| Arc::new(InstructionLoader::new(cwd)))
}

/// Create context manager with optional instruction loader.
fn create_context_manager(system_prompt: String) -> ContextManager {
    if let Some(loader) = create_instruction_loader() {
        ContextManager::new(system_prompt).with_instruction_loader(loader)
    } else {
        ContextManager::new(system_prompt)
    }
}

impl Agent {
    pub fn new(provider: Arc<dyn LlmApi>, orchestrator: Arc<ToolOrchestrator>) -> Self {
        let designer = Arc::new(Designer::new(provider.clone()));
        let system_prompt = "You are ion, a fast terminal coding agent. Be concise and efficient. Use tools to fulfill user requests.".to_string();
        let compaction_config = CompactionConfig::default();
        let context_window = Arc::new(std::sync::atomic::AtomicUsize::new(
            compaction_config.context_window,
        ));

        let context_manager = create_context_manager(system_prompt);

        Self {
            provider,
            orchestrator,
            designer: Some(designer),
            compaction_config,
            context_window,
            token_counter: TokenCounter::new(),
            skills: Arc::new(tokio::sync::RwLock::new(SkillRegistry::new())),
            context_manager: Arc::new(context_manager),
            active_plan: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn with_compaction_config(mut self, config: CompactionConfig) -> Self {
        self.context_window
            .store(config.context_window, std::sync::atomic::Ordering::Relaxed);
        self.compaction_config = config;
        self
    }

    /// Update the context window size (call when model changes).
    pub fn set_context_window(&self, window: usize) {
        self.context_window
            .store(window, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get the current context window size.
    #[must_use]
    pub fn context_window(&self) -> usize {
        self.context_window
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[must_use]
    pub fn with_skills(mut self, skills: SkillRegistry) -> Self {
        self.skills = Arc::new(tokio::sync::RwLock::new(skills));
        self
    }

    /// Set a custom system prompt (overrides default).
    #[must_use]
    pub fn with_system_prompt(self, prompt: String) -> Self {
        Self {
            context_manager: Arc::new(create_context_manager(prompt)),
            ..self
        }
    }

    pub async fn activate_skill(&self, name: Option<String>) -> Result<()> {
        let skill = if let Some(ref n) = name {
            let mut skills = self.skills.write().await;
            Some(
                skills
                    .get(n)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Skill not found: {n}"))?,
            )
        } else {
            None
        };
        self.context_manager.set_active_skill(skill).await;
        Ok(())
    }

    #[must_use]
    pub fn provider(&self) -> Arc<dyn LlmApi> {
        self.provider.clone()
    }

    /// Clear the active plan (e.g., when starting fresh with /clear).
    pub async fn clear_plan(&self) {
        let mut plan = self.active_plan.lock().await;
        *plan = None;
    }

    async fn emit_token_usage(&self, messages: &[Message], tx: &mpsc::Sender<AgentEvent>) {
        // Get system prompt (cached) without cloning messages
        let plan = self.active_plan.lock().await;
        let system_prompt = self.context_manager.get_system_prompt(plan.as_ref()).await;

        // Count system prompt + all messages
        let system_tokens = self.token_counter.count_str(&system_prompt);
        let message_tokens = self.token_counter.count_messages(messages).total;
        let total = system_tokens + message_tokens;

        let _ = tx
            .send(AgentEvent::TokenUsage {
                used: total,
                max: self.context_window(),
            })
            .await;
    }

    pub async fn plan(
        &self,
        user_msg: &str,
        session: &Session,
    ) -> Result<crate::agent::designer::Plan> {
        if let Some(designer) = &self.designer {
            designer
                .plan(user_msg, &session.model, &session.messages)
                .await
        } else {
            Err(anyhow::anyhow!("Designer not initialized"))
        }
    }

    /// Run a task with the given user message.
    ///
    /// Returns the session (with any work completed) and optionally an error.
    /// The session is always returned so partial work can be persisted.
    pub async fn run_task(
        &self,
        mut session: Session,
        user_content: Vec<ContentBlock>,
        tx: mpsc::Sender<AgentEvent>,
        message_queue: Option<Arc<std::sync::Mutex<Vec<String>>>>,
        thinking: Option<ThinkingConfig>,
    ) -> (Session, Option<anyhow::Error>) {
        // Extract text for plan generation (ignore images for this purpose)
        let user_msg: String = user_content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(user_content),
        });

        // Send initial token usage
        self.emit_token_usage(&session.messages, &tx).await;

        // Optional: Run designer for complex requests
        if session.messages.len() <= 2
            && user_msg.len() > 100
            && let Ok(plan) = self.plan(&user_msg, &session).await
        {
            {
                let mut active_plan = self.active_plan.lock().await;
                *active_plan = Some(plan.clone());
            }
            let _ = tx.send(AgentEvent::PlanGenerated(plan)).await;
        }

        loop {
            if session.abort_token.is_cancelled() {
                return (session, Some(anyhow::anyhow!("Cancelled")));
            }

            // Check for queued user messages between turns
            let had_queued = if let Some(ref queue) = message_queue {
                // Handle poisoned lock by recovering inner data
                let mut guard = match queue.lock() {
                    Ok(g) => g,
                    Err(poisoned) => {
                        warn!("Message queue lock was poisoned, recovering");
                        poisoned.into_inner()
                    }
                };
                let had_queued = !guard.is_empty();
                for queued_msg in guard.drain(..) {
                    session.messages.push(Message {
                        role: Role::User,
                        content: Arc::new(vec![ContentBlock::Text { text: queued_msg }]),
                    });
                }
                had_queued
                // guard dropped here before await
            } else {
                false
            };
            // Update token count if we added queued messages
            if had_queued {
                self.emit_token_usage(&session.messages, &tx).await;
            }

            match self.execute_turn(&mut session, &tx, thinking.clone()).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => return (session, Some(e)),
            }
        }

        (session, None)
    }

    async fn execute_turn(
        &self,
        session: &mut Session,
        tx: &mpsc::Sender<AgentEvent>,
        thinking: Option<ThinkingConfig>,
    ) -> Result<bool> {
        let ctx = stream::StreamContext {
            provider: &self.provider,
            orchestrator: &self.orchestrator,
            context_manager: &self.context_manager,
            active_plan: &self.active_plan,
            token_counter: &self.token_counter,
        };
        let (assistant_blocks, tool_calls) =
            stream::stream_response(&ctx, session, tx, thinking, session.abort_token.clone())
                .await?;

        session.messages.push(Message {
            role: Role::Assistant,
            content: Arc::new(assistant_blocks),
        });

        // Update token usage after assistant response
        self.emit_token_usage(&session.messages, tx).await;

        if tool_calls.is_empty() {
            return Ok(false);
        }

        let tool_results = tools::execute_tools_parallel(
            &self.orchestrator,
            session,
            tool_calls,
            tx,
            session.abort_token.clone(),
        )
        .await?;

        session.messages.push(Message {
            role: Role::ToolResult,
            content: Arc::new(tool_results),
        });

        // Token usage tracking
        self.emit_token_usage(&session.messages, tx).await;

        // Check for compaction using dynamic context window
        let mut config = self.compaction_config.clone();
        config.context_window = self.context_window();

        if check_compaction_needed(&session.messages, &config, &self.token_counter).needs_compaction
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let threshold = config.trigger_threshold as usize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let target_tokens = config.target_threshold as usize;

            let mut messages = (*session.messages).to_vec();
            let result = prune_messages(&mut messages, &config, &self.token_counter, target_tokens);

            if result.tier_reached != PruningTier::None {
                session.messages = messages;
                let _ = tx
                    .send(AgentEvent::CompactionStatus {
                        threshold,
                        pruned: true,
                    })
                    .await;
            }
        }

        Ok(true)
    }
}

pub mod context;
pub mod designer;
pub mod explorer;

use crate::agent::context::ContextManager;
use crate::agent::designer::{Designer, Plan};
use crate::compaction::{
    CompactionConfig, PruningTier, TokenCounter, check_compaction_needed, prune_messages,
};
use crate::provider::{
    ChatRequest, ContentBlock, Message, Provider, Role, StreamEvent, ThinkingConfig, ToolCallEvent,
    ToolDefinition,
};
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::tool::{ToolContext, ToolOrchestrator};
use anyhow::Result;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;
use tracing::error;

#[derive(Clone)]
pub struct Agent {
    provider: Arc<dyn Provider>,
    orchestrator: Arc<ToolOrchestrator>,
    designer: Option<Arc<Designer>>,
    compaction_config: CompactionConfig,
    token_counter: TokenCounter,
    skills: SkillRegistry,
    context_manager: Arc<ContextManager>,
    active_plan: Arc<Mutex<Option<Plan>>>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, orchestrator: Arc<ToolOrchestrator>) -> Self {
        let designer = Arc::new(Designer::new(provider.clone()));
        let system_prompt = "You are ion, a fast terminal coding agent. Be concise and efficient. Use tools to fulfill user requests.".to_string();
        Self {
            provider,
            orchestrator,
            designer: Some(designer),
            compaction_config: CompactionConfig::default(),
            token_counter: TokenCounter::new(),
            skills: SkillRegistry::new(),
            context_manager: Arc::new(ContextManager::new(system_prompt)),
            active_plan: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_compaction_config(mut self, config: CompactionConfig) -> Self {
        self.compaction_config = config;
        self
    }

    pub fn with_skills(mut self, skills: SkillRegistry) -> Self {
        self.skills = skills;
        self
    }

    pub async fn activate_skill(&self, name: Option<String>) -> Result<()> {
        let skill = if let Some(ref n) = name {
            Some(
                self.skills
                    .get(n)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Skill not found: {}", n))?,
            )
        } else {
            None
        };
        self.context_manager.set_active_skill(skill).await;
        Ok(())
    }

    pub fn provider(&self) -> Arc<dyn Provider> {
        self.provider.clone()
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

    pub async fn run_task(
        &self,
        mut session: Session,
        user_msg: String,
        tx: mpsc::Sender<AgentEvent>,
        message_queue: Option<Arc<std::sync::Mutex<Vec<String>>>>,
        thinking: Option<ThinkingConfig>,
    ) -> Result<Session> {
        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: user_msg.clone(),
            }]),
        });

        // Send initial token usage
        let token_count = self.token_counter.count_messages(&session.messages);
        let _ = tx
            .send(AgentEvent::TokenUsage {
                used: token_count.total,
                max: self.compaction_config.context_window,
            })
            .await;

        // Optional: Run designer for complex requests
        if session.messages.len() <= 2 && user_msg.len() > 100 {
            if let Ok(plan) = self.plan(&user_msg, &session).await {
                {
                    let mut active_plan = self.active_plan.lock().await;
                    *active_plan = Some(plan.clone());
                }
                let _ = tx.send(AgentEvent::PlanGenerated(plan)).await;
            }
        }

        loop {
            // Check for queued user messages between turns
            if let Some(ref queue) = message_queue {
                if let Ok(mut queue) = queue.lock() {
                    for queued_msg in queue.drain(..) {
                        session.messages.push(Message {
                            role: Role::User,
                            content: Arc::new(vec![ContentBlock::Text { text: queued_msg }]),
                        });
                    }
                }
            }

            if !self
                .execute_turn(&mut session, &tx, thinking.clone())
                .await?
            {
                break;
            }
        }

        Ok(session)
    }

    async fn execute_turn(
        &self,
        session: &mut Session,
        tx: &mpsc::Sender<AgentEvent>,
        thinking: Option<ThinkingConfig>,
    ) -> Result<bool> {
        let (assistant_blocks, tool_calls) =
            self.stream_response(session, tx, thinking).await?;

        session.messages.push(Message {
            role: Role::Assistant,
            content: Arc::new(assistant_blocks),
        });

        // Update token usage after assistant response
        let token_count = self.token_counter.count_messages(&session.messages);
        let _ = tx
            .send(AgentEvent::TokenUsage {
                used: token_count.total,
                max: self.compaction_config.context_window,
            })
            .await;

        if tool_calls.is_empty() {
            return Ok(false);
        }

        let tool_results = self.execute_tools_parallel(session, tool_calls, tx).await?;

        session.messages.push(Message {
            role: Role::ToolResult,
            content: Arc::new(tool_results),
        });

        // Token usage tracking
        let token_count = self.token_counter.count_messages(&session.messages);
        let _ = tx
            .send(AgentEvent::TokenUsage {
                used: token_count.total,
                max: self.compaction_config.context_window,
            })
            .await;

        // Check for compaction
        if check_compaction_needed(
            &session.messages,
            &self.compaction_config,
            &self.token_counter,
        )
        .needs_compaction
        {
            let threshold = self.compaction_config.trigger_threshold as usize;
            let target_tokens = self.compaction_config.target_threshold as usize;

            let mut messages = (*session.messages).to_vec();
            let result = prune_messages(
                &mut messages,
                &self.compaction_config,
                &self.token_counter,
                target_tokens,
            );

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

    async fn stream_response(
        &self,
        session: &Session,
        tx: &mpsc::Sender<AgentEvent>,
        thinking: Option<ThinkingConfig>,
    ) -> Result<(Vec<ContentBlock>, Vec<ToolCallEvent>)> {
        let tool_defs: Vec<ToolDefinition> = self
            .orchestrator
            .list_tools()
            .into_iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect();

        let plan = self.active_plan.lock().await;
        let assembly = self
            .context_manager
            .assemble(&session.messages, None, tool_defs, plan.as_ref())
            .await;

        let request = ChatRequest {
            model: session.model.clone(),
            messages: Arc::new(assembly.messages.clone()),
            system: Some(Cow::Owned(assembly.system_prompt.clone())),
            tools: Arc::new(assembly.tools),
            max_tokens: None,
            temperature: None,
            thinking,
        };

        let input_tokens = self.token_counter.count_str(&assembly.system_prompt)
            + assembly
                .messages
                .iter()
                .map(|m| self.token_counter.count_message(m).total)
                .sum::<usize>();
        let _ = tx.send(AgentEvent::InputTokens(input_tokens)).await;

        let (stream_tx, mut stream_rx) = mpsc::channel(100);
        let provider = self.provider.clone();

        tokio::spawn(async move {
            if let Err(e) = provider.stream(request, stream_tx).await {
                error!("Provider stream error: {}", e);
            }
        });

        let mut assistant_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        while let Some(event) = stream_rx.recv().await {
            match event {
                StreamEvent::TextDelta(delta) => {
                    let delta_tokens = self.token_counter.count_str(&delta);
                    let _ = tx.send(AgentEvent::OutputTokensDelta(delta_tokens)).await;
                    let _ = tx.send(AgentEvent::TextDelta(delta.clone())).await;
                    if let Some(ContentBlock::Text { text }) = assistant_blocks.last_mut() {
                        text.push_str(&delta);
                    } else {
                        assistant_blocks.push(ContentBlock::Text { text: delta });
                    }
                }
                StreamEvent::ThinkingDelta(delta) => {
                    let _ = tx.send(AgentEvent::ThinkingDelta(delta.clone())).await;
                    if let Some(ContentBlock::Thinking { thinking }) = assistant_blocks.last_mut() {
                        thinking.push_str(&delta);
                    } else {
                        assistant_blocks.push(ContentBlock::Thinking { thinking: delta });
                    }
                }
                StreamEvent::ToolCall(call) => {
                    let _ = tx
                        .send(AgentEvent::ToolCallStart(
                            call.id.clone(),
                            call.name.clone(),
                        ))
                        .await;
                    tool_calls.push(call.clone());
                    assistant_blocks.push(ContentBlock::ToolCall {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                    });
                }
                StreamEvent::Error(e) => return Err(anyhow::anyhow!(e)),
                _ => {}
            }
        }

        Ok((assistant_blocks, tool_calls))
    }

    async fn execute_tools_parallel(
        &self,
        session: &Session,
        tool_calls: Vec<ToolCallEvent>,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<Vec<ContentBlock>> {
        let mut set = JoinSet::new();
        let num_tools = tool_calls.len();

        let ctx = ToolContext {
            working_dir: session.working_dir.clone(),
            session_id: session.id.clone(),
            abort_signal: session.abort_token.clone(),
            no_sandbox: session.no_sandbox,
            index_callback: None,
            discovery_callback: None,
        };

        for (index, call) in tool_calls.into_iter().enumerate() {
            let orchestrator = self.orchestrator.clone();
            let tx = tx.clone();
            let ctx_clone = ctx.clone();

            set.spawn(async move {
                let result = orchestrator
                    .call_tool(&call.name, call.arguments, &ctx_clone)
                    .await;
                let block = match result {
                    Ok(res) => {
                        let _ = tx
                            .send(AgentEvent::ToolCallResult(
                                call.id.clone(),
                                res.content.clone(),
                                res.is_error,
                            ))
                            .await;
                        ContentBlock::ToolResult {
                            tool_call_id: call.id,
                            content: res.content,
                            is_error: res.is_error,
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(AgentEvent::ToolCallResult(
                                call.id.clone(),
                                format!("Error: {}", e),
                                true,
                            ))
                            .await;
                        ContentBlock::ToolResult {
                            tool_call_id: call.id,
                            content: format!("Error: {}", e),
                            is_error: true,
                        }
                    }
                };
                (index, block)
            });
        }

        let mut results = vec![None; num_tools];
        while let Some(res) = set.join_next().await {
            let (index, block) = res?;
            results[index] = Some(block);
        }

        Ok(results.into_iter().map(|o| o.unwrap()).collect())
    }
}

pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallStart(String, String),
    ToolCallResult(String, String, bool),
    PlanGenerated(crate::agent::designer::Plan),
    CompactionStatus { threshold: usize, pruned: bool },
    TokenUsage { used: usize, max: usize },
    InputTokens(usize),
    OutputTokensDelta(usize),
    Finished(String),
    Error(String),
    ModelsFetched(Vec<crate::provider::ModelInfo>),
    ModelFetchError(String),
}

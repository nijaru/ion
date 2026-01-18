pub mod context;
pub mod designer;
pub mod explorer;

use crate::agent::context::ContextManager;
use crate::agent::designer::{Designer, Plan};
use crate::agent::explorer::Explorer;
use crate::compaction::{
    CompactionConfig, PruningTier, TokenCounter, check_compaction_needed, prune_messages,
};
use crate::memory::embedding::EmbeddingProvider;
use crate::memory::{MemorySystem, MemoryType};
use crate::provider::{
    ChatRequest, ContentBlock, Message, Provider, Role, StreamEvent, ThinkingConfig, ToolCallEvent,
    ToolDefinition,
};
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::tool::{ToolContext, ToolOrchestrator};
use anyhow::Result;
use std::borrow::Cow;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;
use tracing::error;

#[derive(Clone)]
pub struct Agent {
    provider: Arc<dyn Provider>,
    orchestrator: Arc<ToolOrchestrator>,
    memory: Option<Arc<Mutex<MemorySystem>>>,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
    explorer: Option<Arc<Explorer>>,
    designer: Option<Arc<Designer>>,
    indexing_worker: Option<Arc<crate::memory::IndexingWorker>>,
    compaction_config: CompactionConfig,
    token_counter: TokenCounter,
    skills: SkillRegistry,
    context_manager: Arc<ContextManager>,
    active_plan: Arc<Mutex<Option<Plan>>>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, orchestrator: Arc<ToolOrchestrator>) -> Self {
        let designer = Arc::new(Designer::new(provider.clone()));
        let system_prompt = "You are ion, a high-performance Rust terminal agent. Be concise, professional, and efficient. Use tools whenever necessary to fulfill the user request.".to_string();
        Self {
            provider,
            orchestrator,
            memory: None,
            embedding: None,
            explorer: None,
            designer: Some(designer),
            indexing_worker: None,
            compaction_config: CompactionConfig::default(),
            token_counter: TokenCounter::new(),
            skills: SkillRegistry::new(),
            context_manager: Arc::new(ContextManager::new(system_prompt)),
            active_plan: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_memory(
        mut self,
        memory: Arc<Mutex<MemorySystem>>,
        embedding: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        let worker = Arc::new(crate::memory::IndexingWorker::new(
            memory.clone(),
            embedding.clone(),
        ));
        self.memory = Some(memory);
        self.embedding = Some(embedding);
        self.indexing_worker = Some(worker);
        self
    }

    pub fn with_explorer(mut self, explorer: Arc<Explorer>) -> Self {
        self.explorer = Some(explorer);
        self
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

    pub fn indexing_worker(&self) -> Option<Arc<crate::memory::IndexingWorker>> {
        self.indexing_worker.clone()
    }

    /// Re-index a specific path using the Explorer.
    pub async fn reindex(&self, path: Option<&Path>) -> Result<()> {
        if let Some(explorer) = &self.explorer {
            if let Some(p) = path {
                explorer.index_path(p).await?;
            } else {
                // If no path, we could default to working dir, but let's keep it targeted
                explorer.index_path(&std::env::current_dir()?).await?;
            }
        }
        Ok(())
    }

    /// Index a single file lazily.
    pub async fn index_file(&self, path: &Path) -> Result<()> {
        if let Some(explorer) = &self.explorer {
            explorer.index_file(path).await?;
        }
        Ok(())
    }

    /// Run a discovery pass to find relevant symbols or files.
    pub async fn discover(&self, query: &str) -> Result<Vec<(String, f32)>> {
        let (Some(memory), Some(embedding)) = (&self.memory, &self.embedding) else {
            return Ok(Vec::new());
        };

        let vector = embedding.embed(query).await?;
        let memory = memory.clone();
        let query_owned = query.to_string();

        let results = tokio::task::spawn_blocking(move || {
            let mut ms = memory.blocking_lock();
            ms.hybrid_search(vector, &query_owned, 10)
        })
        .await??;

        Ok(results
            .into_iter()
            .map(|(entry, score)| (entry.text, score))
            .collect())
    }

    /// Generate a plan for a complex task.
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
        thinking: Option<ThinkingConfig>,
    ) -> Result<Session> {
        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: user_msg.clone(),
            }]),
        });

        // Index user message
        if let Err(e) = self.index_message(&user_msg, MemoryType::Working).await {
            error!("Failed to index user message: {}", e);
        }

        // Prune old memories once per session
        if session.messages.len() <= 2 {
            if let Some(memory) = &self.memory {
                let memory = memory.clone();
                tokio::task::spawn_blocking(move || {
                    let mut ms = memory.blocking_lock();
                    let _ = ms.prune();
                });
            }
        }

        // Optional: Run designer for complex requests (e.g. first message, long prompt)
        if session.messages.len() <= 2 && user_msg.len() > 100 {
            if let Ok(plan) = self.plan(&user_msg, &session).await {
                {
                    let mut active_plan = self.active_plan.lock().await;
                    *active_plan = Some(plan.clone());
                }
                let _ = tx.send(AgentEvent::PlanGenerated(plan)).await;
            }
        }

        // Fetch memory context once for this task
        let memory_context = self
            .retrieve_context(&user_msg, 5, &tx)
            .await
            .unwrap_or(None);

        loop {
            if !self
                .execute_turn(&mut session, &tx, memory_context.as_deref(), thinking.clone())
                .await?
            {
                break;
            }
        }

        Ok(session)
    }

    /// Index a message turn into memory.
    async fn index_message(&self, text: &str, r#type: MemoryType) -> Result<()> {
        if let Some(worker) = &self.indexing_worker {
            let metadata = serde_json::json!({
                "source": "conversation",
                "indexed_at": chrono::Utc::now().to_rfc3339()
            });
            worker.index(text.to_string(), r#type, metadata).await?;
        }
        Ok(())
    }

    async fn execute_turn(
        &self,
        session: &mut Session,
        tx: &mpsc::Sender<AgentEvent>,
        memory_context: Option<&str>,
        thinking: Option<ThinkingConfig>,
    ) -> Result<bool> {
        let (assistant_blocks, tool_calls) =
            self.stream_response(session, tx, memory_context, thinking).await?;

        // Index assistant turn (text blocks)
        let assistant_text: String = assistant_blocks
            .iter()
            .filter_map(|b| {
                if let ContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"); // Corrected: Changed join separator to "\n"

        if !assistant_text.is_empty() {
            if let Err(e) = self
                .index_message(&assistant_text, MemoryType::Working)
                .await
            {
                error!("Failed to index assistant response: {}", e);
            }
        }

        // Save assistant turn
        session.messages.push(Message {
            role: Role::Assistant,
            content: Arc::new(assistant_blocks),
        });

        if tool_calls.is_empty() {
            return Ok(false);
        }

        // Execute tools (in parallel)
        let tool_results = self.execute_tools_parallel(session, tool_calls, tx).await?;

        // Index tool results
        for result in &tool_results {
            if let ContentBlock::ToolResult { content, .. } = result {
                // Working memory for tool results (short-lived but useful for immediate context)
                if let Err(e) = self.index_message(content, MemoryType::Working).await {
                    error!("Failed to index tool result: {}", e);
                }
            }
        }

        session.messages.push(Message {
            role: Role::ToolResult,
            content: Arc::new(tool_results),
        });

        // Count current tokens and send usage event
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

    /// Retrieve relevant context from memory based on query text.
    async fn retrieve_context(
        &self,
        query_text: &str,
        limit: usize,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<Option<String>> {
        let (Some(memory), Some(embedding)) = (&self.memory, &self.embedding) else {
            return Ok(None);
        };

        let vector = embedding.embed(query_text).await?;
        let memory = memory.clone();
        let query_text_owned = query_text.to_string();

        let results = tokio::task::spawn_blocking(move || {
            let mut ms = memory.blocking_lock();
            ms.hybrid_search(vector, &query_text_owned, limit)
        })
        .await??;

        let _ = tx
            .send(AgentEvent::MemoryRetrieval {
                query: query_text.to_string(),
                results_count: results.len(),
            })
            .await;

        if results.is_empty() {
            return Ok(None);
        }

        let mut context = String::from("\nRelevant Context from Memory:\n");
        for (entry, score) in results {
            context.push_str(&format!(
                "--- [{:?}, Score: {:.2}] ---\n{}\n",
                entry.r#type, score, entry.text
            ));
        }

        Ok(Some(context))
    }

    async fn stream_response(
        &self,
        session: &Session,
        tx: &mpsc::Sender<AgentEvent>,
        memory_context: Option<&str>,
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
            .assemble(&session.messages, memory_context, tool_defs, plan.as_ref())
            .await;

        let request = ChatRequest {
            model: session.model.clone(),
            messages: Arc::new(assembly.messages),
            system: Some(Cow::Owned(assembly.system_prompt)),
            tools: Arc::new(assembly.tools),
            max_tokens: None,
            temperature: None,
            thinking,
        };

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
                _ => {} // Corrected: Added missing underscore for unused match arm
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

        let agent_ref = self.clone();
        let index_callback: Option<Arc<dyn Fn(PathBuf) + Send + Sync>> =
            Some(Arc::new(move |path| {
                let agent = agent_ref.clone();
                tokio::spawn(async move {
                    if let Err(e) = agent.index_file(&path).await {
                        error!("Lazy indexing failed for {:?}: {}", path, e);
                    }
                });
            }));

        let agent_ref_disc = self.clone();
        let discovery_callback = Some(Arc::new(move |query: String| {
            let agent = agent_ref_disc.clone();
            Box::pin(async move { agent.discover(&query).await })
                as Pin<Box<dyn Future<Output = Result<Vec<(String, f32)>>> + Send>>
        })
            as Arc<
                dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<(String, f32)>>> + Send>>
                    + Send
                    + Sync,
            >);

        let ctx = ToolContext {
            working_dir: session.working_dir.clone(),
            session_id: session.id.clone(),
            abort_signal: session.abort_token.clone(),
            index_callback,
            discovery_callback,
        };

        // Track original order via index
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
    MemoryRetrieval { query: String, results_count: usize },
    /// Current token usage for context tracking
    TokenUsage { used: usize, max: usize },
    Finished(String),
    Error(String),
    // Model picker events
    ModelsFetched(Vec<crate::provider::ModelInfo>),
    ModelFetchError(String),
}

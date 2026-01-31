pub mod context;
pub mod designer;
pub mod explorer;
pub mod instructions;
pub mod subagent;

use crate::agent::context::ContextManager;
use crate::agent::designer::{Designer, Plan};
use crate::agent::instructions::InstructionLoader;
use crate::compaction::{
    CompactionConfig, PruningTier, TokenCounter, check_compaction_needed, prune_messages,
};
use crate::provider::{
    ChatRequest, ContentBlock, LlmApi, Message, Role, StreamEvent, ThinkingConfig, ToolCallEvent,
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
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

/// Check if an error is retryable (transient network/server issues)
fn is_retryable_error(err: &str) -> bool {
    let err_lower = err.to_lowercase();

    // Rate limits
    if err.contains("429") || err_lower.contains("rate limit") {
        return true;
    }

    // Timeouts
    if err_lower.contains("timeout")
        || err_lower.contains("timed out")
        || err_lower.contains("deadline exceeded")
    {
        return true;
    }

    // Network errors
    if err_lower.contains("connection")
        || err_lower.contains("network")
        || err_lower.contains("dns")
        || err_lower.contains("resolve")
    {
        return true;
    }

    // Server errors (5xx)
    if err.contains("500")
        || err.contains("502")
        || err.contains("503")
        || err.contains("504")
        || err_lower.contains("server error")
        || err_lower.contains("internal error")
        || err_lower.contains("service unavailable")
        || err_lower.contains("bad gateway")
    {
        return true;
    }

    false
}

/// Get a human-readable category for a retryable error
fn categorize_error(err: &str) -> &'static str {
    let err_lower = err.to_lowercase();

    if err.contains("429") || err_lower.contains("rate limit") {
        return "Rate limited";
    }

    if err_lower.contains("timeout")
        || err_lower.contains("timed out")
        || err_lower.contains("deadline exceeded")
    {
        return "Request timed out";
    }

    if err_lower.contains("connection")
        || err_lower.contains("network")
        || err_lower.contains("dns")
        || err_lower.contains("resolve")
    {
        return "Network error";
    }

    if err.contains("500")
        || err.contains("502")
        || err.contains("503")
        || err.contains("504")
        || err_lower.contains("server error")
        || err_lower.contains("internal error")
        || err_lower.contains("service unavailable")
        || err_lower.contains("bad gateway")
    {
        return "Server error";
    }

    "Transient error"
}

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
        user_msg: String,
        tx: mpsc::Sender<AgentEvent>,
        message_queue: Option<Arc<std::sync::Mutex<Vec<String>>>>,
        thinking: Option<ThinkingConfig>,
    ) -> (Session, Option<anyhow::Error>) {
        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: user_msg.clone(),
            }]),
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
        let (assistant_blocks, tool_calls) = self
            .stream_response(session, tx, thinking, session.abort_token.clone())
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

        let tool_results = self
            .execute_tools_parallel(session, tool_calls, tx, session.abort_token.clone())
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

    async fn stream_response(
        &self,
        session: &Session,
        tx: &mpsc::Sender<AgentEvent>,
        thinking: Option<ThinkingConfig>,
        abort_token: tokio_util::sync::CancellationToken,
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

        // Ollama and OpenRouter don't support streaming with tools reliably
        let provider_id = self.provider.id();
        let use_streaming =
            (provider_id != "ollama" && provider_id != "openrouter") || request.tools.is_empty();

        if use_streaming
            && let Some(result) = self.stream_with_retry(&request, tx, &abort_token).await?
        {
            return Ok(result);
        }
        // Fallback to non-streaming if streaming not supported or returns None

        self.complete_with_retry(&request, tx, &abort_token).await
    }

    /// Attempt streaming completion with retry logic.
    /// Returns Some((blocks, calls)) on success, None if fallback to non-streaming is needed.
    async fn stream_with_retry(
        &self,
        request: &ChatRequest,
        tx: &mpsc::Sender<AgentEvent>,
        abort_token: &CancellationToken,
    ) -> Result<Option<(Vec<ContentBlock>, Vec<ToolCallEvent>)>> {
        const MAX_RETRIES: u32 = 3;
        let mut retry_count = 0;
        let mut assistant_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        'retry: loop {
            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let provider = self.provider.clone();
            let request_clone = request.clone();

            let handle =
                tokio::spawn(async move { provider.stream(request_clone, stream_tx).await });

            let mut stream_error: Option<String> = None;

            loop {
                tokio::select! {
                    () = abort_token.cancelled() => {
                        handle.abort();
                        return Err(anyhow::anyhow!("Cancelled"));
                    }
                    event = stream_rx.recv() => {
                        match event {
                            Some(StreamEvent::TextDelta(delta)) => {
                                let delta_tokens = self.token_counter.count_str(&delta);
                                let _ = tx.send(AgentEvent::OutputTokensDelta(delta_tokens)).await;
                                let _ = tx.send(AgentEvent::TextDelta(delta.clone())).await;
                                if let Some(ContentBlock::Text { text }) = assistant_blocks.last_mut() {
                                    text.push_str(&delta);
                                } else {
                                    assistant_blocks.push(ContentBlock::Text { text: delta });
                                }
                            }
                            Some(StreamEvent::ThinkingDelta(delta)) => {
                                let _ = tx.send(AgentEvent::ThinkingDelta(delta.clone())).await;
                                if let Some(ContentBlock::Thinking { thinking }) = assistant_blocks.last_mut() {
                                    thinking.push_str(&delta);
                                } else {
                                    assistant_blocks.push(ContentBlock::Thinking { thinking: delta });
                                }
                            }
                            Some(StreamEvent::ToolCall(call)) => {
                                let _ = tx
                                    .send(AgentEvent::ToolCallStart(
                                        call.id.clone(),
                                        call.name.clone(),
                                        call.arguments.clone(),
                                    ))
                                    .await;
                                tool_calls.push(call.clone());
                                assistant_blocks.push(ContentBlock::ToolCall {
                                    id: call.id,
                                    name: call.name,
                                    arguments: call.arguments,
                                });
                            }
                            Some(StreamEvent::Error(e)) => {
                                stream_error = Some(e);
                                break;
                            }
                            Some(_) => {}
                            None => {
                                if let Ok(Err(e)) = handle.await {
                                    stream_error = Some(e.to_string());
                                }
                                break;
                            }
                        }
                    }
                }
            }

            if let Some(ref err) = stream_error {
                let err_lower = err.to_lowercase();
                let is_tools_not_supported = err_lower
                    .contains("streaming with tools not supported")
                    || err_lower.contains("tools not supported")
                    || (err_lower.contains("parse") && !request.tools.is_empty());

                if is_tools_not_supported {
                    warn!(
                        "Provider doesn't support streaming with tools, falling back to non-streaming"
                    );
                    return Ok(None); // Signal fallback needed
                } else if is_retryable_error(err) && retry_count < MAX_RETRIES {
                    retry_count += 1;
                    let delay = 1u64 << retry_count;
                    let reason = categorize_error(err);
                    warn!(
                        "{}, retrying in {}s (attempt {}/{})",
                        reason, delay, retry_count, MAX_RETRIES
                    );
                    let _ = tx.send(AgentEvent::Retry(reason.to_string(), delay)).await;
                    assistant_blocks.clear();
                    tool_calls.clear();
                    // Interruptible sleep - check abort token during retry delay
                    tokio::select! {
                        () = abort_token.cancelled() => {
                            return Err(anyhow::anyhow!("Cancelled"));
                        }
                        () = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
                    }
                    continue 'retry;
                }
                error!("Stream error: {}", err);
                return Err(anyhow::anyhow!("{err}"));
            }
            return Ok(Some((assistant_blocks, tool_calls)));
        }
    }

    /// Non-streaming completion with retry logic.
    async fn complete_with_retry(
        &self,
        request: &ChatRequest,
        tx: &mpsc::Sender<AgentEvent>,
        abort_token: &CancellationToken,
    ) -> Result<(Vec<ContentBlock>, Vec<ToolCallEvent>)> {
        const MAX_RETRIES: u32 = 3;
        let mut retry_count = 0u32;

        let response = loop {
            debug!(
                "Using non-streaming completion (provider: {})",
                self.provider.id()
            );
            let result = tokio::select! {
                () = abort_token.cancelled() => {
                    return Err(anyhow::anyhow!("Cancelled"));
                }
                result = self.provider.complete(request.clone()) => result
            };

            match result {
                Ok(response) => break response,
                Err(e) => {
                    let err = e.to_string();
                    if is_retryable_error(&err) && retry_count < MAX_RETRIES {
                        retry_count += 1;
                        let delay = 1u64 << retry_count;
                        let reason = categorize_error(&err);
                        warn!(
                            "{}, retrying in {}s (attempt {}/{})",
                            reason, delay, retry_count, MAX_RETRIES
                        );
                        let _ = tx.send(AgentEvent::Retry(reason.to_string(), delay)).await;
                        // Interruptible sleep - check abort token during retry delay
                        tokio::select! {
                            () = abort_token.cancelled() => {
                                return Err(anyhow::anyhow!("Cancelled"));
                            }
                            () = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
                        }
                        continue;
                    }
                    return Err(anyhow::anyhow!("Completion error: {e}"));
                }
            }
        };

        let mut assistant_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        for block in response.content.iter() {
            match block {
                ContentBlock::Text { text } => {
                    let tokens = self.token_counter.count_str(text);
                    let _ = tx.send(AgentEvent::OutputTokensDelta(tokens)).await;
                    let _ = tx.send(AgentEvent::TextDelta(text.clone())).await;
                    assistant_blocks.push(block.clone());
                }
                ContentBlock::Thinking { thinking } => {
                    let _ = tx.send(AgentEvent::ThinkingDelta(thinking.clone())).await;
                    assistant_blocks.push(block.clone());
                }
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    let _ = tx
                        .send(AgentEvent::ToolCallStart(
                            id.clone(),
                            name.clone(),
                            arguments.clone(),
                        ))
                        .await;
                    tool_calls.push(ToolCallEvent {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    });
                    assistant_blocks.push(block.clone());
                }
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
        abort_token: CancellationToken,
    ) -> Result<Vec<ContentBlock>> {
        let mut set = JoinSet::new();
        let num_tools = tool_calls.len();

        if abort_token.is_cancelled() {
            return Err(anyhow::anyhow!("Cancelled"));
        }

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
                        let error_msg = e.to_string();
                        let _ = tx
                            .send(AgentEvent::ToolCallResult(
                                call.id.clone(),
                                error_msg.clone(),
                                true,
                            ))
                            .await;
                        ContentBlock::ToolResult {
                            tool_call_id: call.id,
                            content: error_msg,
                            is_error: true,
                        }
                    }
                };
                (index, block)
            });
        }

        let mut results = vec![None; num_tools];
        loop {
            tokio::select! {
                () = abort_token.cancelled() => {
                    set.abort_all();
                    return Err(anyhow::anyhow!("Cancelled"));
                }
                res = set.join_next() => {
                    match res {
                        Some(Ok(result)) => {
                            let (index, block) = result;
                            results[index] = Some(block);
                        }
                        Some(Err(e)) => {
                            // JoinError: task panicked or was cancelled
                            if e.is_panic() {
                                return Err(anyhow::anyhow!("Tool task panicked unexpectedly"));
                            }
                            return Err(anyhow::anyhow!("Tool task cancelled"));
                        }
                        None => break,
                    }
                }
            }
        }

        // Collect results, returning error if any slot is missing
        results
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("Tool execution incomplete - some results missing"))
    }
}

pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// Tool call started: (id, name, arguments)
    ToolCallStart(String, String, serde_json::Value),
    ToolCallResult(String, String, bool),
    PlanGenerated(crate::agent::designer::Plan),
    CompactionStatus {
        threshold: usize,
        pruned: bool,
    },
    TokenUsage {
        used: usize,
        max: usize,
    },
    InputTokens(usize),
    OutputTokensDelta(usize),
    /// Retry in progress: (reason, `delay_seconds`)
    Retry(String, u64),
    Finished(String),
    Error(String),
    ModelsFetched(Vec<crate::provider::ModelInfo>),
    ModelFetchError(String),
}

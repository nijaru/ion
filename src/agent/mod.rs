pub mod context;
mod events;
pub mod instructions;
mod retry;
mod stream;
pub mod subagent;
mod tools;

pub use events::AgentEvent;

use crate::agent::context::ContextManager;
use crate::agent::instructions::InstructionLoader;
use crate::compaction::{
    CompactionConfig, CompactionTier, TokenCounter, check_compaction_needed,
    compact_with_summarization,
};
use crate::provider::{ContentBlock, LlmApi, Message, Role, ThinkingConfig};
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::tool::ToolOrchestrator;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

const DEFAULT_SYSTEM_PROMPT: &str = "\
You are ion, a fast terminal coding agent. You help users with software engineering tasks: \
reading, editing, and creating files, running commands, and searching codebases. \
Be concise — under 4 lines for explanations, longer only for code. \
Never praise the user's question or idea. Prioritize action over explanation.

## Core Principles

- Simple-first: prefer the smallest local fix over a cross-file architecture change.
- Reuse-first: search for existing patterns before inventing new ones. Mirror naming, error handling, style.
- ALWAYS read code before modifying it. Prefer editing existing files over creating new ones.
- Make minimal, focused changes. Don't add features or refactoring beyond what was asked. \
Three similar lines of code is better than a premature abstraction.
- When deleting or moving code, remove it completely. No `// removed`, `// deprecated`, or compatibility shims.
- Comments for non-obvious context only. Don't add docstrings or comments to code you didn't change.
- Add error handling for real failure cases only. Don't handle impossible scenarios.
- Don't add new dependencies without asking.
- Implement completely. No placeholder code, no TODO comments.
- Don't introduce security vulnerabilities (injection, XSS, path traversal).

## Task Execution

You must keep going until the task is completely resolved. Do not stop at analysis or partial fixes. \
Carry changes through implementation, verification, and a clear explanation of outcomes. \
Persevere even when tool calls fail — retry with a different approach.

- Unless the user explicitly asks for a plan or explanation, assume they want you to make changes.
- Get context fast, then act. Stop exploring as soon as you can name the files and symbols to change. \
Trace only what you'll modify or depend on.
- Before tool calls, state what you're doing in 1-2 sentences.
- After changes, verify: run relevant tests, check compilation, or re-read changed files.
- Only ask when truly blocked — you cannot safely pick a reasonable default, the action is \
destructive and irreversible, or you need a credential. Never ask \"Should I proceed?\" — just do it.
- Do not guess or fabricate information. Use tools to find out.

## Tool Usage

Prefer specialized tools (read, edit, grep, glob) over bash equivalents. \
Use `bash` for builds, tests, git, and system commands.

- NEVER edit a file without reading it first.
- Run independent tool calls in parallel — multiple reads, searches, and diagnostics at once.
- No interactive shell commands (stdin prompts, pagers, editors). Use non-interactive flags \
(--yes, --no-pager, -y).
- Use the `directory` parameter in bash instead of `cd && cmd`.

## Output

- Reference files with line numbers: `src/main.rs:42`
- Don't use a colon immediately before a tool call.
- No emoji unless the user uses them first.
- No ANSI escape codes in text output.

## Safety

- Git: don't commit, branch, amend, force push, or skip hooks unless explicitly asked.
- Don't commit credentials, secrets, or .env files.
- Don't revert or discard changes you didn't make.
- Explain destructive commands before executing them.
- Respect AGENTS.md instructions from the project and user.";

#[derive(Clone)]
pub struct Agent {
    provider: Arc<dyn LlmApi>,
    orchestrator: Arc<ToolOrchestrator>,
    compaction_config: CompactionConfig,
    /// Dynamic context window size (updated when model changes)
    context_window: Arc<std::sync::atomic::AtomicUsize>,
    token_counter: TokenCounter,
    skills: Arc<tokio::sync::RwLock<SkillRegistry>>,
    context_manager: Arc<ContextManager>,
    /// Whether the current model supports vision (image inputs).
    supports_vision: Arc<std::sync::atomic::AtomicBool>,
    /// Cheap model for Tier 3 summarization (dynamically selected from model list).
    /// Falls back to session model when None.
    summarization_model: Arc<std::sync::Mutex<Option<String>>>,
}

/// Create instruction loader from current directory.
fn create_instruction_loader() -> Option<Arc<InstructionLoader>> {
    std::env::current_dir()
        .ok()
        .map(|cwd| Arc::new(InstructionLoader::new(cwd)))
}

/// Create context manager with optional instruction loader and working directory.
fn create_context_manager(system_prompt: String) -> ContextManager {
    let cwd = std::env::current_dir().ok();
    let mut cm = ContextManager::new(system_prompt);
    if let Some(loader) = create_instruction_loader() {
        cm = cm.with_instruction_loader(loader);
    }
    if let Some(ref dir) = cwd {
        cm = cm.with_working_dir(dir.clone());
    }
    cm
}

fn drain_queued_user_messages(
    session: &mut Session,
    message_queue: Option<&Arc<std::sync::Mutex<Vec<String>>>>,
) -> bool {
    let Some(queue) = message_queue else {
        return false;
    };

    // Handle poisoned lock by recovering inner data.
    let mut guard = match queue.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            warn!("Message queue lock was poisoned, recovering");
            poisoned.into_inner()
        }
    };
    if guard.is_empty() {
        return false;
    }

    let drained: Vec<String> = guard.drain(..).collect();
    drop(guard);

    for queued_msg in drained {
        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text { text: queued_msg }]),
        });
    }

    true
}

impl Agent {
    pub fn new(provider: Arc<dyn LlmApi>, orchestrator: Arc<ToolOrchestrator>) -> Self {
        let system_prompt = DEFAULT_SYSTEM_PROMPT.to_string();
        let compaction_config = CompactionConfig::default();
        let context_window = Arc::new(std::sync::atomic::AtomicUsize::new(
            compaction_config.context_window,
        ));

        let context_manager = create_context_manager(system_prompt);

        Self {
            provider,
            orchestrator,
            compaction_config,
            context_window,
            token_counter: TokenCounter::new(),
            skills: Arc::new(tokio::sync::RwLock::new(SkillRegistry::new())),
            context_manager: Arc::new(context_manager),
            supports_vision: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            summarization_model: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn with_compaction_config(mut self, config: CompactionConfig) -> Self {
        self.context_window
            .store(config.context_window, std::sync::atomic::Ordering::Relaxed);
        self.compaction_config = config;
        self
    }

    /// Manually compact messages with mechanical pruning only (Tier 1 + 2).
    ///
    /// Synchronous -- safe to call from event handlers.
    /// Returns the number of messages modified, or 0 if no pruning was needed.
    pub fn compact_messages(&self, messages: &mut [Message]) -> usize {
        use crate::compaction::prune_messages;

        let mut config = self.compaction_config.clone();
        config.context_window = self.context_window();

        let target = config.target_tokens();
        let result = prune_messages(messages, &config, &self.token_counter, target);
        result.messages_modified
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

    /// Update whether the current model supports vision.
    pub fn set_supports_vision(&self, val: bool) {
        self.supports_vision
            .store(val, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check if the current model supports vision.
    #[must_use]
    pub fn supports_vision(&self) -> bool {
        self.supports_vision
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set the model to use for Tier 3 summarization.
    ///
    /// When set, compaction uses this cheap model instead of the session's
    /// active model. Pass `None` to fall back to the session model.
    pub fn set_summarization_model(&self, model: Option<String>) {
        let mut guard = self
            .summarization_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = model;
    }

    /// Get the summarization model, falling back to the given session model.
    fn summarization_model_or(&self, session_model: &str) -> String {
        self.summarization_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| session_model.to_string())
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

    /// Access the context manager (e.g., to set MCP tool availability).
    pub fn context_manager(&self) -> &ContextManager {
        &self.context_manager
    }

    async fn emit_token_usage(&self, messages: &[Message], tx: &mpsc::Sender<AgentEvent>) {
        // Get system prompt (cached) without cloning messages
        let system_prompt = self.context_manager.get_system_prompt().await;

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
        // Estimate attachment token count for budget warning
        let attachment_tokens: usize = user_content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => TokenCounter::estimate_str(text),
                ContentBlock::Image { data, .. } => data.len() * 3 / 4 / 4,
                _ => 0,
            })
            .sum();

        session.messages.push(Message {
            role: Role::User,
            content: Arc::new(user_content),
        });

        // Send initial token usage
        self.emit_token_usage(&session.messages, &tx).await;

        // Warn if attachments consume >25% of context window
        let ctx_window = self.context_window();
        if ctx_window > 0 && attachment_tokens > ctx_window / 4 {
            let pct = attachment_tokens * 100 / ctx_window;
            let _ = tx
                .send(AgentEvent::Warning(format!(
                    "Attachments use ~{pct}% of context window (~{}k of {}k tokens)",
                    attachment_tokens / 1000,
                    ctx_window / 1000,
                )))
                .await;
        }

        loop {
            if session.abort_token.is_cancelled() {
                return (session, Some(anyhow::anyhow!("Cancelled")));
            }

            // Check for queued user messages between turns
            let had_queued = drain_queued_user_messages(&mut session, message_queue.as_ref());
            // Update token count if we added queued messages
            if had_queued {
                self.emit_token_usage(&session.messages, &tx).await;
            }

            match self.execute_turn(&mut session, &tx, thinking.clone()).await {
                Ok(true) => {}
                Ok(false) => {
                    // Final-answer turns can race with late user steering messages.
                    // Drain once more before exiting so queued input isn't lost.
                    let had_late_queued =
                        drain_queued_user_messages(&mut session, message_queue.as_ref());
                    if had_late_queued {
                        self.emit_token_usage(&session.messages, &tx).await;
                        continue;
                    }
                    break;
                }
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
            token_counter: &self.token_counter,
            supports_vision: self.supports_vision(),
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

        // Check if any tool call is the compact tool (agent-triggered compaction)
        let compact_tool_call_id = tool_calls
            .iter()
            .find(|tc| tc.name == crate::tool::builtin::COMPACT_TOOL_NAME)
            .map(|tc| tc.id.clone());
        let compact_requested = compact_tool_call_id.is_some();

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

        // Check for compaction: forced if compact tool was called, or automatic at threshold
        let mut config = self.compaction_config.clone();
        config.context_window = self.context_window();

        let needs_compaction = compact_requested
            || check_compaction_needed(&session.messages, &config, &self.token_counter)
                .needs_compaction;

        if needs_compaction {
            let summarization_model = self.summarization_model_or(&session.model);
            let result = compact_with_summarization(
                &mut session.messages,
                &config,
                &self.token_counter,
                self.provider.as_ref(),
                &summarization_model,
            )
            .await;

            if let Some(usage) = result.api_usage
                && (usage.input_tokens > 0 || usage.output_tokens > 0)
            {
                let _ = tx
                    .send(AgentEvent::ProviderUsage {
                        input_tokens: usage.input_tokens as usize,
                        output_tokens: usage.output_tokens as usize,
                        cache_read_tokens: usage.cache_read_tokens as usize,
                        cache_write_tokens: usage.cache_write_tokens as usize,
                    })
                    .await;
            }

            if result.tier_reached != CompactionTier::None {
                // Replace compact tool placeholder with actual result
                if let Some(ref call_id) = compact_tool_call_id {
                    let summary = format!(
                        "Compacted: {}k → {}k tokens ({:?})",
                        result.tokens_before / 1000,
                        result.tokens_after / 1000,
                        result.tier_reached,
                    );
                    replace_tool_result(&mut session.messages, call_id, &summary);
                }
                let _ = tx
                    .send(AgentEvent::CompactionStatus {
                        before: result.tokens_before,
                        after: result.tokens_after,
                    })
                    .await;
            }
        }

        Ok(true)
    }
}

/// Replace the content of a specific tool result in the message list.
fn replace_tool_result(messages: &mut [Message], tool_call_id: &str, new_content: &str) {
    for msg in messages.iter_mut().rev() {
        if msg.role != Role::ToolResult {
            continue;
        }
        let blocks: Vec<_> = msg
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::ToolResult {
                    tool_call_id: id,
                    is_error,
                    ..
                } if id == tool_call_id => ContentBlock::ToolResult {
                    tool_call_id: id.clone(),
                    content: new_content.to_string(),
                    is_error: *is_error,
                },
                other => other.clone(),
            })
            .collect();
        if blocks.iter().any(
            |b| matches!(b, ContentBlock::ToolResult { content, .. } if content == new_content),
        ) {
            msg.content = Arc::new(blocks);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::drain_queued_user_messages;
    use crate::provider::{ContentBlock, Role};
    use crate::session::Session;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn drain_queued_user_messages_none_queue() {
        let mut session = Session::new(PathBuf::from("."), "test-model".to_string());
        assert!(!drain_queued_user_messages(&mut session, None));
        assert!(session.messages.is_empty());
    }

    #[test]
    fn drain_queued_user_messages_appends_and_drains_in_order() {
        let mut session = Session::new(PathBuf::from("."), "test-model".to_string());
        let queue = Arc::new(Mutex::new(vec![
            "first message".to_string(),
            "second message".to_string(),
        ]));

        assert!(drain_queued_user_messages(&mut session, Some(&queue)));
        assert_eq!(session.messages.len(), 2);
        assert!(queue.lock().expect("queue lock").is_empty());

        let first = &session.messages[0];
        let second = &session.messages[1];
        assert!(matches!(first.role, Role::User));
        assert!(matches!(second.role, Role::User));
        assert!(matches!(
            first.content.first(),
            Some(ContentBlock::Text { text }) if text == "first message"
        ));
        assert!(matches!(
            second.content.first(),
            Some(ContentBlock::Text { text }) if text == "second message"
        ));
    }
}

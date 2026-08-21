//! Pure operation/session transition core (DESIGN.md §4.4, §10).
//!
//! No Tokio and no I/O. The [`OperationMachine`] is the single transition
//! authority for one active operation: `state + input → next state +
//! session entries + effect intents`. The `SessionRuntime` serializes
//! these transitions, executes the intents as spawned effects, and owns
//! the session entry log until the SQLite store takes over (§32 Step 2).

use crate::ids::OperationId;
use crate::tool::{ToolCall, ToolResult, ToolSpec};

/// Durably accepted input that has not yet been applied to model-visible
/// session history (DESIGN.md §6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InboxKind {
    /// Opens an operation when idle.
    Prompt,
    /// Joins the active operation; applied at the next safe continuation
    /// boundary (the next model step).
    Steer,
    /// Joins the active operation; applied after the current response
    /// reaches its follow-up boundary.
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InboxItem {
    pub kind: InboxKind,
    pub text: String,
}

/// Append-only semantic item (DESIGN.md §6). Streaming text deltas are
/// not entries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionEntry {
    UserMessage {
        text: String,
    },
    /// Only validated completed provider output becomes this entry.
    AssistantMessage {
        text: String,
    },
    ToolCall {
        call: ToolCall,
    },
    ToolResult {
        result: ToolResult,
    },
}

/// Total durable operation state (DESIGN.md §10.1). Only states with
/// distinct recovery semantics exist; approval, retry-wait, and
/// compaction states arrive with their owning phases.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OperationState {
    /// Prompt accepted durably; model step not yet started.
    Accepted,
    /// Inbox drained; ready to start a model step.
    NeedAssistant,
    /// Provider effect intent committed; awaiting outcome.
    AssistantEffectPending,
    /// Assistant completed with tool calls not yet admitted.
    ToolsPlanned {
        pending: Vec<ToolCall>,
    },
    /// Tool effect intent committed; awaiting settlement.
    ToolEffectPending {
        pending: Vec<ToolCall>,
    },
    /// Assistant completed without tool calls; accepted inbox items are
    /// pending, so the operation continues.
    NeedContinuation,
    /// Session closed while the operation was open; recoverable on
    /// restart (DESIGN.md §9.5). Not a user cancellation.
    Suspended,
    Finished(OperationOutcome),
}

/// Terminal outcomes (DESIGN.md §10.1). `Indeterminate` is produced only
/// by recovery of an unresolved NeverReplay effect (§32 Step 3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OperationOutcome {
    Completed,
    Failed(String),
    Cancelled,
    Indeterminate,
}

/// The durable fact that Ion is allowed to perform one repeat-sensitive
/// external effect (DESIGN.md §6, §12.1). Committed before execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EffectIntent {
    /// One model step: projected input + tool snapshot.
    ModelStep {
        operation_id: OperationId,
        prompt: String,
        tools: Vec<ToolSpec>,
    },
    /// One tool invocation with the exact effective arguments.
    Tool { call: ToolCall },
}

/// Inputs to the transition function. Provider/tool outcomes are fed by
/// the `SessionRuntime`; inbox and lifecycle inputs come from commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// Apply an accepted inbox item. Queues while an effect is pending;
    /// applies (as a user entry) at a safe continuation boundary.
    ApplyInbox {
        item: InboxItem,
    },
    /// Start the next model step from a quiescent state. `prompt` is the
    /// model-facing projection the runtime derived from the session
    /// transcript (placeholder for the ContextProjector, §14/§32 Step 4).
    StartModelStep {
        prompt: String,
    },
    /// A validated completed provider generation.
    ProviderCompleted {
        text: String,
        tool_calls: Vec<ToolCall>,
    },
    ProviderFailed {
        message: String,
    },
    ProviderCancelled,
    /// Admit the next planned tool: canonicalize, validate, commit intent.
    AdmitNextTool,
    /// A tool effect settled.
    ToolSettled {
        result: ToolResult,
    },
    /// Semantic cancellation request (DESIGN.md §9.4).
    CancelRequested,
    /// Harness failure (lost persistence, impossible invariant) from any
    /// open state; distinct from model-visible tool outcomes (§16.5,
    /// §26.2).
    FailOperation {
        message: String,
    },
    /// Recovery (§32 Step 3): re-issue the model step of an operation
    /// found pending after process loss. Provider generation with local
    /// canonical context is ReplaySafe (§12.2).
    RecoverModelStep {
        prompt: String,
    },
    /// Recovery: re-execute a ReplaySafe tool effect found pending after
    /// process loss, with its exact effective input.
    RecoverTool {
        call: ToolCall,
    },
    /// Recovery: an unresolved NeverReplay effect becomes indeterminate;
    /// the user/agent must inspect and decide (§12.2).
    SettleIndeterminate,
    /// Session close while operating (DESIGN.md §9.5).
    Suspend,
}

/// What one transition decided. The runtime executes intents as effects,
/// appends entries to the session log, and derives live events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub state: OperationState,
    pub entries: Vec<SessionEntry>,
    pub intents: Vec<EffectIntent>,
    /// The runtime must signal running effects to cancel.
    pub cancel_effects: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub state: &'static str,
    pub transition: &'static str,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "transition {} is invalid in state {}",
            self.transition, self.state
        )
    }
}

/// One active operation's transition authority. The session is idle when
/// no machine exists. Cloning stages a transition: a failed durable
/// commit discards the staged clone and never mutates live state
/// (DESIGN.md §26.2).
#[derive(Debug, Clone)]
pub struct OperationMachine {
    operation_id: OperationId,
    prompt: String,
    state: OperationState,
    /// Accepted steers, applied at the next reasoning boundary (the next
    /// model step, including tool continuations) — §9.2.
    steers: Vec<InboxItem>,
    /// Accepted follow-ups, applied only after the current response
    /// reaches its follow-up boundary (NeedContinuation) — §9.3.
    followups: Vec<InboxItem>,
    /// Immutable capability snapshot for every model step of this
    /// operation (DESIGN.md §18.2).
    tools: Vec<ToolSpec>,
    cancel_requested: bool,
}

impl OperationMachine {
    /// Accept a prompt as a new operation. The user entry is appended
    /// atomically with acceptance (DESIGN.md §9.1). `tools` is the
    /// capability snapshot frozen for this operation's model steps.
    #[must_use]
    pub fn accept(
        operation_id: OperationId,
        prompt: impl Into<String>,
        tools: Vec<ToolSpec>,
    ) -> (Self, Applied) {
        let prompt = prompt.into();
        let applied = Applied {
            state: OperationState::Accepted,
            entries: vec![SessionEntry::UserMessage {
                text: prompt.clone(),
            }],
            intents: Vec::new(),
            cancel_effects: false,
        };
        let machine = Self {
            operation_id,
            cancel_requested: false,
            state: OperationState::Accepted,
            steers: Vec::new(),
            followups: Vec::new(),
            tools,
            prompt,
        };
        (machine, applied)
    }

    /// Rebuild a machine from a durable checkpoint on reopen. Pending
    /// inbox items come from the store's pending inbox rows.
    #[must_use]
    pub fn restore(
        operation_id: OperationId,
        prompt: String,
        tools: Vec<ToolSpec>,
        state: OperationState,
        cancel_requested: bool,
        steers: Vec<InboxItem>,
        followups: Vec<InboxItem>,
    ) -> Self {
        Self {
            operation_id,
            prompt,
            state,
            steers,
            followups,
            tools,
            cancel_requested,
        }
    }

    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// The capability snapshot frozen for this operation's model steps
    /// (DESIGN.md §18.2).
    #[must_use]
    pub const fn frozen_tools(&self) -> &Vec<ToolSpec> {
        &self.tools
    }

    #[must_use]
    pub const fn state(&self) -> &OperationState {
        &self.state
    }

    #[must_use]
    pub const fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    /// True when queued steers wait for the next reasoning boundary.
    #[must_use]
    pub fn has_queued_steers(&self) -> bool {
        !self.steers.is_empty()
    }

    /// True when queued follow-ups wait for the follow-up boundary.
    #[must_use]
    pub fn has_queued_followups(&self) -> bool {
        !self.followups.is_empty()
    }

    /// True when any accepted inbox item is still pending.
    #[must_use]
    pub fn has_queued_inbox(&self) -> bool {
        self.has_queued_steers() || self.has_queued_followups()
    }

    /// Apply one transition. Invalid state/transition pairs are typed
    /// errors; nothing mutates on error.
    pub fn apply(&mut self, transition: Transition) -> Result<Applied, TransitionError> {
        match transition {
            Transition::ApplyInbox { item } => self.apply_inbox(item),
            Transition::StartModelStep { prompt } => self.start_model_step(prompt),
            Transition::ProviderCompleted { text, tool_calls } => {
                self.provider_completed(text, tool_calls)
            }
            Transition::ProviderFailed { message } => self.provider_failed(message),
            Transition::ProviderCancelled => self.provider_cancelled(),
            Transition::AdmitNextTool => self.admit_next_tool(),
            Transition::ToolSettled { result } => self.tool_settled(result),
            Transition::CancelRequested => self.cancel_requested_transition(),
            Transition::FailOperation { message } => self.fail_operation(message),
            Transition::RecoverModelStep { prompt } => self.recover_model_step(prompt),
            Transition::RecoverTool { call } => self.recover_tool(call),
            Transition::SettleIndeterminate => self.settle_indeterminate(),
            Transition::Suspend => self.suspend(),
        }
    }

    fn apply_inbox(&mut self, item: InboxItem) -> Result<Applied, TransitionError> {
        let applies_now = match item.kind {
            // Steer joins at the next reasoning boundary (§9.2).
            InboxKind::Steer => matches!(
                self.state,
                OperationState::Accepted
                    | OperationState::NeedAssistant
                    | OperationState::NeedContinuation
            ),
            // Follow-up waits for the response's follow-up boundary
            // (§9.3); it never joins between tool continuations.
            InboxKind::FollowUp => matches!(self.state, OperationState::NeedContinuation),
            InboxKind::Prompt => {
                return Err(TransitionError {
                    state: state_name(&self.state),
                    transition: "apply_inbox",
                });
            }
        };
        if applies_now {
            self.state = OperationState::NeedAssistant;
            Ok(Applied {
                state: self.state.clone(),
                entries: vec![SessionEntry::UserMessage { text: item.text }],
                intents: Vec::new(),
                cancel_effects: false,
            })
        } else if matches!(
            self.state,
            OperationState::Accepted
                | OperationState::NeedAssistant
                | OperationState::NeedContinuation
                | OperationState::AssistantEffectPending
                | OperationState::ToolsPlanned { .. }
                | OperationState::ToolEffectPending { .. }
        ) {
            match item.kind {
                InboxKind::Steer => self.steers.push(item),
                InboxKind::FollowUp => self.followups.push(item),
                InboxKind::Prompt => unreachable!("rejected above"),
            }
            Ok(Applied {
                state: self.state.clone(),
                entries: Vec::new(),
                intents: Vec::new(),
                cancel_effects: false,
            })
        } else {
            Err(TransitionError {
                state: state_name(&self.state),
                transition: "apply_inbox",
            })
        }
    }

    fn start_model_step(&mut self, prompt: String) -> Result<Applied, TransitionError> {
        match self.state {
            OperationState::Accepted | OperationState::NeedAssistant => {
                self.state = OperationState::AssistantEffectPending;
                Ok(Applied {
                    state: self.state.clone(),
                    entries: Vec::new(),
                    intents: vec![EffectIntent::ModelStep {
                        operation_id: self.operation_id,
                        prompt,
                        tools: self.tools.clone(),
                    }],
                    cancel_effects: false,
                })
            }
            ref state => Err(TransitionError {
                state: state_name(state),
                transition: "start_model_step",
            }),
        }
    }

    fn provider_completed(
        &mut self,
        text: String,
        tool_calls: Vec<ToolCall>,
    ) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::AssistantEffectPending) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "provider_completed",
            });
        }
        let mut entries = vec![SessionEntry::AssistantMessage { text }];
        if tool_calls.is_empty() {
            if self.has_queued_inbox() {
                self.state = OperationState::NeedContinuation;
            } else {
                self.state = OperationState::Finished(OperationOutcome::Completed);
            }
        } else {
            for call in &tool_calls {
                entries.push(SessionEntry::ToolCall { call: call.clone() });
            }
            self.state = OperationState::ToolsPlanned {
                pending: tool_calls,
            };
        }
        Ok(Applied {
            state: self.state.clone(),
            entries,
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn provider_failed(&mut self, message: String) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::AssistantEffectPending) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "provider_failed",
            });
        }
        self.state = OperationState::Finished(OperationOutcome::Failed(message));
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn provider_cancelled(&mut self) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::AssistantEffectPending) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "provider_cancelled",
            });
        }
        self.state = OperationState::Finished(OperationOutcome::Cancelled);
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn admit_next_tool(&mut self) -> Result<Applied, TransitionError> {
        let pending = match &self.state {
            OperationState::ToolsPlanned { pending } => pending.clone(),
            state => {
                return Err(TransitionError {
                    state: state_name(state),
                    transition: "admit_next_tool",
                });
            }
        };
        let Some(call) = pending.first() else {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "admit_next_tool",
            });
        };
        let rest = pending[1..].to_vec();
        self.state = OperationState::ToolEffectPending { pending: rest };
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: vec![EffectIntent::Tool { call: call.clone() }],
            cancel_effects: false,
        })
    }

    fn tool_settled(&mut self, result: ToolResult) -> Result<Applied, TransitionError> {
        let pending = match &self.state {
            OperationState::ToolEffectPending { pending } => pending.clone(),
            state => {
                return Err(TransitionError {
                    state: state_name(state),
                    transition: "tool_settled",
                });
            }
        };
        self.state = if self.cancel_requested {
            OperationState::Finished(OperationOutcome::Cancelled)
        } else if pending.is_empty() {
            OperationState::NeedAssistant
        } else {
            OperationState::ToolsPlanned { pending }
        };
        Ok(Applied {
            state: self.state.clone(),
            entries: vec![SessionEntry::ToolResult { result }],
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn cancel_requested_transition(&mut self) -> Result<Applied, TransitionError> {
        if matches!(self.state, OperationState::Finished(_)) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "cancel_requested",
            });
        }
        self.cancel_requested = true;
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: true,
        })
    }

    fn fail_operation(&mut self, message: String) -> Result<Applied, TransitionError> {
        if matches!(
            self.state,
            OperationState::Finished(_) | OperationState::Suspended
        ) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "fail_operation",
            });
        }
        self.state = OperationState::Finished(OperationOutcome::Failed(message));
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: true,
        })
    }

    fn recover_model_step(&mut self, prompt: String) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::AssistantEffectPending) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "recover_model_step",
            });
        }
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: vec![EffectIntent::ModelStep {
                operation_id: self.operation_id,
                prompt,
                tools: self.tools.clone(),
            }],
            cancel_effects: false,
        })
    }

    fn recover_tool(&mut self, call: ToolCall) -> Result<Applied, TransitionError> {
        match &self.state {
            OperationState::ToolEffectPending { .. } => {}
            state => {
                return Err(TransitionError {
                    state: state_name(state),
                    transition: "recover_tool",
                });
            }
        }
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: vec![EffectIntent::Tool { call }],
            cancel_effects: false,
        })
    }

    fn settle_indeterminate(&mut self) -> Result<Applied, TransitionError> {
        if !matches!(
            self.state,
            OperationState::AssistantEffectPending | OperationState::ToolEffectPending { .. }
        ) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "settle_indeterminate",
            });
        }
        self.state = OperationState::Finished(OperationOutcome::Indeterminate);
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: true,
        })
    }

    fn suspend(&mut self) -> Result<Applied, TransitionError> {
        if matches!(self.state, OperationState::Finished(_)) {
            return Err(TransitionError {
                state: state_name(&self.state),
                transition: "suspend",
            });
        }
        self.state = OperationState::Suspended;
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: true,
        })
    }

    /// Drain queued steers at a reasoning boundary. Each applied item
    /// moves the machine to [`OperationState::NeedAssistant`].
    pub fn drain_steers(&mut self) -> Result<Vec<Applied>, TransitionError> {
        let mut applied = Vec::new();
        while let Some(item) = self.steers.first().cloned() {
            self.steers.remove(0);
            applied.push(self.apply_inbox(item)?);
        }
        Ok(applied)
    }

    /// Drain queued follow-ups at the follow-up boundary
    /// ([`OperationState::NeedContinuation`]).
    pub fn drain_followups(&mut self) -> Result<Vec<Applied>, TransitionError> {
        let mut applied = Vec::new();
        while let Some(item) = self.followups.first().cloned() {
            self.followups.remove(0);
            applied.push(self.apply_inbox(item)?);
        }
        Ok(applied)
    }
}

/// Derive the model-facing input for one model step from the session
/// transcript. Deterministic: the same entries always project to the
/// same prompt (DESIGN.md §31 invariant 15). Placeholder for the
/// ContextProjector and ContextManifest (§14, §32 Step 4).
#[must_use]
pub fn project_transcript(entries: &[SessionEntry]) -> String {
    let mut out = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        match entry {
            SessionEntry::UserMessage { text } => {
                out.push_str("user: ");
                out.push_str(text);
            }
            SessionEntry::AssistantMessage { text } => {
                out.push_str("assistant: ");
                out.push_str(text);
            }
            SessionEntry::ToolCall { call } => {
                out.push_str("tool_call: ");
                out.push_str(&call.name);
                out.push('(');
                out.push_str(&call.arguments.to_string());
                out.push(')');
            }
            SessionEntry::ToolResult { result } => {
                out.push_str("tool_result: ");
                match result {
                    crate::tool::ToolResult::Ok { output, .. } => out.push_str(output),
                    crate::tool::ToolResult::Err { error, .. } => out.push_str(error),
                }
            }
        }
    }
    out
}

fn state_name(state: &OperationState) -> &'static str {
    match state {
        OperationState::Accepted => "accepted",
        OperationState::NeedAssistant => "need_assistant",
        OperationState::AssistantEffectPending => "assistant_effect_pending",
        OperationState::ToolsPlanned { .. } => "tools_planned",
        OperationState::ToolEffectPending { .. } => "tool_effect_pending",
        OperationState::NeedContinuation => "need_continuation",
        OperationState::Suspended => "suspended",
        OperationState::Finished(_) => "finished",
    }
}

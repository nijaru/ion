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
    /// Start the next model step from a quiescent state.
    StartModelStep,
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
    /// Accepted inbox items not yet applied to session history.
    inbox: Vec<InboxItem>,
    /// Model-facing input accumulated so far (prompt, then applied
    /// steers/follow-ups); every model step of the operation projects the
    /// full accumulated input. Replaced by the ContextProjector in
    /// DESIGN.md §32 Step 4.
    projected: String,
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
            inbox: Vec::new(),
            cancel_requested: false,
            state: OperationState::Accepted,
            projected: prompt.clone(),
            tools,
            prompt,
        };
        (machine, applied)
    }

    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    #[must_use]
    pub const fn state(&self) -> &OperationState {
        &self.state
    }

    #[must_use]
    pub const fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    /// True when queued inbox items wait for a continuation boundary.
    #[must_use]
    pub fn has_queued_inbox(&self) -> bool {
        !self.inbox.is_empty()
    }

    /// Apply one transition. Invalid state/transition pairs are typed
    /// errors; nothing mutates on error.
    pub fn apply(&mut self, transition: Transition) -> Result<Applied, TransitionError> {
        match transition {
            Transition::ApplyInbox { item } => self.apply_inbox(item),
            Transition::StartModelStep => self.start_model_step(),
            Transition::ProviderCompleted { text, tool_calls } => {
                self.provider_completed(text, tool_calls)
            }
            Transition::ProviderFailed { message } => self.provider_failed(message),
            Transition::ProviderCancelled => self.provider_cancelled(),
            Transition::AdmitNextTool => self.admit_next_tool(),
            Transition::ToolSettled { result } => self.tool_settled(result),
            Transition::CancelRequested => self.cancel_requested_transition(),
            Transition::FailOperation { message } => self.fail_operation(message),
            Transition::Suspend => self.suspend(),
        }
    }

    fn apply_inbox(&mut self, item: InboxItem) -> Result<Applied, TransitionError> {
        match self.state {
            OperationState::Accepted
            | OperationState::NeedAssistant
            | OperationState::NeedContinuation => {
                self.projected.push('\n');
                self.projected.push_str(&item.text);
                self.state = OperationState::NeedAssistant;
                Ok(Applied {
                    state: self.state.clone(),
                    entries: vec![SessionEntry::UserMessage { text: item.text }],
                    intents: Vec::new(),
                    cancel_effects: false,
                })
            }
            OperationState::AssistantEffectPending
            | OperationState::ToolsPlanned { .. }
            | OperationState::ToolEffectPending { .. } => {
                self.inbox.push(item);
                Ok(Applied {
                    state: self.state.clone(),
                    entries: Vec::new(),
                    intents: Vec::new(),
                    cancel_effects: false,
                })
            }
            ref state => Err(TransitionError {
                state: state_name(state),
                transition: "apply_inbox",
            }),
        }
    }

    fn start_model_step(&mut self) -> Result<Applied, TransitionError> {
        match self.state {
            OperationState::Accepted | OperationState::NeedAssistant => {
                let prompt = self.projected.clone();
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
            if self.inbox.is_empty() {
                self.state = OperationState::Finished(OperationOutcome::Completed);
            } else {
                self.state = OperationState::NeedContinuation;
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

    /// Drain queued inbox items at a continuation boundary. Each applied
    /// item moves the machine to [`OperationState::NeedAssistant`].
    pub fn drain_inbox(&mut self) -> Result<Vec<Applied>, TransitionError> {
        let mut applied = Vec::new();
        while let Some(item) = self.inbox.first().cloned() {
            self.inbox.remove(0);
            applied.push(self.apply_inbox(item)?);
        }
        Ok(applied)
    }
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

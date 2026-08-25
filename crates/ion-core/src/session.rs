//! Pure operation/session transition core (DESIGN.md §4.4, §10).
//!
//! No Tokio and no I/O. The [`OperationMachine`] is the single transition
//! authority for one active operation: `state + input → next state +
//! session entries + effect intents`. The `SessionRuntime` serializes
//! these transitions, executes the intents as spawned effects, and owns
//! the session entry log until the SQLite store takes over (§32 Step 2).

use crate::context::ContextPlan;
use crate::ids::OperationId;
use crate::provider::ModelConfig;
use crate::tool::{ToolCall, ToolResult, ToolSpec};

/// Durably accepted input that has not yet been applied to model-visible
/// session history (DESIGN.md §6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InboxKind {
    /// Root input for an accepted operation.
    Prompt,
    /// Joins the active operation; applied at the next safe continuation
    /// boundary (the next model step).
    Steer,
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
    /// Readable compaction baseline (DESIGN.md §14.7): the summary
    /// replaces entries through `covers_through_seq` in the model
    /// projection only; canonical entries stay durable.
    Compaction {
        covers_through_seq: u64,
        summary: String,
    },
    UserMessage {
        text: String,
    },
    /// A durably accepted model selection. It applies only to model
    /// steps started after this entry; any in-flight step keeps its
    /// frozen [`ModelConfig`].
    ModelChanged {
        model_ref: String,
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
    /// A compaction effect intent is committed; the provider is
    /// producing the readable summary (DESIGN.md §14.7). Canonical
    /// history is untouched; the summary becomes a projection baseline.
    CompactionPending,
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
    /// Non-interactive policy gate: a concrete action needs an approval
    /// no one can grant in this mode, so the operation terminates with
    /// a clear record instead of inviting a model retry loop
    /// (DESIGN.md §17.4).
    ApprovalRequired {
        tool: String,
    },
}

/// The durable fact that Ion is allowed to perform one repeat-sensitive
/// external effect (DESIGN.md §6, §12.1). Committed before execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EffectIntent {
    /// One model step: projected input + frozen model/tool snapshot.
    ModelStep {
        operation_id: OperationId,
        model: ModelConfig,
        plan: ContextPlan,
        tools: Vec<ToolSpec>,
    },
    /// One tool invocation with the exact effective arguments.
    Tool { call: ToolCall },
    /// One compaction step: the provider summarizes the projected
    /// history; the result becomes a readable Compaction entry
    /// (DESIGN.md §14.7). ReplaySafe (§12.5): provider generation over
    /// local canonical context.
    Compaction {
        operation_id: OperationId,
        plan: ContextPlan,
    },
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
    /// The provider rejected the step because the context exceeded the
    /// window (14.7.4): settle the failed attempt without entries and
    /// move to compaction; the retry is the natural continuation after
    /// the summary baseline lands.
    OverflowCompaction {
        plan: ContextPlan,
    },
    /// Start the next model step from a quiescent state. `plan` is the
    /// model-facing projection the runtime derived from the session
    /// transcript (deterministic, §14/§31 invariant 15).
    StartModelStep {
        model: ModelConfig,
        plan: ContextPlan,
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
        model: ModelConfig,
        plan: ContextPlan,
    },
    /// Recovery: re-execute a ReplaySafe tool effect found pending after
    /// process loss, with its exact effective input.
    RecoverTool {
        call: ToolCall,
    },
    /// Recovery: an unresolved NeverReplay effect becomes indeterminate;
    /// the user/agent must inspect and decide (§12.2).
    SettleIndeterminate,
    /// The policy gate requires an approval that cannot be granted in
    /// this mode; the operation terminates before any effect intent is
    /// committed (DESIGN.md §17.4).
    ApprovalRequired {
        tool: String,
    },
    /// Begin a compaction step at a continuation boundary (§14.7).
    StartCompaction {
        plan: ContextPlan,
    },
    /// A validated compaction generation: the summary becomes a
    /// readable semantic entry covering history through
    /// `covers_through_seq`; canonical entries stay durable.
    CompactionCompleted {
        summary: String,
        covers_through_seq: u64,
    },
    /// The compaction generation failed or was cancelled. Compaction is
    /// maintenance, so the operation continues without a baseline
    /// unless a cancellation was requested (§14.7, P13 via tracing).
    CompactionFailed,
    /// Recovery (§32 Step 3): re-issue a pending compaction effect.
    /// Provider generation over local canonical context is ReplaySafe.
    RecoverCompaction {
        plan: ContextPlan,
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
    /// Accepted steers, applied at the next reasoning boundary (the next
    /// model step, including tool continuations) — §9.2.
    steers: Vec<InboxItem>,
    /// Capability snapshot for the current model step. The runtime replaces
    /// this at each safe model-step boundary (DESIGN.md §18.2).
    step_tools: Vec<ToolSpec>,
    cancel_requested: bool,
}

impl OperationMachine {
    /// Accept a prompt as a new operation. The user entry is appended
    /// atomically with acceptance (DESIGN.md §9.1). `tools` is the initial
    /// capability snapshot; the runtime replaces it for each model step.
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
            step_tools: tools,
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
    ) -> Self {
        Self {
            operation_id,
            prompt,
            state,
            steers,
            step_tools: tools,
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

    /// Peek the next planned tool call without admitting it. The
    /// runtime policy gate inspects the canonical invocation before any
    /// effect intent is committed (DESIGN.md §17.3).
    #[must_use]
    pub fn next_planned_call(&self) -> Option<&ToolCall> {
        match &self.state {
            OperationState::ToolsPlanned { pending } => pending.first(),
            _ => None,
        }
    }

    /// The capability snapshot used for the current model step
    /// (DESIGN.md §18.2).
    #[must_use]
    pub const fn step_tools(&self) -> &Vec<ToolSpec> {
        &self.step_tools
    }

    /// Replace the capability snapshot at a model-step boundary. A staged
    /// machine is updated before its transition is committed, so a failed
    /// persistence write leaves the live snapshot unchanged.
    pub fn set_step_tools(&mut self, tools: Vec<ToolSpec>) {
        self.step_tools = tools;
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

    /// Apply one transition. Invalid state/transition pairs are typed
    /// errors; nothing mutates on error.
    pub fn apply(&mut self, transition: Transition) -> Result<Applied, TransitionError> {
        match transition {
            Transition::ApplyInbox { item } => self.apply_inbox(item),
            Transition::StartModelStep { model, plan } => self.start_model_step(model, plan),
            Transition::ProviderCompleted { text, tool_calls } => {
                self.provider_completed(text, tool_calls)
            }
            Transition::ProviderFailed { message } => self.provider_failed(message),
            Transition::ProviderCancelled => self.provider_cancelled(),
            Transition::AdmitNextTool => self.admit_next_tool(),
            Transition::ToolSettled { result } => self.tool_settled(result),
            Transition::CancelRequested => self.cancel_requested_transition(),
            Transition::FailOperation { message } => self.fail_operation(message),
            Transition::RecoverModelStep { model, plan } => self.recover_model_step(model, plan),
            Transition::RecoverTool { call } => self.recover_tool(call),
            Transition::SettleIndeterminate => self.settle_indeterminate(),
            Transition::ApprovalRequired { tool } => self.approval_required(tool),
            Transition::StartCompaction { plan } => self.start_compaction(plan),
            Transition::RecoverCompaction { plan } => self.recover_compaction(plan),
            Transition::CompactionCompleted {
                summary,
                covers_through_seq,
            } => self.compaction_completed(summary, covers_through_seq),
            Transition::CompactionFailed => self.compaction_failed(),
            Transition::OverflowCompaction { plan } => self.overflow_compaction(plan),
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
            InboxKind::Prompt => {
                return Err(TransitionError {
                    state: self.state.kind(),
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
                state: self.state.kind(),
                transition: "apply_inbox",
            })
        }
    }

    fn start_model_step(
        &mut self,
        model: ModelConfig,
        plan: ContextPlan,
    ) -> Result<Applied, TransitionError> {
        match self.state {
            OperationState::Accepted | OperationState::NeedAssistant => {
                self.state = OperationState::AssistantEffectPending;
                Ok(Applied {
                    state: self.state.clone(),
                    entries: Vec::new(),
                    intents: vec![EffectIntent::ModelStep {
                        operation_id: self.operation_id,
                        model,
                        plan,
                        tools: self.step_tools.clone(),
                    }],
                    cancel_effects: false,
                })
            }
            ref state => Err(TransitionError {
                state: state.kind(),
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
                state: self.state.kind(),
                transition: "provider_completed",
            });
        }
        let mut entries = vec![SessionEntry::AssistantMessage { text }];
        if tool_calls.is_empty() {
            if self.has_queued_steers() {
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
                state: self.state.kind(),
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
                state: self.state.kind(),
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
                    state: state.kind(),
                    transition: "admit_next_tool",
                });
            }
        };
        let Some(call) = pending.first() else {
            return Err(TransitionError {
                state: self.state.kind(),
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
                    state: state.kind(),
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
                state: self.state.kind(),
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
                state: self.state.kind(),
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

    fn recover_model_step(
        &mut self,
        model: ModelConfig,
        plan: ContextPlan,
    ) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::AssistantEffectPending) {
            return Err(TransitionError {
                state: self.state.kind(),
                transition: "recover_model_step",
            });
        }
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: vec![EffectIntent::ModelStep {
                operation_id: self.operation_id,
                model,
                plan,
                tools: self.step_tools.clone(),
            }],
            cancel_effects: false,
        })
    }

    fn recover_tool(&mut self, call: ToolCall) -> Result<Applied, TransitionError> {
        match &self.state {
            OperationState::ToolEffectPending { .. } => {}
            state => {
                return Err(TransitionError {
                    state: state.kind(),
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
                state: self.state.kind(),
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

    fn approval_required(&mut self, tool: String) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::ToolsPlanned { .. }) {
            return Err(TransitionError {
                state: self.state.kind(),
                transition: "approval_required",
            });
        }
        self.state = OperationState::Finished(OperationOutcome::ApprovalRequired { tool });
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn start_compaction(&mut self, plan: ContextPlan) -> Result<Applied, TransitionError> {
        match self.state {
            OperationState::NeedAssistant | OperationState::NeedContinuation => {
                self.state = OperationState::CompactionPending;
                Ok(Applied {
                    state: self.state.clone(),
                    entries: Vec::new(),
                    intents: vec![EffectIntent::Compaction {
                        operation_id: self.operation_id,
                        plan,
                    }],
                    cancel_effects: false,
                })
            }
            ref state => Err(TransitionError {
                state: state.kind(),
                transition: "start_compaction",
            }),
        }
    }

    fn compaction_completed(
        &mut self,
        summary: String,
        covers_through_seq: u64,
    ) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::CompactionPending) {
            return Err(TransitionError {
                state: self.state.kind(),
                transition: "compaction_completed",
            });
        }
        self.state = OperationState::NeedAssistant;
        Ok(Applied {
            state: self.state.clone(),
            entries: vec![SessionEntry::Compaction {
                covers_through_seq,
                summary,
            }],
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn recover_compaction(&mut self, plan: ContextPlan) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::CompactionPending) {
            return Err(TransitionError {
                state: self.state.kind(),
                transition: "recover_compaction",
            });
        }
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: vec![EffectIntent::Compaction {
                operation_id: self.operation_id,
                plan,
            }],
            cancel_effects: false,
        })
    }

    fn overflow_compaction(&mut self, plan: ContextPlan) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::AssistantEffectPending) {
            return Err(TransitionError {
                state: self.state.kind(),
                transition: "overflow_compaction",
            });
        }
        self.state = OperationState::CompactionPending;
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: vec![EffectIntent::Compaction {
                operation_id: self.operation_id,
                plan,
            }],
            cancel_effects: false,
        })
    }

    fn compaction_failed(&mut self) -> Result<Applied, TransitionError> {
        if !matches!(self.state, OperationState::CompactionPending) {
            return Err(TransitionError {
                state: self.state.kind(),
                transition: "compaction_failed",
            });
        }
        self.state = if self.cancel_requested {
            OperationState::Finished(OperationOutcome::Cancelled)
        } else {
            OperationState::NeedAssistant
        };
        Ok(Applied {
            state: self.state.clone(),
            entries: Vec::new(),
            intents: Vec::new(),
            cancel_effects: false,
        })
    }

    fn suspend(&mut self) -> Result<Applied, TransitionError> {
        if matches!(self.state, OperationState::Finished(_)) {
            return Err(TransitionError {
                state: self.state.kind(),
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
}

impl OperationState {
    /// Stable lowercase name used in diagnostics and durable rows.
    #[must_use]
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            OperationState::Accepted => "accepted",
            OperationState::NeedAssistant => "need_assistant",
            OperationState::AssistantEffectPending => "assistant_effect_pending",
            OperationState::ToolsPlanned { .. } => "tools_planned",
            OperationState::ToolEffectPending { .. } => "tool_effect_pending",
            OperationState::NeedContinuation => "need_continuation",
            OperationState::CompactionPending => "compaction_pending",
            OperationState::Suspended => "suspended",
            OperationState::Finished(_) => "finished",
        }
    }
}

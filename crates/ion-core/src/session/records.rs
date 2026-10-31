//! Durable Session entry schema and public inspection records.

use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SessionEntry {
    /// Select a validated historical context without rewriting execution facts.
    ContextSelected {
        head_entry: u64,
    },
    ReasoningSelected {
        reasoning: Reasoning,
    },
    ModelSelected {
        model: ModelRef,
    },
    /// Effective physical model selected for the next coding request. This is
    /// distinct from the logical Session selection and advances provider replay
    /// epochs when the physical model changes.
    EffectiveModelChanged {
        turn: u64,
        model: ModelRef,
    },
    /// The selected provider could not safely reuse older opaque continuation
    /// after a changed request context. Raw assistant facts remain intact.
    ProviderReplayRebased {
        turn: u64,
    },
    /// Provider-neutral, model-visible request context at this point in the
    /// Session. Execution routes remain request-bound host state and are never
    /// persisted here.
    ModelContextChanged {
        turn: u64,
        context: ModelContextSnapshot,
    },
    UserShellAdmitted {
        command: String,
        exclude_from_context: bool,
    },
    UserShellSettled {
        /// Sequence of the admitted command, never a host-supplied identity.
        admission_entry: u64,
        outcome: UserShellOutcome,
    },
    Steering {
        turn: u64,
        input: Message,
    },
    /// Automated readiness, distinct from submitted or steered user input.
    Completion {
        turn: u64,
        notices: Vec<crate::CompletionNotice>,
    },
    /// Provider usage from a best-effort prompt-cache refresh. This is an
    /// accounting fact only and never enters model context.
    CacheWarm {
        turn: u64,
        execution: ModelExecution,
        #[serde(default = "Usage::unknown")]
        usage: Usage,
    },
    Compacted {
        through_entry: u64,
        summary: String,
        execution: ModelExecution,
        #[serde(default = "Usage::unknown")]
        usage: Usage,
    },
    TurnStarted {
        turn: u64,
        input: Message,
        model: ModelRef,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        completion: Vec<crate::CompletionNotice>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<RequestKey>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<RecoveryBinding>,
    },
    /// Explicit continuation admitted against this exact prior checkpoint.
    TurnResumed {
        turn: u64,
        checkpoint: u64,
    },
    Assistant {
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        #[serde(default = "Usage::unknown")]
        usage: Usage,
        #[serde(default = "completed_termination")]
        termination: ResponseTermination,
    },
    ToolResult {
        turn: u64,
        result: crate::ToolSettlement,
    },
    ChildToolAdmitted {
        turn: u64,
        intent: crate::ChildIntent,
    },
    ChildToolResult {
        turn: u64,
        parent: crate::ToolOccurrence,
        child: usize,
        outcome: crate::ChildOutcome,
    },
    /// Parent relationship intent, committed before child materialization.
    DelegationAdmitted {
        record: crate::DelegationRecord,
    },
    DelegationTransferred {
        run: uuid::Uuid,
        owner: uuid::Uuid,
    },
    DelegationSettled {
        run: uuid::Uuid,
        outcome: crate::DelegationOutcome,
    },
    /// Actual request binding; a reused conversation retains earlier bindings.
    ChildSessionOf {
        record: crate::DelegationRecord,
    },
    TurnEnded {
        turn: u64,
        reason: TurnEndReason,
    },
}

/// An observed host result is distinct from closure after lost observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserShellOutcome {
    Observed {
        output: serde_json::Value,
        is_error: bool,
    },
    Unknown,
}

impl UserShellOutcome {
    pub fn unknown_notice() -> &'static str {
        "Shell result was not committed; external effect unknown. Inspect the working directory before retrying."
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextSnapshot {
    pub instructions: String,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToolActivity {
    pub call_id: String,
    pub activity: ToolActivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Interrupted,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPoint {
    BeforeTurn(u64),
    AfterTurn(u64),
}

/// Passive inspection of an admission without a committed host observation.
/// Dispatch and physical termination cannot be inferred from this state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "effect", rename = "unknown")]
pub struct UnobservedUserShell {
    pub admission_entry: u64,
}

/// Coherent operational status without reading or cloning conversation history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStatus {
    pub cwd: Option<PathBuf>,
    pub name: Option<String>,
    pub reasoning: Reasoning,
    pub entries: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub id: uuid::Uuid,
    pub cwd: Option<PathBuf>,
    pub name: Option<String>,
    pub entries: Vec<SessionEntry>,
    pub messages: Vec<Message>,
    /// Original entry coordinates on the selected context path, in order.
    pub context_path: Vec<u64>,
    pub unfinished_turn: Option<u64>,
    pub unfinished_user_shell: Option<UnobservedUserShell>,
    pub last_end: Option<(u64, TurnEndReason)>,
    pub last_model: Option<ModelRef>,
    pub reasoning: Reasoning,
    pub last_effective_model: Option<ModelRef>,
    pub last_context: Option<ModelContextSnapshot>,
    pub compacted_through: Option<u64>,
    pub last_execution: Option<ModelExecution>,
    pub last_usage: Option<Usage>,
}

#[derive(Debug, Clone)]
pub struct TurnSummary {
    pub turn: u64,
    pub reasoning: Reasoning,
    pub input: Message,
    pub model: ModelRef,
    pub end: Option<TurnEndReason>,
}

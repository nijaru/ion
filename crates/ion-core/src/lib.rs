//! Ion durable session kernel: a provider-neutral coding turn engine.
//!
//! The durable nouns are a session, its conversations, immutable entries,
//! accepted inputs and the turns that answer them. A turn owns model-step
//! continuation, tool invocation, resource accounting, cancellation and its
//! terminal outcome. There is no generic task graph, no resident state mirror
//! and no second runtime.

mod attempt;
mod config;
mod conversation;
mod entry;
mod error;
mod id;
mod input;
mod invocation;
mod limits;
mod request;
mod session;
mod store;
mod tool;
mod turn;
mod view;

pub use attempt::{AttemptState, ModelAttempt, ModelStep};
pub use config::{
    ConfigError, ContextPolicy, ConversationConfig, InstalledConfig, MAX_ATTEMPTS_PER_STEP,
    RunLimits,
};
pub use conversation::{Conversation, HistoryParent};
pub use entry::{ASSISTANT_ENTRY, Entry, EntryKind, EntryKindError, INPUT_ENTRY, TOOL_ENTRY};
pub use error::{Error, Result};
pub use id::{
    AttemptId, CommitSeq, ConversationId, EntryId, IdError, InputId, InvocationId, SessionId,
    StepId, TurnId,
};
pub use input::{
    EntryPlacement, Input, InputBody, InputDisposition, InputMode, InputPlacement, InputSender,
    RequestKey, RequestKeyError,
};
pub use invocation::{InvocationOutcome, InvocationState, Resolution, ToolInvocation};
pub use limits::{LimitsError, SessionLimits};
pub use request::{AssembledRequest, RequestError};
pub use session::{
    AdmissionReceipt, CancelReceipt, ConfigureRequest, EntryQuery, ResolveRequest, Services,
    Session, SessionEvent, SessionHandle, SessionSpec, SessionWatch, SubmitRequest, WatchError,
};
pub use tool::{ScriptedTool, Tool, ToolOutcome, ToolRegistry};
pub use turn::{Cancellation, PendingOutcome, Turn, TurnFailure, TurnOutcome, TurnPhase};
pub use view::{EntryPage, TurnView};

/// The oldest page size this build will serve. Larger requests are clamped.
pub const MAX_ENTRY_PAGE: u32 = store::sqlite::entry::MAX_ENTRY_PAGE;

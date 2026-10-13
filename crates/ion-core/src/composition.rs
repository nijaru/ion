//! Confined program protocol. Session authority never crosses this boundary.
use std::time::Duration;

use ion_ai::{ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    tool_result::ToolOutput,
    tool_set::{ToolActivity, ToolDefinition, ToolExposure, ToolPresentation},
};

pub const CODE_MODE_NAME: &str = "code_mode";

/// One occurrence, independent of provider IDs reused in later assistant steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOccurrence {
    pub assistant_entry: u64,
    pub ordinal: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct CodeLimits {
    pub heap_bytes: usize,
    pub stack_bytes: usize,
    /// Cumulative runnable guest wall time, enforced by the runtime. Excludes
    /// idle waits for bridge replies, not guest code running alongside them.
    pub execution_budget: Duration,
    pub max_code_bytes: usize,
    pub max_json_bytes: usize,
    pub max_calls: usize,
    pub max_concurrency: usize,
    pub max_host_bytes: usize,
    /// Stop further dispatch after this much child audit. Already started
    /// outcomes still commit: this admission threshold can overshoot, not cut.
    pub audit_admission_bytes: usize,
}

impl Default for CodeLimits {
    fn default() -> Self {
        Self {
            heap_bytes: 64 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            execution_budget: Duration::from_secs(30),
            max_code_bytes: 64 * 1024,
            max_json_bytes: 1024 * 1024,
            max_calls: 64,
            max_concurrency: 4,
            max_host_bytes: 8 * 1024 * 1024,
            audit_admission_bytes: 32 * 1024 * 1024,
        }
    }
}

pub type CodeReply = Result<String, String>;
pub enum CodeRequestKind {
    Call { name: String, args_json: String },
    DescribeTool { name: String },
    SearchTools { query: String, limit: usize },
    Inspect { query_json: String },
}
pub struct CodeRequest {
    pub kind: CodeRequestKind,
    pub reply: oneshot::Sender<CodeReply>,
}

/// Close the request sender when the worker exits. Guest failure cancels `stop`;
/// successful return leaves transferred work alive. No Session/catalogue enters
/// the worker. The coding operation retains this task until it joins.
pub struct CodeTask {
    pub requests: mpsc::Receiver<CodeRequest>,
    pub result: JoinHandle<CodeReply>,
}
/// Owns guest budget enforcement and cancellation responsiveness. Charge all
/// runnable guest work cumulatively, including reply resolution/serialization;
/// only idle bridge waits may be excluded. Signal failure before closing the
/// sender, and do not cancel transferred effects on successful guest return.
pub trait CodeRuntime: Send + Sync {
    fn start(
        &self,
        code: String,
        limits: CodeLimits,
        stop: CancellationToken,
    ) -> Result<CodeTask, String>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChildOutcome {
    Observed { output: ToolOutput },
    NotDispatched { reason: String },
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildIntent {
    pub parent: ToolOccurrence,
    pub child: usize,
    pub call: ToolCall,
    pub definition: ToolSpec,
    pub activity: ToolActivity,
}

impl ChildOutcome {
    pub fn inspection_output(self) -> ToolOutput {
        match self {
            Self::Observed { output } => output,
            Self::NotDispatched { reason } => {
                failure(format!("Child tool was not dispatched: {reason}"))
            }
            Self::Unknown => failure(
                "The child tool result was not committed. Its external effect is unknown; inspect the working directory before retrying.",
            ),
        }
    }
}

pub(crate) fn failure(message: impl Into<String>) -> ToolOutput {
    ToolOutput {
        value: json!({"error":message.into()}),
        images: Vec::new(),
        is_error: true,
    }
}

pub(crate) fn definition(limits: CodeLimits) -> ToolDefinition {
    ToolDefinition {
        spec: ToolSpec {
            name: CODE_MODE_NAME.into(),
            description: format!(
                "Run an async JavaScript function body without ambient filesystem, network, modules or timers. Await tools.call(name,args) for {{value,is_error,image_mime_types}}; searchTools(query,options?) returns matching {{name,description}} candidates; options accepts only limit, an integer 1..10 (default 10), with exact ASCII-insensitive names ranked first; describeTool(name) returns the exact case-sensitive frozen ToolSpec (name/description/input_schema) or null for unknown/non-guest capabilities. Both include callable deferred tools, exclude intrinsic tool_search/code_mode, and are read-only, not activation or effects. Return JSON selected for the model; raw child outputs stay inspectable and are not automatically added to model context. tools.inspect({{kind:'parents'}}) discovers up to 10 settled parents with saved children, newest first; pass next_before as before for older locators, including after compaction/reopen. tools.inspect({{kind:'children',parent}}) pages saved child metadata; pass next_after as after for more. tools.inspect({{kind:'output',parent,child,pointer:'/stdout_full_path'}}) returns UTF-8 chunks of encoded saved output.value as json with next_offset and total_bytes; pointer defaults to the whole value, offset to 0, limit to 4096 (4..65536 bytes). parent is a Session-scoped locator from the prior envelope or parents page; pointer is a JSON Pointer. Inspection is read-only, excludes image bytes and general history, and preserves observed/not_dispatched/unknown states. Return only selected evidence. Observed native tool failures set is_error in the reply envelope; invalid bridge requests, unknown/non-guest capabilities and gateway faults reject rather than return envelopes. Transferred requests settle even if unawaited. Failure/cancellation stops new dispatch and awaits started effects; no rollback or script replay. Limits: {}ms cumulative runnable guest budget (idle bridge waits excluded), {} bridge requests, {} concurrent tools, {} byte JS heap, {} byte JSON value, {} cumulative host reply bytes. Direct tools remain available.",
                limits.execution_budget.as_millis(),
                limits.max_calls,
                limits.max_concurrency,
                limits.heap_bytes,
                limits.max_json_bytes,
                limits.max_host_bytes
            ),
            input_schema: json!({"type":"object","additionalProperties":false,"required":["code"],"properties":{"code":{"type":"string","maxLength":limits.max_code_bytes}}}),
        },
        presentation: ToolPresentation::static_target(
            crate::ToolActivityKind::External,
            "JavaScript",
        ),
        exposure: ToolExposure::Direct,
    }
}

//! Confined program protocol. Session authority never crosses this boundary.
use std::time::Duration;

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    ToolOccurrence,
    tool_result::ToolOutput,
    tool_set::{ToolActivity, ToolDefinition, ToolExposure, ToolPresentation},
};

pub const CODE_MODE_NAME: &str = "code_mode";

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CodeLimits {
    pub heap_bytes: usize,
    pub stack_bytes: usize,
    /// Cumulative runnable guest wall time, enforced by the runtime. Excludes
    /// idle waits for bridge replies, not guest code running alongside them.
    pub execution_budget: Duration,
    pub max_code_bytes: usize,
    pub max_json_bytes: usize,
    /// Combined selected JSON return, text items and image attachments.
    pub max_output_bytes: usize,
    pub max_calls: usize,
    pub max_concurrency: usize,
    /// Encoded callable-name manifest plus cumulative JSON replies delivered to the VM.
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
            max_output_bytes: 8 * 1024 * 1024,
            max_calls: 64,
            max_concurrency: 4,
            max_host_bytes: 8 * 1024 * 1024,
            audit_admission_bytes: 32 * 1024 * 1024,
        }
    }
}

pub type CodeReply = Result<String, String>;

/// Explicit guest selections, never an instruction to execute or replay a tool.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodeOutput {
    Text {
        text: String,
    },
    /// Resolves only against this invocation's committed child observations.
    Image {
        id: String,
    },
}

/// Selected output survives an ordinary script error; infrastructure faults
/// still prevent parent consumption. `result` is the JSON return or diagnostic.
pub struct CodeResult {
    pub result: CodeReply,
    pub output: Vec<CodeOutput>,
}

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
/// successful return leaves transferred work alive. No Session or executable
/// catalogue enters the worker. The coding operation retains this task until it joins.
/// `result` owns that join and completes only after worker teardown, not merely
/// after receiving a guest result. Cancelling an await must retain this future.
pub struct CodeTask {
    pub requests: mpsc::Receiver<CodeRequest>,
    pub result: BoxFuture<'static, Result<CodeResult, String>>,
}
/// Owns guest budget enforcement and cancellation responsiveness. Charge all
/// runnable guest work cumulatively, including reply resolution/serialization;
/// only idle bridge waits may be excluded. Signal failure before closing the
/// sender, and do not cancel transferred effects on successful guest return.
/// A waiting guest must not occupy capacity required by its host bridge work.
/// `tools_json` is the complete bounded JSON array of exact callable names from
/// the frozen catalogue, not executable authority. Its bytes are charged against
/// the same JSON-delivery budget as replies; publish no partial namespace.
pub trait CodeRuntime: Send + Sync {
    fn start(
        &self,
        code: String,
        tools_json: String,
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
                concat!(
                    "Run an async JavaScript function body without ambient filesystem, network, modules or timers. ",
                    "Await tools.name(args), or tools[name](args) for exact names needing brackets, for {{value,is_error,images}}. ",
                    "tools is a frozen null-prototype namespace of all callable direct/deferred tools, without aliases; then/toJSON names prevent startup to avoid implicit dispatch. ",
                    "Unknown members are undefined. ",
                    "searchTools(query,options?) returns matching {{name,description}} candidates; options accepts only limit, an integer 1..10 (default 10), with exact ASCII-insensitive names ranked first; describeTool(name) returns the exact case-sensitive frozen ToolSpec (name/description/input_schema) or null for unknown/non-guest capabilities. ",
                    "Both include callable deferred tools, exclude intrinsic tool_search/code_mode, and are read-only, not activation or effects. ",
                    "Return JSON and/or call text(value) and image(block) to select output for the model. text renders primitives as strings and objects as JSON. ",
                    "images contains small invocation-scoped {{type:'image',id,mime_type}} blocks; image(block) selects a committed image from this invocation without copying bytes through JavaScript. It does not accept URLs, raw image data or historical locators. ",
                    "No explicit return means null. Selected output survives ordinary script errors and is marked with the parent failure; raw unselected child outputs stay inspectable, not automatically added to model context. ",
                    "inspectSaved({{kind:'parents'}}) discovers up to 10 settled parents with saved children, newest first; pass next_before as before for older locators, including after compaction/reopen. ",
                    "inspectSaved({{kind:'children',parent}}) pages saved child metadata; pass next_after as after for more. ",
                    "inspectSaved({{kind:'output',parent,child,pointer:'/stdout_full_path'}}) returns UTF-8 chunks of encoded saved output.value as json with next_offset and total_bytes; pointer defaults to the whole value, offset to 0, limit to 4096 (4..65536 bytes). ",
                    "parent is a Session-scoped locator from the prior envelope or parents page; pointer is a JSON Pointer. ",
                    "Inspection is read-only, excludes image bytes and general history, and preserves observed/not_dispatched/unknown states. ",
                    "Return only selected evidence. ",
                    "Observed native tool failures set is_error in the reply envelope; invalid bridge requests, unknown/non-guest capabilities and gateway faults reject rather than return envelopes. ",
                    "Transferred requests settle even if unawaited. ",
                    "Failure/cancellation stops new dispatch and awaits started effects; no rollback or script replay. ",
                    "Limits: {}ms cumulative runnable guest budget (idle bridge waits excluded), {} bridge requests, {} concurrent tools, {} byte JS heap, {} byte JSON value, {} cumulative host JSON bytes including the name manifest. ",
                    "Selected return, text and image attachments share a {} byte output limit. Exceeding output limits fails the script even if caught. ",
                    "Direct tools remain available.",
                ),
                limits.execution_budget.as_millis(),
                limits.max_calls,
                limits.max_concurrency,
                limits.heap_bytes,
                limits.max_json_bytes,
                limits.max_host_bytes,
                limits.max_output_bytes
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

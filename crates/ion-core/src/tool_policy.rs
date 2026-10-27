//! Application policy over one admitted call, never a mutable execution grant.
use ion_ai::{BoxFuture, ToolCall};
use tokio_util::sync::CancellationToken;

use crate::{ToolDefinition, ToolOrigin};

/// Immutable facts from the issuing request's frozen catalog. The origin is
/// provenance, not authority or permission to replay. No Session writer,
/// delegation port, model context or executable route is exposed.
pub struct ToolPolicyRequest<'a> {
    pub call: &'a ToolCall,
    pub definition: &'a ToolDefinition,
    pub origin: ToolOrigin,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ToolDecision {
    Allow,
    Deny { reason: String },
}

/// Optional application-owned veto before a selected executor or intrinsic
/// route runs. Core has checked the call envelope and frozen capability, not
/// every tool's complete JSON Schema; executors retain their argument checks.
///
/// Errors and cancellation fail closed. Retain and join started policy work
/// through Stop; callers await this future rather than dropping it. The local
/// stop scope cannot cancel its parent. Denial reasons and errors become durable,
/// model-visible refusals; do not return private diagnostics. Decisions cannot replace arguments or
/// results, add capabilities or authorize later invocations. Trusted host code
/// still has host permissions; this interface is not a sandbox.
pub trait ToolPolicy: Send + Sync {
    fn decide<'a>(
        &'a self,
        request: ToolPolicyRequest<'a>,
        stop: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolDecision, String>>;
}

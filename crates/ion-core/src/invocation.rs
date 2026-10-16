//! Engine-issued tool provenance, distinct from application authority.
use ion_ai::ToolCall;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// One committed assistant call, independent of provider IDs reused in later steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOccurrence {
    /// One-based committed Session entry sequence.
    pub assistant_entry: u64,
    /// Zero-based tool-call ordinal, excluding non-call content parts.
    pub ordinal: usize,
}

/// Where Core dispatched an admitted call. Provider call IDs and model arguments
/// are not execution identity. A composed child is a nested tool, not an agent.
///
/// This is historical provenance, not a live execution lease, authorization,
/// replay permission or proof that an effect completed. Copies of Sessions have
/// different logical identities even when they contain the same history prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct ToolOrigin {
    session: uuid::Uuid,
    turn: u64,
    occurrence: ToolOccurrence,
    composed_child: Option<usize>,
}

impl ToolOrigin {
    pub fn session(self) -> uuid::Uuid {
        self.session
    }

    pub fn turn(self) -> u64 {
        self.turn
    }

    /// The direct assistant call, or the parent Code Mode call for a child.
    pub fn occurrence(self) -> ToolOccurrence {
        self.occurrence
    }

    /// The committed Code Mode child index; absent for a direct invocation.
    pub fn composed_child(self) -> Option<usize> {
        self.composed_child
    }
}

/// One frozen route receives its original call, cancellation and optional Core
/// provenance together. Only Core can associate admitted provenance with a call.
/// Core does not pass this value to the Code Mode guest or add its provenance
/// to tool arguments. Applications still control what their tool outputs disclose.
/// Trusted in-process executors may forward an invocation; this is not isolation
/// from malicious host code. The public API cannot rebind its call/provenance:
///
/// ```compile_fail
/// use ion_core::ToolInvocation;
/// fn rebind<'a>(original: ToolInvocation<'a>, call: &'a ion_ai::ToolCall)
///     -> ToolInvocation<'a>
/// {
///     ToolInvocation {
///         call,
///         stop: original.stop().clone(),
///         origin: original.origin(),
///     }
/// }
/// ```
pub struct ToolInvocation<'a> {
    call: &'a ToolCall,
    stop: CancellationToken,
    origin: Option<ToolOrigin>,
    delegation: Option<crate::delegation::AdmissionPort>,
}

impl<'a> ToolInvocation<'a> {
    /// Low-level execution without Session admission. This neither records an
    /// intent/outcome nor grants any authority. Tools requiring engine provenance
    /// must reject its absent origin; native tools still use host permissions.
    pub fn untracked(call: &'a ToolCall, stop: CancellationToken) -> Self {
        Self {
            call,
            stop,
            origin: None,
            delegation: None,
        }
    }

    pub fn call(&self) -> &'a ToolCall {
        self.call
    }

    pub fn stop(&self) -> &CancellationToken {
        &self.stop
    }

    pub fn origin(&self) -> Option<ToolOrigin> {
        self.origin
    }

    /// Request one child with explicit context through this live invocation. Exact
    /// repetition reconciles; different work conflicts. Cancelling this borrowed
    /// wait does not revoke a transferred request or abandon started supervision.
    /// Historical origins alone cannot make a request, and closed ports cannot
    /// start another child. Foreground returns only after joining; background
    /// returns a committed transfer, not completion. Later outcomes belong to
    /// the destination owner and Session facts. Infrastructure failure blocks
    /// the parent before release, or closes the destination after transfer,
    /// even when tool code catches an error.
    pub async fn delegate(
        &self,
        task: crate::DelegationTask,
    ) -> Result<crate::DelegationReceipt, String> {
        let port = self
            .delegation
            .as_ref()
            .ok_or("untracked invocation cannot delegate")?;
        let (reply, result) = tokio::sync::oneshot::channel();
        port.send(crate::delegation::DelegationRequest { task, reply })
            .await
            .map_err(|_| "delegation admission is closed".to_owned())?;
        result
            .await
            .map_err(|_| "delegation owner ended without a receipt".to_owned())?
    }

    /// Callers must have committed the direct intent, and any child intent,
    /// before constructing an invocation. Construction itself executes nothing.
    pub(crate) fn admitted(
        call: &'a ToolCall,
        stop: CancellationToken,
        session: uuid::Uuid,
        turn: u64,
        occurrence: ToolOccurrence,
        composed_child: Option<usize>,
    ) -> (Self, crate::delegation::gateway::Gateway) {
        let (port, gateway) = crate::delegation::gateway::Gateway::new(crate::DelegationSite {
            session,
            turn,
            tool: occurrence,
            composed_child,
        });
        (
            Self {
                call,
                stop,
                delegation: Some(port),
                origin: Some(ToolOrigin {
                    session,
                    turn,
                    occurrence,
                    composed_child,
                }),
            },
            gateway,
        )
    }
}

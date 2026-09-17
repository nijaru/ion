//! Host tools: the engine's only path to an external action.
//!
//! A tool declares its implementation identity, its repeat-safety policy and
//! what happened. It cannot declare authority: selecting a tool in a
//! conversation configuration grants nothing, and an invocation is admitted
//! only because the host registered an implementation for it.
//!
//! The three outcomes are deliberately distinct. A known failure is a fact the
//! model may read and recover from. An indeterminate outcome is not: the action
//! may have happened, so the engine stops rather than repeating it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use ion_ai::{BoxFuture, ToolCall, ToolSpec};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// A request that a running action stop.
///
/// Stopping is a request, not an interruption. `request` may race the action's
/// own completion, and a tool that cannot stop itself is permitted to keep
/// running: what it may not do is claim an outcome it did not establish. The
/// session keeps ownership of an action that has not reported, so nothing here
/// releases a workspace claim or promises that an external effect ended.
#[derive(Clone, Debug)]
pub struct Stop {
    token: CancellationToken,
}

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop {
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    /// Ask the action to stop. Idempotent, and safe to call before it starts.
    pub fn request(&self) {
        self.token.cancel();
    }

    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once a stop has been requested.
    pub async fn requested(&self) {
        self.token.cancelled().await;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    /// The action completed. The value is the result the model reads.
    Completed(Value),
    /// The action did not happen, or happened without effect, and the reason is
    /// known.
    KnownFailure(String),
    /// The action may have happened and its outcome cannot be established.
    Indeterminate(String),
}

pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// The implementation behind the tool's name, including its revision.
    ///
    /// Recorded with every invocation so a later build with a different
    /// implementation cannot reinterpret old prepared arguments.
    fn identity(&self) -> String;

    /// Whether the recorded policy permits repeating this action after its
    /// outcome became unknown. Defaults to `false`: an action that mutates the
    /// world is not repeated because a name looked idempotent.
    fn repeat_safe(&self) -> bool {
        false
    }

    /// Perform the action, honoring `stop`.
    ///
    /// A tool that stops before it took effect returns
    /// [`ToolOutcome::KnownFailure`] with that fact; a tool that cannot
    /// establish whether it happened returns [`ToolOutcome::Indeterminate`]
    /// rather than guessing. Returning promptly after a stop is what lets a
    /// cancelled turn record a truthful outcome instead of leaving the action
    /// unresolved.
    fn execute<'a>(&'a self, call: &'a ToolCall, stop: &'a Stop) -> BoxFuture<'a, ToolOutcome>;
}

/// The tools a session can execute, keyed by the name a model uses.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool.spec().name, tool);
        self
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Resolve the specs a frozen basis selected.
    ///
    /// A selected name with no registered implementation is an error: dropping
    /// it would silently change the request the basis promised to make.
    pub fn specs(&self, names: &[String]) -> Result<Vec<ToolSpec>, MissingTool> {
        let mut specs = Vec::with_capacity(names.len());
        for name in names {
            match self.tools.get(name) {
                Some(tool) => specs.push(tool.spec()),
                None => return Err(MissingTool { name: name.clone() }),
            }
        }
        Ok(specs)
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no implementation is registered for tool {name:?}")]
pub struct MissingTool {
    pub name: String,
}

/// A tool whose results a test (or a scripted harness) states in advance.
///
/// It exists for the same reason the scripted model service does: the engine's
/// continuation, cancellation and recovery rules are only observable if the
/// boundary they drive is deterministic.
pub struct ScriptedTool {
    name: String,
    identity: String,
    repeat_safe: bool,
    outcomes: Mutex<VecDeque<ToolOutcome>>,
    calls: Mutex<Vec<ToolCall>>,
}

impl ScriptedTool {
    #[must_use]
    pub fn new(name: impl Into<String>, outcomes: impl IntoIterator<Item = ToolOutcome>) -> Self {
        let name = name.into();
        Self {
            identity: format!("{name}@scripted-1"),
            name,
            repeat_safe: false,
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn repeat_safe(mut self, repeat_safe: bool) -> Self {
        self.repeat_safe = repeat_safe;
        self
    }

    /// Every call this tool received, including calls whose script ran out.
    #[must_use]
    pub fn calls(&self) -> Vec<ToolCall> {
        self.calls.lock().expect("call mutex").clone()
    }

    /// How many calls have not been answered by the script yet.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.outcomes.lock().expect("outcome mutex").len()
    }
}

impl Tool for ScriptedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: format!("scripted tool {}", self.name),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn repeat_safe(&self) -> bool {
        self.repeat_safe
    }

    fn execute<'a>(&'a self, call: &'a ToolCall, _stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.calls.lock().expect("call mutex").push(call.clone());
            self.outcomes
                .lock()
                .expect("outcome mutex")
                .pop_front()
                .unwrap_or_else(|| {
                    ToolOutcome::Indeterminate(format!(
                        "scripted tool {} has no scripted outcome left",
                        self.name
                    ))
                })
        })
    }
}

mod context;
mod invocation;
mod kind;
mod output;
mod record;
mod registry;

pub use context::{AbortContext, TaskContext, TaskContextError};
pub(crate) use context::{ContextFuture, TaskRuntime};
pub use invocation::{InvocationKind, TaskInvocation};
pub use kind::{RunningTask, TaskCompletion, TaskFuture, TaskKind, TaskRunError};
pub use output::TaskOutput;
pub use record::{
    TaskKindName, TaskKindNameError, TaskOutcome, TaskOutcomeKind, TaskRecord, TaskStatus,
};
pub use registry::{TaskRegistry, TaskRegistryError};

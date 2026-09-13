mod context;
mod invocation;
mod kind;
mod output;
mod plan;
mod record;
mod registry;
mod typed;

pub use context::{AbortContext, TaskContext, TaskContextError};
pub(crate) use context::{ContextFuture, TaskRuntime};
pub use invocation::{InvocationKind, TaskInvocation};
pub use kind::{ResourceDomain, RunningTask, TaskCompletion, TaskFuture, TaskKind, TaskRunError};
pub use output::TaskOutput;
pub use plan::{PlannedEntry, PlannedTask, PlannedTaskRef, TaskDependency, TaskPlan};
pub use record::{
    TaskKindName, TaskKindNameError, TaskOutcome, TaskOutcomeKind, TaskRecord, TaskStatus,
};
pub use registry::{TaskRegistry, TaskRegistryError};
pub(crate) use typed::erase as erase_typed;
pub use typed::{
    TypedAbortContext, TypedContext, TypedFuture, TypedHandler, TypedOutcome, TypedReport,
    TypedTask,
};

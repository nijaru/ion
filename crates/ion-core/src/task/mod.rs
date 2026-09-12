mod invocation;
mod output;
mod record;

pub use invocation::{InvocationKind, TaskInvocation};
pub use output::TaskOutput;
pub use record::{
    TaskKindName, TaskKindNameError, TaskOutcome, TaskOutcomeKind, TaskRecord, TaskStatus,
};

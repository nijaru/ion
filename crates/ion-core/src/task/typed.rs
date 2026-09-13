use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    AbortContext, InvocationKind, RunningTask, TaskCompletion, TaskContext, TaskContextError,
    TaskKind, TaskOutcomeKind, TaskOutput, TaskPlan, TaskRunError,
};
use crate::{CommitSeq, ConversationId, TaskId};

/// A typed task authoring surface.
///
/// Implement this and register with [`TaskRegistry::register_typed`]. It erases
/// into the ordinary [`TaskKind`] registry: the same session writer, scheduler,
/// cancellation and finalization apply. This is not a second task framework.
///
/// `Input` and `Checkpoint` are decoded from their durable `serde_json::Value`
/// representation. A decode failure settles a structured terminal `Failed`
/// outcome rather than panicking or fabricating success.
///
/// [`TaskRegistry::register_typed`]: super::TaskRegistry::register_typed
pub trait TypedHandler: Send + Sync + 'static {
    type Input: std::fmt::Debug + DeserializeOwned + Send;
    type Checkpoint: std::fmt::Debug + Serialize + DeserializeOwned + Send;
    type Completed: std::fmt::Debug + Serialize + Send;
    type Failure: std::fmt::Debug + Serialize + Send;
    type Aborted: std::fmt::Debug + Serialize + Send;

    /// Optional scarce-resource domain held only while an eligible invocation
    /// runs. Defaults to no additional limit.
    fn resource_domain(&self) -> Option<super::ResourceDomain> {
        None
    }

    fn execute(&self, task: TypedTask<Self>, context: TypedContext<Self>) -> TypedFuture<'_, Self>
    where
        Self: Sized;

    fn recover(&self, task: TypedTask<Self>, context: TypedContext<Self>) -> TypedFuture<'_, Self>
    where
        Self: Sized;

    fn abort(
        &self,
        task: TypedTask<Self>,
        context: TypedAbortContext<Self>,
    ) -> TypedFuture<'_, Self>
    where
        Self: Sized;
}

pub type TypedFuture<'a, H> =
    Pin<Box<dyn Future<Output = Result<TypedReport<H>, TaskRunError>> + Send + 'a>>;

/// A decoded view of the durable task, with typed input and checkpoint.
#[derive(Debug)]
pub struct TypedTask<H: TypedHandler> {
    pub id: TaskId,
    pub conversation_id: ConversationId,
    pub input: H::Input,
    pub checkpoint: Option<H::Checkpoint>,
    pub output: Option<TaskOutput>,
    pub generation: u64,
    pub invocation_kind: InvocationKind,
    pub reservation_commit: CommitSeq,
}

/// The typed terminal result. Completed, failed and aborted payloads are
/// distinct durable shapes; `Indeterminate` carries retained raw evidence for
/// work whose external effect may have happened but cannot be reconciled.
#[derive(Debug)]
pub enum TypedOutcome<H: TypedHandler> {
    Completed(H::Completed),
    Failed(H::Failure),
    Aborted(H::Aborted),
    Indeterminate(Value),
}

#[derive(Debug)]
pub struct TypedReport<H: TypedHandler> {
    pub outcome: TypedOutcome<H>,
    pub output: Option<TaskOutput>,
    pub plan: TaskPlan,
}

impl<H: TypedHandler> TypedReport<H> {
    fn new(outcome: TypedOutcome<H>) -> Self {
        Self {
            outcome,
            output: None,
            plan: TaskPlan::new(),
        }
    }

    #[must_use]
    pub fn completed(value: H::Completed) -> Self {
        Self::new(TypedOutcome::Completed(value))
    }

    #[must_use]
    pub fn failed(value: H::Failure) -> Self {
        Self::new(TypedOutcome::Failed(value))
    }

    #[must_use]
    pub fn aborted(value: H::Aborted) -> Self {
        Self::new(TypedOutcome::Aborted(value))
    }

    #[must_use]
    pub fn indeterminate(evidence: Value) -> Self {
        Self::new(TypedOutcome::Indeterminate(evidence))
    }

    /// Commit successor entries/tasks atomically with this result.
    #[must_use]
    pub fn with_plan(mut self, plan: TaskPlan) -> Self {
        self.plan = plan;
        self
    }

    #[must_use]
    pub fn with_output(mut self, output: TaskOutput) -> Self {
        self.output = Some(output);
        self
    }
}

/// Typed checkpoint commits for `execute`/`recover`.
pub struct TypedContext<H: TypedHandler> {
    inner: TaskContext,
    _handler: PhantomData<fn() -> H>,
}

impl<H: TypedHandler> TypedContext<H> {
    pub(crate) fn new(inner: TaskContext) -> Self {
        Self {
            inner,
            _handler: PhantomData,
        }
    }

    pub async fn checkpoint(
        &self,
        checkpoint: Option<H::Checkpoint>,
        output: Option<TaskOutput>,
    ) -> Result<CommitSeq, TaskContextError> {
        let encoded = match checkpoint {
            Some(checkpoint) => Some(serde_json::to_value(checkpoint).map_err(encode_error)?),
            None => None,
        };
        self.inner.checkpoint(encoded, output).await
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}

/// Typed checkpoint commits for `abort`, without the normal cancellation token.
pub struct TypedAbortContext<H: TypedHandler> {
    inner: AbortContext,
    _handler: PhantomData<fn() -> H>,
}

impl<H: TypedHandler> TypedAbortContext<H> {
    pub(crate) fn new(inner: AbortContext) -> Self {
        Self {
            inner,
            _handler: PhantomData,
        }
    }

    pub async fn checkpoint(
        &self,
        checkpoint: Option<H::Checkpoint>,
        output: Option<TaskOutput>,
    ) -> Result<CommitSeq, TaskContextError> {
        let encoded = match checkpoint {
            Some(checkpoint) => Some(serde_json::to_value(checkpoint).map_err(encode_error)?),
            None => None,
        };
        self.inner.checkpoint(encoded, output).await
    }
}

/// Erased bridge from [`TypedHandler`] to the kernel [`TaskKind`].
struct ErasedKind<H: TypedHandler> {
    handler: Arc<H>,
}

impl<H: TypedHandler> TaskKind for ErasedKind<H> {
    fn resource_domain(&self) -> Option<super::ResourceDomain> {
        self.handler.resource_domain()
    }

    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> super::TaskFuture<'a> {
        // `execute` only runs for a never-dispatched task, so a decode failure
        // cannot hide an external effect.
        let typed = match self.decode(&task) {
            Ok(typed) => typed,
            Err(failure) => return Box::pin(async move { Ok(failure.into_completion()) }),
        };
        let handler = self.handler.clone();
        Box::pin(async move { run_typed(handler.execute(typed, TypedContext::new(context)).await) })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> super::TaskFuture<'a> {
        // The task was dispatched. An undecodable durable checkpoint does not
        // prove the external effect is absent, so recovery is interrupted and
        // the task stays recoverable rather than being terminalized.
        let typed = match self.decode(&task) {
            Ok(typed) => typed,
            Err(failure) => return Box::pin(async move { Err(failure.into_interruption()) }),
        };
        let handler = self.handler.clone();
        Box::pin(async move { run_typed(handler.recover(typed, TypedContext::new(context)).await) })
    }

    fn abort<'a>(&'a self, task: RunningTask, context: AbortContext) -> super::TaskFuture<'a> {
        // Same reasoning as `recover`: cleanup must not be settled by a decode
        // failure. The task remains cancelled and abortable.
        let typed = match self.decode(&task) {
            Ok(typed) => typed,
            Err(failure) => return Box::pin(async move { Err(failure.into_interruption()) }),
        };
        let handler = self.handler.clone();
        Box::pin(
            async move { run_typed(handler.abort(typed, TypedAbortContext::new(context)).await) },
        )
    }
}

impl<H: TypedHandler> ErasedKind<H> {
    fn decode(&self, task: &RunningTask) -> Result<TypedTask<H>, DecodeFailure> {
        let input = decode_value::<H::Input>("input", &task.input)?;
        let checkpoint = match &task.checkpoint {
            Some(value) => Some(decode_value::<H::Checkpoint>("checkpoint", value)?),
            None => None,
        };
        Ok(TypedTask {
            id: task.id,
            conversation_id: task.conversation_id,
            input,
            checkpoint,
            output: task.output.clone(),
            generation: task.generation,
            invocation_kind: task.invocation_kind,
            reservation_commit: task.reservation_commit,
        })
    }
}

/// A durable payload that does not decode into its typed shape. Small so it can
/// be an `Err` without boxing; the adapter turns it into a terminal completion.
struct DecodeFailure {
    field: &'static str,
    message: String,
}

impl DecodeFailure {
    fn into_completion(self) -> TaskCompletion {
        TaskCompletion::failed(json!({
            "code": "decode_failed",
            "field": self.field,
            "message": self.message,
        }))
    }

    fn into_interruption(self) -> TaskRunError {
        TaskRunError::Interrupted(format!(
            "durable {} does not decode for the registered schema: {}",
            self.field, self.message
        ))
    }
}

fn decode_value<T: DeserializeOwned>(
    field: &'static str,
    value: &Value,
) -> Result<T, DecodeFailure> {
    serde_json::from_value(value.clone()).map_err(|error| DecodeFailure {
        field,
        message: error.to_string(),
    })
}

fn encode_error(error: serde_json::Error) -> TaskContextError {
    TaskContextError::Runtime(format!("checkpoint encode failed: {error}"))
}

fn run_typed<H: TypedHandler>(
    result: Result<TypedReport<H>, TaskRunError>,
) -> Result<TaskCompletion, TaskRunError> {
    let report = result?;
    let (kind, value) = match report.outcome {
        TypedOutcome::Completed(value) => (TaskOutcomeKind::Completed, encode(&value)),
        TypedOutcome::Failed(value) => (TaskOutcomeKind::Failed, encode(&value)),
        TypedOutcome::Aborted(value) => (TaskOutcomeKind::Aborted, encode(&value)),
        TypedOutcome::Indeterminate(value) => (TaskOutcomeKind::Indeterminate, Ok(value)),
    };
    let value = match value {
        Ok(value) => value,
        // The handler ran and may have caused an external effect, but its
        // result cannot be encoded. Do not fabricate a terminal outcome that
        // would discard the reconciliation opportunity; leave it recoverable.
        Err(error) => {
            return Err(TaskRunError::Interrupted(format!(
                "task result does not encode: {error}"
            )));
        }
    };
    let completion = TaskCompletion::terminal(kind, value).with_plan(report.plan);
    Ok(match report.output {
        Some(output) => completion.with_output(output),
        None => completion,
    })
}
fn encode<T: Serialize>(value: &T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}

/// Build an erased [`TaskKind`] from a typed handler. Used by
/// `TaskRegistry::register_typed`.
pub(crate) fn erase<H: TypedHandler>(handler: Arc<H>) -> Arc<dyn TaskKind> {
    Arc::new(ErasedKind { handler })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DriveOutcome, InterruptionReason, InvocationKind, Session, TaskDriver, TaskKindName,
        TaskRegistry, TaskRequest, TaskStatus,
    };
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize)]
    struct Progress {
        step: u32,
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct TinyInput {
        label: String,
    }

    struct Tiny;

    impl TypedHandler for Tiny {
        type Input = TinyInput;
        type Checkpoint = Progress;
        type Completed = String;
        type Failure = String;
        type Aborted = String;

        fn execute<'a>(
            &'a self,
            task: TypedTask<Self>,
            _context: TypedContext<Self>,
        ) -> TypedFuture<'a, Self> {
            Box::pin(async move { Ok(TypedReport::completed(task.input.label)) })
        }

        fn recover<'a>(
            &'a self,
            task: TypedTask<Self>,
            context: TypedContext<Self>,
        ) -> TypedFuture<'a, Self> {
            self.execute(task, context)
        }

        fn abort<'a>(
            &'a self,
            _task: TypedTask<Self>,
            _context: TypedAbortContext<Self>,
        ) -> TypedFuture<'a, Self> {
            Box::pin(async move { Ok(TypedReport::aborted("aborted".to_owned())) })
        }
    }

    /// A dispatched task whose durable checkpoint no longer decodes must not be
    /// terminalized: the external effect may already have happened.
    #[tokio::test]
    async fn undecodable_running_checkpoint_interrupts_instead_of_failing() {
        let mut session = Session::new().expect("session");
        let kind = TaskKindName::new("tiny").expect("kind");
        let task = session
            .create_task(TaskRequest {
                conversation_id: session.root_conversation(),
                kind: kind.clone(),
                schema_version: 1,
                input: json!({"label": "alpha"}),
                dependencies: Vec::new(),
            })
            .expect("task");
        let invocation = session
            .reserve_task_invocation(task.task_id, InvocationKind::Execute)
            .expect("reserve");
        session
            .checkpoint_task(
                task.task_id,
                invocation.generation,
                Some(json!({"step": "not-a-number"})),
                None,
            )
            .expect("checkpoint");

        let mut registry = TaskRegistry::new();
        registry
            .register_typed(kind, 1, Arc::new(Tiny))
            .expect("register");
        let driver = TaskDriver::new(session, registry);

        match driver.drive_task(task.task_id).await.expect("drive") {
            DriveOutcome::Interrupted(interruption) => {
                assert!(matches!(
                    interruption.reason,
                    InterruptionReason::HandlerFailed(_)
                ));
            }
            other => panic!("expected interruption, got {other:?}"),
        }
        let snapshot = driver.snapshot().await;
        assert!(matches!(snapshot.tasks[0].status, TaskStatus::Running));
        assert_eq!(
            snapshot.tasks[0].checkpoint,
            Some(json!({"step": "not-a-number"}))
        );
    }
}

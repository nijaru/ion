use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::store::{
    Invocation, InvocationMode, PrototypeTaskStore, StoreError, StoredTask, TaskId,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type SharedStore = Arc<Mutex<PrototypeTaskStore>>;

pub trait JsonPayload: Serialize + DeserializeOwned + Send + Sync + 'static {}
impl<T> JsonPayload for T where T: Serialize + DeserializeOwned + Send + Sync + 'static {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTask<I, C> {
    pub id: TaskId,
    pub input: I,
    pub checkpoint: Option<C>,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion<R, F> {
    Completed(R),
    Failed(F),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPlan<C, R, F> {
    pub checkpoint: Option<C>,
    pub completion: Completion<R, F>,
}

impl<C, R, F> TerminalPlan<C, R, F> {
    pub fn completed(value: R) -> Self {
        Self {
            checkpoint: None,
            completion: Completion::Completed(value),
        }
    }

    pub fn failed(value: F) -> Self {
        Self {
            checkpoint: None,
            completion: Completion::Failed(value),
        }
    }

    pub fn with_checkpoint(mut self, checkpoint: C) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortPlan<C, A> {
    pub checkpoint: Option<C>,
    pub result: A,
}

impl<C, A> AbortPlan<C, A> {
    pub fn new(result: A) -> Self {
        Self {
            checkpoint: None,
            result,
        }
    }

    pub fn with_checkpoint(mut self, checkpoint: C) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("task payload json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task implementation error: {0}")]
    Implementation(String),
    #[error("task kind {0} is not registered")]
    MissingKind(String),
    #[error("task kind mismatch: stored {stored}, adapter {adapter}")]
    KindMismatch {
        stored: String,
        adapter: &'static str,
    },
}

pub struct TaskCommit<C> {
    checkpoint: Option<C>,
}

impl<C> Default for TaskCommit<C> {
    fn default() -> Self {
        Self { checkpoint: None }
    }
}

impl<C> TaskCommit<C> {
    pub fn checkpoint(&mut self, checkpoint: C) {
        self.checkpoint = Some(checkpoint);
    }
}

#[derive(Clone)]
pub struct TaskContext<C> {
    store: SharedStore,
    invocation: Invocation,
    marker: PhantomData<fn() -> C>,
}

impl<C: JsonPayload> TaskContext<C> {
    fn new(store: SharedStore, invocation: Invocation) -> Self {
        Self {
            store,
            invocation,
            marker: PhantomData,
        }
    }

    pub const fn invocation(&self) -> Invocation {
        self.invocation
    }

    /// Prototype of the only mutation path available to a running task.
    ///
    /// The builder is process-local. The produced checkpoint is persisted only
    /// after the store revalidates task identity, invocation generation and
    /// cancellation authority.
    pub fn commit<T>(&self, build: impl FnOnce(&mut TaskCommit<C>) -> T) -> Result<T, TaskError> {
        let mut commit = TaskCommit::default();
        let result = build(&mut commit);
        if let Some(checkpoint) = commit.checkpoint {
            let value = serde_json::to_value(checkpoint)?;
            lock_store(&self.store)?.commit_checkpoint(self.invocation, &value)?;
        }
        Ok(result)
    }
}

pub trait TaskKind: Send + Sync + 'static {
    const KIND: &'static str;

    type Input: JsonPayload;
    type Checkpoint: JsonPayload;
    type Completed: JsonPayload;
    type Failure: JsonPayload;
    type Aborted: JsonPayload;

    fn execute<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    >;

    fn recover<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    >;

    fn abort<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<'a, Result<AbortPlan<Self::Checkpoint, Self::Aborted>, TaskError>>;
}

#[derive(Debug)]
struct ErasedTerminalPlan {
    checkpoint: Option<Value>,
    outcome_kind: &'static str,
    outcome: Value,
}

trait ErasedTaskKind: Send + Sync {
    fn execute<'a>(
        &'a self,
        task: StoredTask,
        store: SharedStore,
        invocation: Invocation,
    ) -> BoxFuture<'a, Result<ErasedTerminalPlan, TaskError>>;

    fn recover<'a>(
        &'a self,
        task: StoredTask,
        store: SharedStore,
        invocation: Invocation,
    ) -> BoxFuture<'a, Result<ErasedTerminalPlan, TaskError>>;

    fn abort<'a>(
        &'a self,
        task: StoredTask,
        store: SharedStore,
        invocation: Invocation,
    ) -> BoxFuture<'a, Result<ErasedTerminalPlan, TaskError>>;
}

struct TaskKindAdapter<K>(K);

impl<K: TaskKind> TaskKindAdapter<K> {
    fn typed_task(
        &self,
        task: StoredTask,
    ) -> Result<RunningTask<K::Input, K::Checkpoint>, TaskError> {
        if task.kind != K::KIND {
            return Err(TaskError::KindMismatch {
                stored: task.kind,
                adapter: K::KIND,
            });
        }
        Ok(RunningTask {
            id: task.id,
            input: serde_json::from_value(task.input)?,
            checkpoint: task.checkpoint.map(serde_json::from_value).transpose()?,
            generation: task.generation,
        })
    }

    fn erase_terminal(
        plan: TerminalPlan<K::Checkpoint, K::Completed, K::Failure>,
    ) -> Result<ErasedTerminalPlan, TaskError> {
        let checkpoint = plan.checkpoint.map(serde_json::to_value).transpose()?;
        let (outcome_kind, outcome) = match plan.completion {
            Completion::Completed(value) => ("completed", serde_json::to_value(value)?),
            Completion::Failed(value) => ("failed", serde_json::to_value(value)?),
        };
        Ok(ErasedTerminalPlan {
            checkpoint,
            outcome_kind,
            outcome,
        })
    }
}

impl<K: TaskKind> ErasedTaskKind for TaskKindAdapter<K> {
    fn execute<'a>(
        &'a self,
        task: StoredTask,
        store: SharedStore,
        invocation: Invocation,
    ) -> BoxFuture<'a, Result<ErasedTerminalPlan, TaskError>> {
        Box::pin(async move {
            let task = self.typed_task(task)?;
            let context = TaskContext::new(store, invocation);
            Self::erase_terminal(self.0.execute(task, context).await?)
        })
    }

    fn recover<'a>(
        &'a self,
        task: StoredTask,
        store: SharedStore,
        invocation: Invocation,
    ) -> BoxFuture<'a, Result<ErasedTerminalPlan, TaskError>> {
        Box::pin(async move {
            let task = self.typed_task(task)?;
            let context = TaskContext::new(store, invocation);
            Self::erase_terminal(self.0.recover(task, context).await?)
        })
    }

    fn abort<'a>(
        &'a self,
        task: StoredTask,
        store: SharedStore,
        invocation: Invocation,
    ) -> BoxFuture<'a, Result<ErasedTerminalPlan, TaskError>> {
        Box::pin(async move {
            let task = self.typed_task(task)?;
            let context = TaskContext::new(store, invocation);
            let plan = self.0.abort(task, context).await?;
            Ok(ErasedTerminalPlan {
                checkpoint: plan.checkpoint.map(serde_json::to_value).transpose()?,
                outcome_kind: "aborted",
                outcome: serde_json::to_value(plan.result)?,
            })
        })
    }
}

#[derive(Default)]
pub struct TaskRegistry {
    kinds: HashMap<&'static str, Arc<dyn ErasedTaskKind>>,
}

impl TaskRegistry {
    pub fn register<K: TaskKind>(&mut self, kind: K) {
        let previous = self.kinds.insert(K::KIND, Arc::new(TaskKindAdapter(kind)));
        assert!(previous.is_none(), "duplicate task kind {}", K::KIND);
    }

    fn get(&self, kind: &str) -> Result<Arc<dyn ErasedTaskKind>, TaskError> {
        self.kinds
            .get(kind)
            .cloned()
            .ok_or_else(|| TaskError::MissingKind(kind.to_owned()))
    }
}

pub fn shared_store(store: PrototypeTaskStore) -> SharedStore {
    Arc::new(Mutex::new(store))
}

pub fn create_task<K: TaskKind>(
    store: &SharedStore,
    input: &K::Input,
) -> Result<TaskId, TaskError> {
    let input = serde_json::to_value(input)?;
    Ok(lock_store(store)?.create_task(K::KIND, &input)?)
}

pub async fn execute_task(
    registry: &TaskRegistry,
    store: SharedStore,
    task_id: TaskId,
) -> Result<(), TaskError> {
    let invocation = lock_store(&store)?.reserve_execute(task_id)?;
    debug_assert_eq!(invocation.mode, InvocationMode::Execute);
    let task = lock_store(&store)?.task(task_id)?;
    let kind = registry.get(&task.kind)?;
    let plan = kind.execute(task, Arc::clone(&store), invocation).await?;
    lock_store(&store)?.apply_terminal(
        invocation,
        plan.checkpoint.as_ref(),
        plan.outcome_kind,
        &plan.outcome,
    )?;
    Ok(())
}

pub async fn recover_task(
    registry: &TaskRegistry,
    store: SharedStore,
    task_id: TaskId,
) -> Result<(), TaskError> {
    let invocation = lock_store(&store)?.reserve_recover(task_id)?;
    debug_assert_eq!(invocation.mode, InvocationMode::Recover);
    let task = lock_store(&store)?.task(task_id)?;
    let kind = registry.get(&task.kind)?;
    let plan = kind.recover(task, Arc::clone(&store), invocation).await?;
    lock_store(&store)?.apply_terminal(
        invocation,
        plan.checkpoint.as_ref(),
        plan.outcome_kind,
        &plan.outcome,
    )?;
    Ok(())
}

pub async fn abort_task(
    registry: &TaskRegistry,
    store: SharedStore,
    task_id: TaskId,
) -> Result<(), TaskError> {
    let invocation = lock_store(&store)?.reserve_abort(task_id)?;
    debug_assert_eq!(invocation.mode, InvocationMode::Abort);
    let task = lock_store(&store)?.task(task_id)?;
    let kind = registry.get(&task.kind)?;
    let plan = kind.abort(task, Arc::clone(&store), invocation).await?;
    lock_store(&store)?.apply_terminal(
        invocation,
        plan.checkpoint.as_ref(),
        plan.outcome_kind,
        &plan.outcome,
    )?;
    Ok(())
}

pub fn mark_cancel(store: &SharedStore, task_id: TaskId) -> Result<bool, TaskError> {
    Ok(lock_store(store)?.mark_cancel(task_id)?)
}

pub fn task(store: &SharedStore, task_id: TaskId) -> Result<StoredTask, TaskError> {
    Ok(lock_store(store)?.task(task_id)?)
}

fn lock_store(
    store: &SharedStore,
) -> Result<std::sync::MutexGuard<'_, PrototypeTaskStore>, TaskError> {
    store
        .lock()
        .map_err(|_| TaskError::Implementation("prototype store mutex poisoned".to_owned()))
}

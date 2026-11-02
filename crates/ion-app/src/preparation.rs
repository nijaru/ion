//! Retained stop-and-join custody for Host preparation outside Core's slot.
use anyhow::Result;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct Retained<T> {
    task: Option<JoinHandle<Result<T>>>,
}

/// One owned Host preparation, independent of a Session's storage registry.
/// Borrowed joins are cancellation safe. Stop is cooperative: it never aborts
/// the actual worker or proves that an in-flight native effect did not commit.
#[must_use = "retain preparation and join its outcome"]
pub(crate) struct Preparation<T> {
    stop: CancellationToken,
    retained: Arc<Mutex<Retained<T>>>,
}
impl<T: Send + 'static> Preparation<T> {
    pub(crate) fn pending(stop: CancellationToken) -> Self {
        Self {
            stop,
            retained: Arc::new(Mutex::new(Retained { task: None })),
        }
    }

    pub(crate) fn start<F, W>(stop: CancellationToken, work: F) -> Self
    where
        F: FnOnce(CancellationToken) -> W + Send + 'static,
        W: Future<Output = Result<T>> + Send + 'static,
    {
        let preparation = Self::pending(stop);
        let custody = preparation.retained.clone();
        let token = preparation.stop.clone();
        let mut state = preparation
            .retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.task = Some(tokio::spawn(async move {
            let _custody = custody;
            work(token).await
        }));
        drop(state);
        preparation
    }

    /// Retain the actual blocking task, not an async wrapper that runtime
    /// cancellation could destroy before joining its native leaf.
    pub(crate) fn start_blocking<F>(stop: CancellationToken, work: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Result<T> + Send + 'static,
    {
        let mut preparation = Self::pending(stop);
        preparation.launch_blocking(work);
        preparation
    }

    /// Install `pending` in the Controller before calling this. The short
    /// installation lock also prevents the worker from starting before its
    /// actual JoinHandle has entered retained custody.
    pub(crate) fn launch_blocking<F>(&mut self, work: F)
    where
        F: FnOnce(CancellationToken) -> Result<T> + Send + 'static,
    {
        let custody = self.retained.clone();
        let token = self.stop.clone();
        let mut state = self
            .retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.task.is_none(), "preparation launched once");
        state.task = Some(tokio::task::spawn_blocking(move || {
            drop(
                custody
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            let _custody = custody;
            work(token)
        }));
    }

    pub(crate) fn request_stop(&self) {
        self.stop.cancel();
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.stop.is_cancelled()
    }
    pub(crate) async fn join(&mut self) -> Result<T> {
        poll_fn(|cx| {
            let mut retained = self
                .retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let task = retained
                .task
                .as_mut()
                .expect("preparation completion consumed once");
            match Pin::new(task).poll(cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(result) => {
                    let completed = retained.task.take();
                    drop(retained);
                    // Neither task destruction nor arbitrary outcomes/errors
                    // may run destructors under the retained-state mutex.
                    drop(completed);
                    std::task::Poll::Ready(result.unwrap_or_else(|error| {
                        let context = format!("preparation task failed: {error}");
                        Err(anyhow::Error::new(error).context(context))
                    }))
                }
            }
        })
        .await
    }
}
impl<T> Drop for Preparation<T> {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
